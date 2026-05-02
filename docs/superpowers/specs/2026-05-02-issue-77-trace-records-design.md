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
    pub turn_id: u64,
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

Serialization into `extra_frontmatter`:

```yaml
trace_event: pre_tool
trace:
  session_id: 01ARZ3...
  turn_id: 4
  sequence: 2
  capture_event_id: 01HQ...
  tool_call_id: call_abc
```

Validation invariants (all in `TraceLink::validate`):

- `member_event_ids` is empty unless `trace_event == TurnSummary`.
- `parent_event_id` is set iff `trace_event ∈ {PostTool, ToolOutput}`.
- `tool_call_id` is set iff `trace_event ∈ {PreTool, PostTool, ToolOutput}`.
- `sequence` strictly monotonic within `(session_id, turn_id)` — enforced at
  the store layer (UNIQUE index), surfaced as a typed error.

## 5. Pipeline (`cairn-core/src/pipeline/`)

Two pure modules.

### 5.1 `pipeline/capture_trace.rs`

```rust
pub fn project(
    event: &CaptureEvent,
    classified: TraceEvent,
    link: TraceLink,
) -> Result<MemoryRecord, TraceProjectError>
```

Builds a `MemoryRecord` with:

- `kind = MemoryKind::Trace`
- `class = MemoryClass::Episodic`
- `visibility = MemoryVisibility::Private`
- `scope = ScopeTuple { session_id: Some(link.session_id), .. }`
- `body` = privacy-filtered text per event type (see §5.3)
- `extra_frontmatter` = `{trace_event, trace}` plus carry-through of
  `payload_hash` and `capture_event_id` from the envelope
- `actor_chain` cloned from the `CaptureEvent` — preserves sensor/author
  attribution
- `provenance` filled from `CaptureEvent` metadata (sensor, mode, captured_at)

### 5.2 `pipeline/turn.rs`

```rust
pub fn summarize_turn(
    session_id: &SessionId,
    turn_id: u64,
    events: &[MemoryRecord],
) -> Result<MemoryRecord, TurnSummaryError>
```

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

### 6.1 Migration `0003_trace_links.sql`

Generated columns over the existing records table extracting trace fields
from `extra_frontmatter` JSON (SQLite supports `GENERATED ALWAYS AS ... VIRTUAL`
with `json_extract`):

```sql
ALTER TABLE records ADD COLUMN trace_event TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace_event')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_session_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.session_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_turn_id INTEGER
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.turn_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_sequence INTEGER
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.sequence')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_parent_event_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.parent_event_id')) VIRTUAL;

CREATE UNIQUE INDEX records_trace_seq
  ON records(trace_session_id, trace_turn_id, trace_sequence)
  WHERE trace_event IS NOT NULL AND trace_event != 'turn_summary';

CREATE INDEX records_trace_parent
  ON records(trace_parent_event_id)
  WHERE trace_parent_event_id IS NOT NULL;
```

The unique index enforces sequence monotonicity at the database boundary,
turning a duplicate `sequence` into a typed `TraceSequenceConflict` error
instead of silent corruption.

### 6.2 Retrieve targets

`retrieve(target=Turn)` and `retrieve(target=Session)` already exist in the
IDL with `DataTurn` / `TurnItem` wire shape. Implement them in the SQLite
store:

- `Turn`: `SELECT ... WHERE trace_session_id=? AND trace_turn_id=? ORDER BY trace_sequence`,
  fold into `TurnItem` per role mapping (user_message → User,
  agent_message → Assistant, pre_tool/post_tool/tool_output → Tool with
  `tool_calls` array).
- `Session`: paged across turns using the cursor field already in the IDL.

## 7. CLI verb (`cairn-cli/src/verbs/capture_trace.rs`)

Replace the current stub. Pseudocode:

```rust
async fn run(args: CaptureTraceArgs, store: &dyn MemoryStore) -> Result<...> {
    refuse_if_degraded(...)?;
    for event in args.events {
        let classified = classify(&event)?;          // hook_name + payload tag
        let link = next_link_for(&store, &event).await?;
        let record = pipeline::capture_trace::project(&event, classified, link)?;
        store.write(record).await?;                  // WAL two-phase, §5.6
    }
    if args.is_stop {
        let turn = store.list_turn(session, turn_id).await?;
        let summary = pipeline::turn::summarize_turn(...)?;
        store.write(summary).await?;
    }
    Ok(success_response(...))
}
```

Classification rule: `(CaptureEvent.refs.hook_name, payload tag) → TraceEvent`.
Static table — no LLM.

## 8. Privacy, forget, search — no new code paths

| Concern   | Mechanism                                                              |
| --------- | ---------------------------------------------------------------------- |
| Privacy   | Bodies pass through existing `filter` (Presidio pre-persist). Raw bytes stay in `sources/` referenced by `payload_hash`. |
| Forget    | `forget --record` / `forget --session` operates on `MemoryRecord` regardless of kind — trace records get embeddings zeroed, bodies dropped, `payload_hash` retained for audit. |
| Search    | FTS5 already gates by visibility. Trace records default to `private`; `search` with the right scope predicate finds them. |
| Retention | `ExpirationWorkflow` already operates on `MemoryRecord` — trace records inherit the same decay/salience rules. |

Tests confirm these paths work — no implementation changes expected here. If
a path *does* need adjustment, that's a finding, not planned work.

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

- End-to-end `capture_trace` → store → `retrieve(target=Turn)` returns the
  expected `DataTurn` with `tool_calls` populated, ordering preserved.
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
crates/cairn-store-sqlite/migrations/0003_trace_links.sql  # NEW
crates/cairn-store-sqlite/src/...                    # retrieve(Turn|Session) impl
crates/cairn-cli/src/verbs/capture_trace.rs          # replace stub
crates/cairn-test-fixtures/src/...                   # trace fixtures
```

## 11. Risks

- **Generated-column migration cost.** SQLite virtual generated columns are
  free at write time but rebuild on read. If retrieve perf suffers, fall back
  to a denormalized `trace_links` side table populated via trigger. Defer
  until benchmarks show a problem.
- **Sequence assignment under crash.** `next_link_for` reads max sequence
  then writes — a crash between commits could leave a gap. Acceptable
  (gaps are not corruption); the unique index prevents duplicates.
- **Body cap of 4 KiB.** Could truncate useful context for long tool outputs.
  Mitigated by `payload_ref` pointing at full bytes in `sources/`. Tunable
  via config; default conservative.

## 12. Verification checklist

Standard CLAUDE.md §8 checklist plus:

- `cargo run -p cairn-idl --bin cairn-codegen -- --check` (no IDL changes
  expected, but verify).
- `cargo run -p cairn-cli --bin cairn-docgen -- --write` if any CLI flag
  changes; otherwise skip.
