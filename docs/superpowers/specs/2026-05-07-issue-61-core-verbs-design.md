# Design — issue #61 core verb implementation

**Status:** Approved (brainstorm 2026-05-07).  
**Issue:** [#61](https://github.com/windoliver/cairn/issues/61).  
**Parent epic:** [#9](https://github.com/windoliver/cairn/issues/9).  
**Brief refs:** §5 write/read pipeline, §5.5 FlushPlan, §5.6 WAL, §7 hot memory, §8.0 core verbs, §8.0.b common envelope, §8.0.c retrieve variants, §14 privacy/consent.

## 1. Goal

Implement the remaining P0 behavior for `ingest`, `capture_trace`, `retrieve`,
`summarize`, and `assemble_hot` in one PR, based on `origin/main`.

The PR must close issue #61 as a whole:

- `ingest` and `capture_trace` run through capture/extract/filter/classify/scope,
  FlushPlan/WAL admission, store apply, policy trace, and signed-envelope
  validation.
- `retrieve` supports record, session, turn, folder, scope, and profile variants.
- `summarize` produces deterministic P0 rollups and persists them only through
  the same verified write path when requested.
- `assemble_hot` loads real configured hot-memory inputs, preserves #288
  segment markers, and enforces the configured byte budget before returning.

## 2. Non-goals

- P1 reflection, DreamWorkflow regeneration, cold rehydration, Nexus sidecars,
  or remote LLM provider behavior.
- Session-tree-aware retrieval or hot-memory assembly.
- Named recipe presets from #293.
- New policy gates. This PR exposes and composes existing gates; it does not
  invent new authorization semantics.
- New public verb names. The eight `cairn.mcp.v1` verbs stay fixed.

## 3. Current base

`origin/main` already contains the dependencies #61 needs:

- CLI command tree and common response envelope from #59.
- `EnvelopeVerifier`, `ScopePolicy`, `resolve_issuer`, and wire error mapping
  from #51.
- Replay/handshake scaffolding from #52.
- `FlushPlan` and WAL FSM/recovery scaffold from #54/#55.
- `MemoryStore` CRUD/search/trace/session surfaces and `SqliteMemoryStore`.
- Filter pure functions: `redact`, `fence`, `should_memorize`,
  `default_visibility`, `BlockedAuditEntry`.
- Policy trace producer types and `to_wire`.
- Trace-record persistence helpers from #77.
- Segment-aware but stub-body `assemble_hot` from #288.

The implementation should not restart any of that work. It should connect the
existing pieces through one shared verb execution path.

## 4. Architecture

### 4.1 Shared execution layer

Add shared signed-verb plumbing in `cairn-cli` and pure helpers in `cairn-core`:

```text
cairn-cli
  ├── resolve vault/config/store/registry/keystore
  ├── build or accept Request envelope
  ├── sign local CLI-shaped requests when needed
  ├── resolve_issuer + EnvelopeVerifier::verify
  ├── call verb-specific handler with VerifiedSignedIntent
  └── emit common Response envelope

cairn-core
  ├── pure ingest/capture filtering and policy_trace conversion
  ├── pure retrieve response shaping
  ├── pure summarize rollup shaping
  └── pure hot-memory composition/trimming helpers

cairn-store-sqlite
  ├── persistence, WAL rows, consent journal rows
  ├── active record/session/trace/profile queries
  └── no policy decisions beyond scoped SQL predicates supplied by caller
```

The shared layer is not a new public protocol. It is an internal boundary that
keeps CLI, MCP, and SDK behavior convergent: every surface presents or builds
the same `Request { contract, verb, signed_intent, args }` shape, and every
surface receives the same response envelope.

### 4.2 Direct CLI signing

Human-friendly commands like `cairn ingest --body ...` remain valid. They become
thin wrappers over the signed envelope:

1. Resolve the active vault and identity.
2. Resolve the issuer from `--issuer`, `CAIRN_ISSUER`, or the vault's default
   active local identity. Mutating verbs fail closed if no active signing
   identity is available.
3. Load the issuer signing key from the keystore.
4. Build `SignedIntent` using the generated args, `operation_id`, scope policy,
   replay sequence or server challenge, and bounded expiry.
5. Sign the canonical signed-intent bytes.
6. Immediately route through `resolve_issuer` and `EnvelopeVerifier::verify`.

MCP/SDK callers may supply a complete signed request envelope directly. CLI
direct commands synthesize that envelope; they do not bypass it.

### 4.3 Core/store boundary

`cairn-store-sqlite` stays an adapter. It may expose efficient query methods,
transactions, WAL runner hooks, and trace/session helpers, but it must not own:

- identity policy decisions,
- privacy filtering,
- redaction/fencing decisions,
- hot-memory recipe semantics,
- summarize policy.

Those belong to the verb/pipeline layer so future MCP and SDK adapters can call
the same behavior.

## 5. Verb Data Flow

### 5.1 `ingest`

Pipeline:

```text
parse CLI or Request args
  -> resolve source body/file/url/folder item
  -> redact(raw_body)
  -> fence(redacted.text)
  -> should_memorize(FilterInputs)
  -> default_visibility(identity/mode/source/policy)
  -> extract/classify/scope draft records
  -> MemoryRecord::validate_against_intent(&VerifiedSignedIntent)
  -> build FlushPlan
  -> WAL/store upsert + consent journal
  -> committed/rejected/aborted response with policy_trace
```

Requirements:

- Pass `fenced.text`, never `raw_body`, to extraction/classification.
- On `Decision::Discard`, return `rejected`, append a body-free blocked audit
  entry where configured, and write no record rows.
- Folder ingest keeps the extraction cache, but cached results must represent
  filtered/fenced inputs or be invalidated by the filter/cache key version.
- The response includes live `policy_trace` entries for redaction, fence, filter,
  visibility, scope, and consent/WAL outcome.

### 5.2 `capture_trace`

Pipeline:

```text
verify request envelope
  -> read JSONL CaptureEvent stream
  -> validate and group by (session_id, turn_id)
  -> resolve each payload_ref under sources/
  -> redact + fence + should_memorize before projection
  -> project trace records
  -> per-turn transaction: renumber, validate links, upsert events, summarize
  -> response with trace_id, failed_turns, policy_trace
```

Requirements:

- Invalid identity context rejects the whole invocation before payload bodies are
  opened.
- Privacy filtering runs before any trace event or turn summary is persisted.
- A poisoned turn stays isolated from other turn groups, but a privacy block in
  a turn prevents that turn's partial rows from landing.
- `capture_trace` must not use a faster trace path that bypasses the ingest
  privacy filters.

### 5.3 `retrieve`

Implement the generated variants:

- `record`: active record by id.
- `session`: ordered turn/session records with limit/order/include handling.
- `turn`: one turn in one session with include filtering.
- `folder`: projected folder subtree.
- `scope`: scoped recent/history page.
- `profile`: synthesized static/dynamic profile.

Requirements:

- Verify scope before store reads that could leak target existence.
- Store queries must be narrowed by the verified scope tuple and visibility
  tier; a shared multi-tenant DB must not be queried unscoped.
- Missing authorized data returns a committed empty/not-found shape only after
  identity and scope checks pass.
- `retrieve` responses include read policy trace where the policy-trace design
  requires it, especially `read.visibility` and consent-gate outcomes for
  redacted/protected records.

### 5.4 `summarize`

Pipeline:

```text
verify request envelope
  -> retrieve authorized source records or session/turn windows
  -> deterministic P0 rollup from retrieved text and metadata
  -> if persist=false: committed response only
  -> if persist=true: run summary record through same validate/FlushPlan/WAL path
```

Requirements:

- P0 rollups are local and deterministic. No external LLM is required.
- Source records are filtered by the same read policy used by `retrieve`.
- Persisted summaries carry provenance back to source records and a consent ref.
- Persisted summaries never skip signed-envelope verification or WAL.

### 5.5 `assemble_hot`

Pipeline:

```text
verify request envelope
  -> load purpose.md and index.md
  -> load profile summary
  -> load pinned user/feedback records
  -> load top-salience project records
  -> load active playbook
  -> load recent user_signal records
  -> compose in HotMemoryConfig.recipe order
  -> trim by configured byte budget with UTF-8-safe boundaries
  -> build #288 segments
  -> validate AssembleHotData before emit
```

Requirements:

- Honor `--session` and `--budget`; do not accept-and-drop either flag.
- The returned prefix is bounded by `HotMemoryConfig.max_bytes` or the explicit
  budget override, whichever applies.
- Segment ranges cover the emitted prefix with no gaps or overlaps.
- Records above the verified visibility tier never enter the prefix.
- If an input source fails to load, fail closed with an aborted response unless
  the source is explicitly optional by recipe semantics.

## 6. Error Semantics

| Condition | Response status | Store/WAL mutation | Notes |
|---|---:|---:|---|
| Invalid signature, unknown issuer, revoked key, expired intent, scope denied | `rejected` | none | Map through `envelope_error_for`; no target-existence leaks. |
| Privacy block (`pii_blocked`, policy denied, duplicate/drop) | `rejected` | none | Include body-free policy trace and blocked audit metadata. |
| Capability absent | `rejected` | none | Exit `69` where CLI maps to sysexits. |
| Store/WAL/SQLite/projector failure | `aborted` | no partial committed side effects | Earlier policy trace entries stay in response. |
| Authorized missing record/session/folder/profile data | `committed` | none | Empty/not-found data shape after policy passes. |
| Idempotent replay | prior outcome | no duplicate effects | Operation id is the replay key. |

`aborted` means the system failed while trying to perform an admitted operation.
Policy denials are not internal errors.

## 7. Policy Trace

Write verbs populate non-empty `policy_trace` whenever a gate runs. At minimum:

- `ingest`: redact, fence, should_memorize, visibility, scope, consent/WAL.
- `capture_trace`: redact, fence, should_memorize, trace visibility/session
  floor, consent/WAL.
- `summarize` with `persist=true`: read gates for sources plus write gates for
  the summary record.

Read verbs populate policy trace according to the policy-trace design:

- `retrieve` exposes visibility/consent gate decisions without leaking records
  the caller could not discover.
- `assemble_hot` emits only aggregate read gate outcomes for scoped recipe
  queries. It does not include per-record exclusions or hidden record IDs.

Every `detail` remains body-free. Tests must run the existing body-free walker
against live verb responses, not only synthetic fixtures.

## 8. Testing Strategy

Follow TDD: failing tests first, then minimal implementation.

### 8.1 Core unit tests

- Ingest privacy gate composition: raw body is redacted/fenced before drafts.
- Policy trace conversion for live gate outcomes.
- Summarize deterministic rollup stability.
- Hot-memory UTF-8-safe trimming and segment validation.
- Profile/static/dynamic synthesis from fixture records.

### 8.2 Store/transaction tests

- Upsert via WAL produces a record, consent journal row, and committed response.
- Invalid envelope does not create WAL rows.
- Capture-trace per-turn atomicity: poisoned turn rolls back, sibling turn can
  commit.
- Retrieve scope predicates do not return out-of-scope rows.

### 8.3 CLI integration tests

Against temp vaults:

- `cairn ingest --body ... --json` commits a redacted/fenced record and returns
  non-empty body-free policy trace.
- PII-bearing `ingest` returns `rejected` and persists no raw body.
- `capture_trace --from ... --json` stores turn/tool trace data after filters.
- PII-bearing `capture_trace` blocks before trace rows or summaries land.
- `retrieve` covers all six variants.
- `summarize` covers transient and persisted modes.
- `assemble_hot --json` returns a bounded prefix and valid segments.
- Invalid issuer/signature/revoked identity rejects every signed verb and
  leaves store/WAL unchanged.

### 8.4 Verification commands

Targeted during development:

```bash
cargo nextest run -p cairn-core policy_trace ingest summarize assemble_hot
cargo nextest run -p cairn-store-sqlite envelope_blocks_wal capture_trace
cargo nextest run -p cairn-cli ingest capture_trace retrieve summarize assemble_hot
```

Before PR:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

## 9. Implementation Order

One PR, but staged commits:

1. Add signed CLI request builder and shared verified verb context.
2. Wire failing invalid-envelope tests across all five verbs.
3. Wire `ingest` privacy/filter/policy-trace/FlushPlan/WAL/store path.
4. Wire `capture_trace` through the same privacy gate.
5. Wire `retrieve` variants and scoped store reads.
6. Wire deterministic `summarize`, including persisted summary writes.
7. Replace `assemble_hot` stub loader with real bounded source loading.
8. Add live response body-free policy-trace tests and snapshots.
9. Run full verification and update PR description with issue/brief refs.

This order gives reviewers meaningful checkpoints while still producing a
single PR that closes #61.

## 10. Open Decisions Closed by This Spec

- **One PR for the whole issue.** The implementation can be staged internally,
  but the branch should close #61 as one review unit.
- **Shared execution layer.** Policy, identity, WAL admission, and envelope
  handling are implemented once and reused by all five verbs.
- **Direct CLI commands synthesize signed envelopes.** Human CLI ergonomics stay
  intact without weakening the signed-envelope acceptance criterion.
- **SQLite remains an adapter.** It does not own policy or verb orchestration.
