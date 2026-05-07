# Issue #77 — Persist Trace Records

**Status:** Draft
**Date:** 2026-05-02
**Branch:** `feat/issue-77-trace-records`
**Brief sections:** §5.0 (turn journey), §6.1 (taxonomy), §9.3 (five-hook lifecycle), §10.0 (lifecycle)
**Issue:** [#77](https://github.com/windoliver/cairn/issues/77)
**Dependencies (closed):** #71 (CaptureEvent), #76 (session lifecycle)
**Out of scope:** #79 (hook handlers), #84 (sensor adapters), #102 (Claude Code hook map)

## 1. Goal

Persist seven trace event types — `user_message`, `agent_message`, `pre_tool`,
`post_tool`, `tool_output`, `stop`, `turn_summary` — as linked, ordered,
attribution-bearing records that participate in retention, redaction, search,
and forget. A full agent turn must be reconstructable from these records.

## 2. Non-goals

- Hook command handlers (`cairn hook PreToolUse` etc. — #79).
- Sensor adapters that translate harness payloads into `CaptureEvent`s (#84).
- LLM-synthesized turn summaries (P1+).
- P2 tree-structured sessions beyond simple parent/child turn links.
- IDL evolution of `retrieve(target=Turn|Session)` to carry an ordered
  event array and string `turn_id`. Tracked as a follow-up; see §6.2.

## 3. Approach

One MemoryKind (`MemoryKind::Trace`, already in §6.1) with a structured
`trace_event` discriminator and `trace` linkage object stored in
`MemoryRecord.extra_frontmatter`. Reuses the existing
`CaptureEvent → squash → WAL` pipeline; the only new pipeline functions are
the trace classifier and the turn-summary roll-up. No new MemoryKinds — the
19-kind taxonomy stays pinned.

`scope.session_id` mirrors `trace.session_id` so existing IDL `ScopeFilter`
queries find trace records without new predicates.

## 4. Domain (`cairn-core`)

New module `crates/cairn-core/src/domain/trace.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TraceEvent {
    UserMessage,
    AgentMessage,
    PreTool,
    PostTool,
    ToolOutput,
    Stop,
    TurnSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceLink {
    pub session_id: SessionId,
    /// Opaque harness-supplied turn id. Mirrors `CaptureRefs.turn_id`
    /// (`Option<String>` in the capture envelope) — kept as a string
    /// end-to-end so producers do not need a numeric remapping layer.
    /// The trace projector rejects events with `refs.turn_id == None`.
    pub turn_id: String,
    pub sequence: u64,                          // monotonic within turn
    pub capture_event_id: CaptureEventId,       // back-ref to raw envelope
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<CaptureEventId>, // post_tool/tool_output → pre_tool
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_event_ids: Vec<CaptureEventId>,   // turn_summary only
}
```

Serialization into `extra_frontmatter` is shown in §5.1 below; all trace
fields live at `extra_frontmatter.trace.*` (the single canonical path).

Validation invariants. **Field-level** (`TraceLink::validate`, pure):

- `member_event_ids` is empty unless `trace_event == TurnSummary`.
- `parent_event_id` is set iff `trace_event ∈ {PostTool, ToolOutput}`.
- `tool_call_id` is set iff `trace_event ∈ {PreTool, PostTool, ToolOutput}`.

**Ordering** (derived, not allocated from store state):

- `sequence` is **derived from `CaptureEvent.captured_at`** (RFC 3339
  timestamp on every envelope), not from a read-max-then-write counter.
  The projector sorts a turn's events by `(captured_at, capture_event_id)`
  and assigns `sequence = 0..N` in that order; `capture_event_id` is the
  tiebreaker so concurrent events with identical wall-clock timestamps
  still produce a deterministic, replay-stable order.
- A backfilled event whose `captured_at` precedes already-persisted
  events triggers a **two-phase renumber within the affected turn**:
  1. **Park** the turn's existing trace rows by rewriting each row's
     `extra_frontmatter.trace.sequence` to `-1 - i` for `i = 0..M-1`
     where `i` walks the rows in their pre-existing order. Distinct
     negatives, so the unique index `records_trace_seq` (which keys on
     `(session_id, turn_id, sequence)`) holds at every statement.
  2. **Reassign** sequences over the union (existing ∪ new) sorted by
     `(captured_at, capture_event_id)`; write the final positive `0..N`
     values to every row.
  Both phases run inside the same `BEGIN IMMEDIATE` transaction; the
  intermediate (negative) state is invisible to other connections, and
  a crash rolls back to pre-renumber. SQLite checks uniqueness at the
  end of each statement on a non-deferred index; the parked sentinels
  guarantee no transient collision because every value is unique within
  the turn at every step. No row is dropped or reinserted.
- A backfill into a closed turn (one whose `turn_summary` row already
  exists) is admitted; the transaction additionally recomputes and
  upserts the summary so `member_event_ids` reflects current turn state
  — see referential invariant below.

**Referential** (enforced inside `transaction(...)` before commit;
violations roll back the entire turn):

- For every `PostTool` / `ToolOutput` record, `parent_event_id` MUST
  resolve to an existing `PreTool` record in the same
  `(session_id, turn_id)` whose `tool_call_id` matches. Cross-turn,
  cross-session, or cross-tool-call links are rejected with a typed
  `TraceLinkOrphan` error.
- `member_event_ids` on a `TurnSummary` MUST be exactly the set of
  non-summary trace records persisted under the same
  `(session_id, turn_id)`. Drift surfaces as `TraceSummaryMembership`.
- **Closed-turn writes recompute the summary.** A `turn_summary` is a
  *snapshot of current turn state*, not a write-once finalization. When
  a late event lands for a turn whose summary already exists, the
  transaction (a) admits the new event, (b) renumbers sequences over
  the union, and (c) **upserts the summary** with a fresh
  `member_event_ids` reflecting the new state. The summary's record id
  is deterministic on `(session_id, turn_id)` (§5.2), so the upsert
  targets the same row — no duplicate summaries. Idempotent replay
  still produces no-op summary rewrites because the inputs are
  unchanged. Result: late `tool_output` / `post_tool` / `stop` events
  reconcile cleanly without operator intervention. The
  `member_event_ids` invariant continues to hold because it is
  *recomputed*, never *retained from a prior commit*.
- `sequence` strictly monotonic within `(session_id, turn_id)` —
  enforced at the store layer (UNIQUE index), surfaced as
  `TraceSequenceConflict`.

The referential checks run inside the transaction so a malformed parent
reference cannot land partially. Field-level checks run earlier in the CLI
verb's pre-write validation pass (see §7) so invalid input fails fast
without acquiring write locks.

## 5. Pipeline (`cairn-core/src/pipeline/`)

Two pure modules.

### 5.1 `pipeline/capture_trace.rs`

```rust
pub fn project(
    event: &CaptureEvent,
    classified: TraceEvent,
    resolved_body: &ResolvedBody,    // hash-verified text from sources/
    link: TraceLink,
) -> Result<MemoryRecord, TraceProjectError>
```

`ResolvedBody` is the existing extractor-pipeline type
(`crates/cairn-core/src/pipeline/extract/body.rs`) that pairs raw text with
its `payload_hash` after verification. Same construction as the extractor
path: callers (the CLI verb) resolve the body from `sources/` referenced by
`event.payload_ref`, verify `sha256(bytes) == event.payload_hash`, then
pass the verified body in. The projector itself stays pure — it does no
I/O, never opens a file, and never receives raw bytes — so the privacy
boundary stays the same as for ingest.

The privacy filter (Presidio pre-persist) runs on the verified text *before*
construction so the body stored on the record is the redacted form;
`payload_hash` continues to bind the record to the un-redacted source for
audit.

Builds a `MemoryRecord` with:

- `kind = MemoryKind::Trace`
- `class = MemoryClass::Episodic`
- `visibility = MemoryVisibility::Private`
- `scope = ScopeTuple { session_id: Some(link.session_id), .. }`
- `body` = privacy-filtered text per event type (see §5.3)
- `extra_frontmatter` = `{trace_event, trace}` only. `trace`
  itself contains `payload_hash` (and `payload_ref`) carried through
  from the envelope. There is **no top-level `payload_hash` key**: all
  trace metadata lives at `extra_frontmatter.trace.*`. This single
  canonical path is the one indexes, generated columns, and forget SQL
  query against. Concretely the YAML shape is:

```yaml
trace_event: pre_tool
trace:
  session_id: 01ARZ3...
  turn_id: "turn-4"
  sequence: 2
  capture_event_id: 01HQ...
  payload_hash: "sha256:..."     # from CaptureEvent.payload_hash
  payload_ref: "sources/..."     # from CaptureEvent.payload_ref
  tool_call_id: call_abc
```
- `actor_chain` cloned from the `CaptureEvent` — preserves sensor/author
  attribution
- `provenance` filled from `CaptureEvent` metadata (sensor, mode, captured_at)

### 5.2 `pipeline/turn.rs`

```rust
pub fn summarize_turn(
    session_id: &SessionId,
    turn_id: &str,                      // opaque, matches TraceLink.turn_id
    events: &[MemoryRecord],
) -> Result<MemoryRecord, TurnSummaryError>
```

The summary record uses a **deterministic, ULID-shaped record id** derived
from `(session_id, turn_id)`. The current `RecordId` newtype validates
26-char Crockford-base32 ULIDs, so the id must satisfy that lexer:

```rust
fn summary_record_id(session_id: &SessionId, turn_id: &str) -> RecordId {
    // SHA-256 over the canonical pair, first 80 bits → ULID timestamp,
    // next 80 bits → ULID randomness.
    let mut h = Sha256::new();
    h.update(b"cairn:trace:turnsum\0");
    h.update(session_id.as_str().as_bytes());
    h.update(b"\0");
    h.update(turn_id.as_bytes());
    let digest = h.finalize();
    let ulid = Ulid::from_bytes(digest[..16].try_into().unwrap());
    RecordId::parse(ulid.to_string()).expect("ULID-shaped by construction")
}
```

The result is a stable ULID — same input always maps to the same id — but
also a *legal* `RecordId` that the rest of the store, IDL serialization,
and sort logic accept without special-casing. The unique
`records_trace_summary` index in §6.1 is the second line of defense if a
deterministic id ever collides with an existing record id (probability
~2⁻¹²⁸; treated as unreachable but the index still rejects it).

Concatenates ordered events into a single `TurnSummary` record. No LLM —
deterministic format:

```
## Turn N (session <id>)
- [seq 0] user_message: <body excerpt>
- [seq 1] agent_message: <body excerpt>
- [seq 2] pre_tool: <tool>(<arg digest>)
- [seq 3] post_tool: <tool> ok=true
- [seq 4] tool_output: <output excerpt>
```

Member ids stored in `TraceLink.member_event_ids`. Pre-conditions: `events`
all have matching `(session_id, turn_id)`, `sequence` strictly increasing,
and at least one user or agent message. Otherwise typed error.

### 5.3 Body content per event type

Each body is the **already privacy-filtered** text. Filtering reuses the
existing `filter` pipeline stage (Presidio pre-persist). The raw bytes live
under `sources/` per the existing capture flow; record bodies hold redacted
projections.

| Event           | Body                                                 |
| --------------- | ---------------------------------------------------- |
| `user_message`  | Filtered user prompt text                            |
| `agent_message` | Filtered assistant message text + reasoning excerpt  |
| `pre_tool`      | `<tool_name>(<arg digest>)` — args summarized        |
| `post_tool`     | `<tool_name> ok=<bool> duration=<ms>`                |
| `tool_output`   | Filtered output excerpt (capped at N bytes)          |
| `stop`          | One line: stop reason + total turn duration          |
| `turn_summary`  | Roll-up text from §5.2                               |

Bodies cap at 4 KiB each; full content stays in `sources/` referenced by
`payload_ref` on the originating `CaptureEvent`.

## 6. Store (`cairn-store-sqlite`)

### 6.1 Migration `0022_trace_links.sql`

(Number follows the existing migration sequence — `0021` is the most
recently committed migration.)

Generated columns over the existing records table extracting trace fields
from `extra_frontmatter` JSON (SQLite supports `GENERATED ALWAYS AS ... VIRTUAL`
with `json_extract`):

```sql
ALTER TABLE records ADD COLUMN trace_event TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace_event')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_session_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.session_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_turn_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.turn_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_sequence INTEGER
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.sequence')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_parent_event_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.parent_event_id')) VIRTUAL;

ALTER TABLE records ADD COLUMN trace_capture_event_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.capture_event_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_payload_hash TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.payload_hash')) VIRTUAL;

-- Forget refcounting reads this column to decide whether a sources/
-- blob can be deleted. Index keeps the COUNT(*) cheap.
CREATE INDEX records_trace_payload_hash
  ON records(trace_payload_hash)
  WHERE trace_payload_hash IS NOT NULL;

-- Idempotency for raw events: same CaptureEvent may not produce two rows.
CREATE UNIQUE INDEX records_trace_event_id
  ON records(trace_capture_event_id)
  WHERE trace_capture_event_id IS NOT NULL;

-- Idempotency for the synthetic turn_summary: exactly one summary per
-- (session_id, turn_id). The summary's record id is also derived
-- deterministically from the same pair (§5.2) so upserts are stable on
-- both the primary key and this index.
CREATE UNIQUE INDEX records_trace_summary
  ON records(trace_session_id, trace_turn_id)
  WHERE trace_event = 'turn_summary';

-- Sequence monotonicity within a turn (excluding turn_summary).
CREATE UNIQUE INDEX records_trace_seq
  ON records(trace_session_id, trace_turn_id, trace_sequence)
  WHERE trace_event IS NOT NULL AND trace_event != 'turn_summary';

CREATE INDEX records_trace_parent
  ON records(trace_parent_event_id)
  WHERE trace_parent_event_id IS NOT NULL;
```

The unique indices enforce three invariants at the database boundary:

- **Idempotent event replay** — `capture_event_id` is the stable event
  identity. A second `capture_trace --from <same file>` is absorbed as
  no-op upserts on existing rows; no fresh sequences allocated.
- **Idempotent summary replay** — at most one `turn_summary` per
  `(session_id, turn_id)`. Combined with the deterministic record id
  derived in §5.2, replays converge on the same row.
- **Sequence monotonicity** — within a `(session_id, turn_id)` no two
  non-summary events share a `sequence`. Surfaced as a typed
  `TraceSequenceConflict`.

### 6.2 `MemoryStore` contract additions

**Established pattern: transactional work uses `SqliteMemoryStore::with_tx`
(inherent), not a trait method.** `MemoryStore` is `dyn`-compatible by
design — generic-method `transaction(...)` would break that. The existing
verb layer (consolidate, promote) reaches into the concrete
`SqliteMemoryStore` for atomic multi-statement writes; trace persistence
follows the same pattern.

Additions to the SQLite store (none touch the dyn trait):

```rust
impl StoreTx<'_> {
    pub fn upsert_trace(&mut self, record: &MemoryRecord)
        -> Result<UpsertOutcome, StoreError>;
    pub fn list_trace_events(
        &self, session_id: &SessionId, turn_id: &str,
    ) -> Result<Vec<MemoryRecord>, StoreError>;
    pub fn turn_summary_exists(
        &self, session_id: &SessionId, turn_id: &str,
    ) -> Result<bool, StoreError>;
}
```

`upsert_trace` reuses the existing `upsert_in_tx` plumbing — idempotency
flows from the new `records_trace_event_id` and `records_trace_summary`
unique indices (§6.1) plus the deterministic summary record id (§5.2),
not from new logic in the upsert helper.

The CLI verb takes `&SqliteMemoryStore` directly (not `&dyn MemoryStore`),
matching how `consolidate` and `promote` already work. The pure pipeline
functions in `cairn-core` (`project`, `summarize_turn`,
`order_by_captured_at`, `assign_sequences`) stay dyn-store-free and
adapter-free, so unit tests need no SQLite. Integration tests use a real
in-memory `SqliteMemoryStore` via `cairn-test-fixtures`.

WAL §5.6 guarantees still apply — `with_tx` runs inside the same
`tokio_rusqlite` worker thread that the WAL state machine uses; the
trace path is a verb-level wrapper *around* it, not a replacement.

### 6.3 Reconstruction surface and `retrieve` IDL gap

Issue #77's acceptance reads **"A full agent turn can be reconstructed from
trace records and links"** and its verification step is **"Run turn
reconstruction tests"** — the linkage and reconstruction *capability* is
what's gated, not a specific public surface.

Even so, the current public `retrieve` IDL cannot today expose a
reconstructed turn:

- `DataTurn.turn_id` is `integer`; capture envelopes carry strings.
- `DataTurn.turn` is a single `TurnItem` — one role, no event ordering —
  so a multi-message, multi-tool turn cannot round-trip through it.

Two ways to close this gap. Both are explicit choices, not silent deferral:

**Choice X (default, recommended): rescope the public read surface to a
follow-up.** This PR adds a store-internal
`MemoryStore::list_trace_events(session_id, turn_id) -> Vec<MemoryRecord>`
ordered by `sequence`. Reconstruction tests run against that method,
satisfying #77's acceptance criterion. A follow-up issue is filed to evolve
`retrieve(target=Turn|Session)` to carry an ordered event array and string
`turn_id`; #77's PR links to it and notes that first-party CLI/MCP/SDK
clients cannot read trace records back through `retrieve` until that
follow-up lands.

**Choice Y: bundle the IDL evolution into this PR.** Mutates the public
contract — IDL schema change, regenerate via `cairn-codegen`, update
`retrieve` verb in store, regen docs via `cairn-docgen --write`. Larger
diff; the change must run alongside any consumers that already deserialize
`DataTurn`.

Default is **X** because the issue scope (and the user's earlier "A.
persistence only") explicitly excluded surface-level work. Choosing Y is a
re-scoping conversation, not a review-loop fix.

The unique sequence + capture_event_id indices from §6.1 ship in either
choice — they protect the linkage regardless of which read surface is
exposed first.

## 7. CLI verb (`cairn-cli/src/verbs/capture_trace.rs`)

The published IDL (`crates/cairn-idl/schema/verbs/capture_trace.json`) is:

```json
{ "from": "<path>", "session_id": "<optional>" }
```

`from` is a path to a **trace log file** containing one JSON-encoded
`CaptureEvent` per line (JSONL). The verb does not accept an inline event
batch and there is no `is_stop` flag — the stop boundary is recovered from
the events themselves (an event whose `hook_name == "Stop"`).

Pseudocode:

```rust
async fn run(args: CaptureTraceArgs, store: &dyn MemoryStore) -> Result<...> {
    refuse_if_degraded(...)?;
    let events = read_jsonl::<CaptureEvent>(&args.from).await?;  // streaming, bounded buffer

    // Group by (session_id, turn_id). Each turn group is validated and
    // committed independently — a malformed event in turn 2 does NOT
    // abort already-validated, committed turn 1. Reasons:
    //   - large trace batches stay resilient to one bad event;
    //   - replay with a fixed file finishes the missing turn idempotently;
    //   - cross-turn coupling is undesirable (the brief treats turns as
    //     independent units of memory).
    for (session_id, turn_id, raw_group) in group_by_turn(&events) {
        // Per-turn validation. Failure leaves earlier turns intact and
        // surfaces in the response's `failed_turns` list (see below);
        // it does not poison later turns either.
        let projected: Result<Vec<_>, _> = raw_group.iter()
            .map(|event| {
                event.validate()?;
                Ok((event, classify(event)?))
            })
            .collect();
        let projected = match projected {
            Ok(p) => p,
            Err(e) => { failed_turns.push((session_id, turn_id, e)); continue; }
        };
        let group = projected;
        let result = store.transaction(|tx| async move {
            // Sequences come from captured_at ordering across the union of
            // (already-persisted events ∪ this batch). Concurrent writes
            // serialize at BEGIN IMMEDIATE, so the read+rewrite is
            // race-free within a single transaction.
            let existing = tx.list_trace_events(&session_id, &turn_id).await?;
            let ordered = order_by_captured_at(existing, &group)?;  // pure
            let renumbered = assign_sequences(&ordered);            // pure

            for entry in &renumbered {
                let resolved = resolve_body(tx, entry.event).await?; // hash-verified
                let record = pipeline::capture_trace::project(
                    entry.event, entry.classified, &resolved, entry.link.clone(),
                )?;
                tx.upsert_trace(record).await?;  // updates seq on existing rows; inserts new
            }

            // The turn is "closed" if a Stop event is in the persisted set
            // OR a turn_summary already exists. In either case, recompute
            // the summary from current state — late events reconcile by
            // upserting the deterministic-id summary row.
            let already_summarized =
                tx.turn_summary_exists(&session_id, &turn_id).await?;
            if turn_is_closed(&renumbered) || already_summarized {
                let turn_records = tx.list_trace_events(&session_id, &turn_id).await?;
                let summary = pipeline::turn::summarize_turn(
                    &session_id, &turn_id, &turn_records,
                )?;
                tx.upsert_trace(summary).await?;
            }
            Ok::<_, TraceError>(())
        }).await;
        if let Err(e) = result {
            // Transaction rolled back — entire turn is unwritten.
            // Continue to the next turn; partial earlier turns stay
            // committed.
            failed_turns.push((session_id, turn_id, e));
        }
    }
    Ok(success_response(trace_id, failed_turns))
}
```

Classification rule: `(CaptureEvent.refs.hook_name, payload tag) → TraceEvent`.
Static table — no LLM. `hook_name` lives in the existing `CapturePayload`
hook variant; payload tag covers non-hook surfaces (CLI/MCP message capture).

**No IDL changes.** The verb shape and CLI flags stay byte-identical to the
current schema; the docgen output should be unchanged. If a flag does need
to change later (e.g., to surface `trace_id` in human output), that's a
separate scoped change with `cairn-docgen --write` re-run.

## 8. Privacy, forget, search — no new code paths

| Concern   | Mechanism                                                              |
| --------- | ---------------------------------------------------------------------- |
| Privacy   | Bodies pass through existing `filter` (Presidio pre-persist). Raw bytes live in `sources/` referenced by `payload_hash`; deleted on `forget` (see below). |
| Search    | FTS5 already gates by visibility. Trace records default to `private`; `search` with the right scope predicate finds them. |
| Retention | `ExpirationWorkflow` already operates on `MemoryRecord` — trace records inherit the same decay/salience rules. |

### 8.1 Forget for trace records — sources deletion is part of #77

The brief's existing forget semantics for `MemoryRecord` (zero bodies + zero
embeddings + retain `payload_hash` for audit) is insufficient for trace
records: the *most sensitive* trace content — raw prompts, tool I/O,
reasoning — lives in `sources/<payload_hash>` referenced by every trace
record's envelope. Leaving those bytes on disk after `forget --session`
defeats the privacy story.

**One canonical identity for source blobs: `payload_hash`. Refcounting is
scoped to the forgetting principal, never global.** The existing
`CaptureEvent` carries both `payload_hash` (sha256 of bytes) and
`payload_ref` (vault-relative path under `sources/`). Deletion keys on
`payload_hash` so duplicate `payload_ref` paths cannot leave bytes
behind. Refcounting keys on the **same isolation boundary as the
forgetting record** so the operation never reveals whether bytes are
shared with another principal.

Concretely:

1. `forget --record <id>` and `forget --session <id>` collect, from each
   targeted trace record's envelope:
   - `payload_hash` (used for the in-scope refcount query),
   - **the concrete `payload_ref`** (the actual on-disk path under
     `sources/`, used for deletion),
   - the record's scope dimensions (`scope.tenant`, `scope.user`,
     `scope.agent`).
2. For each forgotten record:
   - Query the records table for any *other* live record under the
     **same** `(tenant, user, agent)` whose `trace_payload_hash` matches
     this record's hash. If the count is zero in scope, every
     `payload_ref` collected in step 1 for this hash is **deleted from
     disk** by its concrete path. This guarantees every on-disk file the
     forgotten records actually referenced is removed, even if the
     storage layout is not yet canonicalized to a single hash-derived
     path.
   - If the count is non-zero (the principal has another live record
     referencing the same blob — same prompt captured twice in their
     own session), the file is retained.
   - **Cross-scope references are never inspected.** If a record under
     a different `(tenant, user, agent)` happens to share the
     `payload_hash`, this principal's forget proceeds as if no other
     reference exists. The shared blob remains on disk for the other
     principal's record (which lives in its own `sources/` namespace if
     scoped storage is in use, or in a shared blob whose deletion is
     governed by *that* principal's lifecycle, not this one).
   - Implementation note: P0 stores `sources/` per-principal already
     (brief §3 isolation rule). When P1+ shared content addressing
     ships, blob deletion will move to a refcount maintained at the
     blob layer with no per-principal disclosure. #77 does not
     introduce that shared layer; it stays inside the per-principal
     boundary.
3. The consent journal records, per forgotten record:
   `{record_id, payload_hash, sources_action: "deleted" | "retained-self"}`.
   `retained-self` means the same principal still has a live record
   referencing the blob. There is no `retained-shared` outcome — the
   journal never reveals cross-principal blob coincidence.
4. `payload_hash` itself stays on the (now redacted) record as the
   audit anchor — the hash alone is not user-recoverable PII; the bytes
   it pointed to are gone for this principal.

Refcount SQL: `SELECT COUNT(*) FROM records WHERE trace_payload_hash = ?
AND scope_tenant IS ? AND scope_user IS ? AND scope_agent IS ? AND id
NOT IN (<forgetting set>)`. The `trace_payload_hash` index from §6.1
keeps it O(log n). The scope columns are existing generated columns on
the records table (added by prior migrations, not new in this PR).

### 8.2 Other concerns

Tests confirm the search and retention paths work without code changes — if
adjustment is needed, that's a finding, not planned work.

## 9. Testing

### 9.1 Unit (`cairn-core`)

- `pipeline::capture_trace::project` — table-driven via `rstest`, one case per
  `TraceEvent` variant. Asserts kind, class, visibility, scope, body shape,
  actor_chain, extra_frontmatter.
- `pipeline::turn::summarize_turn` — ordering, member-id capture, error on
  cross-turn events, error on missing user/agent message.
- `domain::trace::TraceLink::validate` — every invariant in §4 has both a
  passing and a failing case.

### 9.2 Property (`proptest`)

- Round-trip: generate N ordered trace events for a turn → project →
  `summarize_turn` → reconstruction equals input ordering and parent/child
  edges.

### 9.3 Integration (`cairn-store-sqlite/tests/`)

- End-to-end `capture_trace --from <fixture.jsonl>` → store →
  `MemoryStore::list_trace_events(session, turn)` returns records in
  `sequence` order with parent/child edges intact. Asserts full-turn
  reconstruction directly at the store boundary (the public retrieve verb
  does not yet expose this shape — see §6.2).
- **Idempotent replay**: running `capture_trace --from` twice on the same
  file produces identical store state — no duplicate rows, no new
  sequences, exactly one `turn_summary` per closed turn. Asserts row
  counts and `capture_event_id` uniqueness.
- **Multi-turn input**: a JSONL containing two complete turns yields two
  `turn_summary` records, each with `member_event_ids` pointing only at
  its own turn's events.
- **Atomicity per turn**: a JSONL whose second turn contains a malformed
  event leaves the *first* turn fully persisted (events + summary) and
  the *second* turn unwritten — no partial prefix. Re-running with the
  fixed file completes the second turn idempotently.
- **Out-of-order backfill (open turn)**: import events `[A@t=2, B@t=3]`
  for an open turn, then a follow-up batch `[C@t=1]`. Final
  `list_trace_events` ordering is `[C, A, B]` (sequence renumbered by
  `captured_at`). No row is dropped; `capture_event_id` is preserved.
- **Backfill on closed turn resummarizes**: a turn already has events +
  summary; importing one more `tool_output` for that turn updates
  sequences and rewrites the summary row in place. The summary's
  `member_event_ids` now includes the new event; the row id is
  unchanged. No duplicate summary, no error.
- **Ties on `captured_at`**: two events with identical timestamps
  produce a stable order keyed on `capture_event_id`; the same input
  always yields the same sequence assignment.
- **Orphan parent rejected**: a `post_tool` whose `parent_event_id`
  points at a non-existent or cross-turn `pre_tool` returns
  `TraceLinkOrphan`; the entire turn rolls back.
- **Tool-call-id mismatch rejected**: a `tool_output` whose
  `tool_call_id` differs from its parent's returns `TraceLinkOrphan`.
- **Forget deletes by concrete payload_ref, scoped refcount by hash**:
  `forget --session <id>` removes every `payload_ref` path persisted
  on the forgotten records, gated by an in-scope hash refcount that
  decides "no other live record references this hash for the same
  principal." Asserts file removal at the actual paths even when the
  storage layout uses non-canonical `payload_ref` values. Consent
  journal records `{deleted, retained-self}` only — no cross-principal
  leak.
- **Forget privacy boundary**: a record under principal A and another
  under principal B share the same `payload_hash`. Forgetting A's
  record proceeds as if B's reference does not exist; the consent
  journal never mentions B and never produces a `retained-shared`
  entry. (Whether A's bytes are physically deleted depends on the P0
  per-principal layout — assert the journal entry, not the file path.)
- Duplicate-`sequence` write returns `TraceSequenceConflict`, store state
  unchanged.
- `forget --session <id>` zeros bodies of all trace records, leaves
  `payload_hash` and the consent journal entry intact.
- Privacy: a trace event whose payload contains a synthetic PII token is
  redacted in the body but the `payload_hash` matches the un-redacted source.

### 9.4 CLI snapshot

- `cairn capture_trace --json` for a fixture turn produces a stable
  `insta`-snapshot of the response envelope.

## 10. File map

```
crates/cairn-core/src/domain/trace.rs                # NEW
crates/cairn-core/src/domain/mod.rs                  # +pub mod trace
crates/cairn-core/src/pipeline/capture_trace.rs      # NEW
crates/cairn-core/src/pipeline/turn.rs               # NEW
crates/cairn-core/src/pipeline/mod.rs                # +pub mod
crates/cairn-store-sqlite/src/migrations/sql/0022_trace_links.sql  # NEW
crates/cairn-store-sqlite/src/store/tx.rs            # +upsert_trace, +list_trace_events, +turn_summary_exists on StoreTx
crates/cairn-store-sqlite/migrations/0022_trace_links.sql  # new generated columns + indices (number is next-after-0021)
crates/cairn-cli/src/verbs/capture_trace.rs          # replace stub (uses existing IDL `from` flag)
crates/cairn-test-fixtures/src/...                   # trace fixtures (JSONL inputs)
```

## 11. Risks

- **Generated-column migration cost.** SQLite virtual generated columns are
  free at write time but rebuild on read. If retrieve perf suffers, fall back
  to a denormalized `trace_links` side table populated via trigger. Defer
  until benchmarks show a problem.
- **Sequence assignment under crash.** `build_link` reads max sequence
  then writes — a crash between commits could leave a gap. Acceptable
  (gaps are not corruption); the unique index prevents duplicates and
  the `capture_event_id` index keeps replay idempotent.
- **Body cap of 4 KiB.** Could truncate useful context for long tool outputs.
  Mitigated by `payload_ref` pointing at full bytes in `sources/`. Tunable
  via config; default conservative.
- **Public read surface for trace records is a known gap.** Adversarial
  review surfaced this in three consecutive rounds. The decision (Choice X
  in §6.3) is to ship persistence first and evolve `retrieve(target=Turn)`
  in a follow-up. Risk: first-party CLI/MCP/SDK clients see records they
  cannot read back through the supported public verb until the follow-up
  lands. Accepted because the user explicitly scoped #77 to persistence-only
  and the issue's reconstruction acceptance is satisfied at the store
  boundary by `list_trace_events`.

## 12. Verification checklist

Standard CLAUDE.md §8 checklist plus:

- `cargo run -p cairn-idl --bin cairn-codegen -- --check` (no IDL changes
  expected, but verify).
- `cargo run -p cairn-cli --bin cairn-docgen -- --write` if any CLI flag
  changes; otherwise skip.
