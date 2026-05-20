# Issue 78 Retrieve Variants Design

**Status:** Approved for implementation
**Date:** 2026-05-12
**Issue:** [#78](https://github.com/windoliver/cairn/issues/78)
**Brief sections:** §8.0.c retrieve variants, §8.1 session lifecycle, §18.c trace records and turn reconstruction
**Base:** `origin/main` after #77 full trace scope

## Context

`origin/main` already has the trace write substrate needed for retrieve work:
`capture_trace` persists privacy-filtered trace records, stores session/turn
linkage in frontmatter, validates tool parent links, and projects all trace
event variants into normal `MemoryKind::Trace` records.

The remaining issue #78 gap is on the read side. `retrieve` can already fetch
records, sessions, and turns, but session retrieval is still event-limited
rather than turn-windowed, there is no first-class tool-call target, linkage
metadata is too thin for harness reconstruction, cursors are rejected, and
budget trimming is not documented in the response.

## Goals

- Add a first-class `retrieve` target for a single tool call.
- Return ordered session history by turn windows, including recent turns.
- Keep turn retrieval scoped to one `(session_id, turn_id)` and include tool
  traces when requested.
- Expose harness-useful linkage metadata without exposing raw payload storage
  internals.
- Apply scope, visibility, redaction-preserving reads, and deterministic budget
  trimming to session, turn, and tool-call retrieval.
- Document budget trimming in the response policy trace.
- Advertise the wired retrieve capabilities once the behavior is tested.

## Non-Goals

- No cold rehydration or archive restore path.
- No new SQLite trace schema migration.
- No retrieval from raw `payload_ref` files.
- No new authorization semantics; retrieval composes existing scope,
  visibility, consent, and record lifecycle checks.
- No tokenizer dependency for P0. Budget enforcement uses a deterministic text
  size proxy and reports that proxy in policy trace metadata.

## Public Contract

The IDL gains a new retrieve args variant:

```json
{
  "target": "tool_call",
  "session_id": "session-1",
  "turn_id": "turn-1",
  "tool_call_id": "tool-1"
}
```

The CLI exposes the same target as:

```bash
cairn retrieve --tool-call tool-1 --session session-1 --turn turn-1 --json
```

The response data adds `DataToolCall`:

```json
{
  "target": "tool_call",
  "session_id": "session-1",
  "turn_id": "turn-1",
  "tool_call_id": "tool-1",
  "items": []
}
```

`TurnItem` gains an optional `linkage` object used by session, turn, and
tool-call retrieval. Fields that are not present on the persisted trace record
are omitted:

```json
{
  "record_id": "01...",
  "trace_event": "pre_tool",
  "sequence": 2,
  "capture_event_id": "evt-2",
  "parent_event_id": "evt-1",
  "tool_call_id": "tool-1",
  "payload_hash": "sha256:..."
}
```

`payload_ref` stays internal. Harnesses can reconstruct ordered turns and tool
parent/child relationships from record ids, event ids, sequence numbers, parent
ids, tool call ids, and payload hashes without reading storage files directly.

Existing include behavior remains:

- `include: ["tool_calls"]` exposes tool trace details on turn/session items.
- `include: ["reasoning"]` exposes persisted reasoning fields when present.
- Without an include flag, optional sensitive or verbose fields stay omitted.

## Session Windows

Session retrieval is turn-windowed rather than raw-row-windowed:

1. Query active trace records through the existing scoped list path.
2. Filter to the requested `session_id`.
3. Group records by `trace.turn_id`.
4. Sort records inside each turn by `trace.sequence`, then record id.
5. Sort turn groups by their trace capture/update ordering, tie-broken by
   `turn_id`, in the requested `asc` or `desc` order.
6. Apply cursor offset and `limit` to turn groups, not individual trace rows.
7. Flatten selected turn groups back into ordered `DataSession.items`.

For `order = desc`, the newest turns appear first, but events inside each turn
remain in ascending sequence order so a single turn can still be replayed
linearly. `next_cursor` advances by turn-group offset and is deterministic for
the filtered record set.

## Turn And Tool-Call Retrieval

Turn retrieval keeps the current shape but tightens ordering and metadata:

- Filter by authorized scope, `session_id`, and `turn_id`.
- Sort by `trace.sequence`, then record id.
- Return an empty committed turn payload when the authorized turn has no rows.
- Include relevant tool traces only when `include: ["tool_calls"]` is present.

Tool-call retrieval is a narrower turn retrieval:

- Filter by authorized scope, `session_id`, `turn_id`, and `tool_call_id`.
- Sort by `trace.sequence`, then record id.
- Return an empty committed tool-call payload when no authorized rows match.
- Expose linkage on every item so harnesses can prove the parent `pre_tool`
  record and child `post_tool` or `tool_output` records were linked correctly.

## Privacy And Policy

Retrieval never opens raw trace payload refs. It only reads persisted
`MemoryRecord` rows that were already redacted, fenced, and filtered by the
capture path. This preserves the write-path privacy boundary and keeps
retrieval from becoming a bypass around `capture_trace`.

All retrieve variants in this issue use the existing scoped list read:

- `read.scope` records whether the caller's scope admitted the trace rows.
- `read.visibility` records visibility filtering.
- `read.consent` records consent/read lifecycle admission.
- `read.budget` records deterministic output trimming without including body
  text in policy trace details.

## Budget Trimming

The implementation enforces a deterministic text budget after authorization,
ordering, cursoring, and include filtering.

For P0, the budget is a character-count proxy using
`config.search.max_snippet_chars_per_page` as the existing configured
read-output limit, rather than adding a tokenizer dependency. The trimming rule
keeps the ordered prefix that fits within budget and always keeps the first
selected item or turn group even if it individually exceeds the budget. Session
trimming prefers whole turn groups so turn reconstruction is not split unless a
single turn group alone exceeds the budget.

The response policy trace includes only counts and parameters, for example:

```text
read.budget: pass chars=8000 items_in=12 items_out=8 turns_in=4 turns_out=3 trimmed=true
```

No policy trace entry may include retrieved body text.

## Codegen And Capabilities

The IDL changes drive generated Rust and schema updates:

- Add `ArgsToolCall`, `DataToolCall`, and `TurnItem.linkage`.
- Add `tool_call` to retrieve response target dispatch.
- Update the SDK retrieve data enum and generated validation checks.
- Add `cairn.mcp.v1.retrieve.tool_call`.
- Flip `retrieve.session`, `retrieve.turn`, and `retrieve.tool_call`
  advertisement only after tests prove the runtime behavior.

Generated files are updated through the IDL codegen path rather than edited by
hand.

## Test Strategy

Implementation follows TDD:

- Core retrieve-shaping tests for `TurnItem.linkage` and `DataToolCall`.
- CLI tests for session turn-window ordering, `order`, `limit`, and cursor.
- CLI tests for turn retrieval with and without `include: ["tool_calls"]`.
- CLI tests for direct tool-call retrieval by `session_id`, `turn_id`, and
  `tool_call_id`.
- Budget trimming tests proving deterministic prefix behavior and body-free
  `read.budget` policy trace details.
- Redacted trace retrieval tests proving persisted redacted bodies are returned
  and raw sensitive text is not exposed.
- Capability/status tests for the newly advertised retrieve variants.

## Verification

Focused commands:

- `cargo test -p cairn-core retrieve`
- `cargo test -p cairn-cli --test issue_61_signed_verbs`
- `cargo test -p cairn-cli --test capture_trace_verb`
- `cargo test -p cairn-store-sqlite trace`
- `cargo run -p cairn-idl --bin cairn-codegen -- --check`
- `scripts/check-core-boundary.sh`
- `cargo fmt --all`
- `cargo clippy -p cairn-core -p cairn-cli -p cairn-store-sqlite -p cairn-idl --all-targets -- -D warnings`

Before PR creation, run the widest feasible workspace test command and record
any local environment limits in the PR description.
