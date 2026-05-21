# Issue 77 Full Trace Scope Design

**Status:** Approved for implementation
**Date:** 2026-05-12
**Issue:** [#77](https://github.com/windoliver/cairn/issues/77)
**Brief sections:** §5.0 End-to-end turn journey, §5.2 Write path, §9.3 Five-hook lifecycle, §10.0 Memory lifecycle
**Base:** `origin/main` after #271, #79, #84, #9, and #46

## Context

Issue #77 already has most of its substrate on `origin/main`: PR #271 persists
trace rows as `MemoryKind::Trace`, links records by `(session_id, turn_id,
sequence)`, resolves `PostToolUse` parent links, writes deterministic
`turn_summary` rows, redacts bodies before persistence, and exposes trace rows
through the SQLite store and retrieve/read paths.

The remaining gap is that `capture_trace` still fails closed for non-hook event
shapes even though the dependent sensor and hook issues have landed. To close
#77 in one PR, this work widens the existing trace path so all seven trace event
types can be imported, linked, reconstructed, and included in the normal privacy,
search, retention, and forget paths.

## Goals

- Persist all seven trace event variants in one full turn:
  `user_message`, `agent_message`, `pre_tool`, `post_tool`, `tool_output`,
  `stop`, and generated `turn_summary`.
- Preserve the existing single persistence path: `capture_trace` remains the
  canonical importer and `MemoryKind::Trace` remains the single record kind.
- Keep tool parent/child rules strict for both `post_tool` and `tool_output`.
- Route terminal tool output through the existing dispatch/squash decision so
  interactive terminal output is compacted deterministically and structured
  terminal output bypasses squash.
- Keep trace bodies privacy-filtered before persistence and queryable through
  the existing store/search/retrieve/forget machinery.

## Non-Goals

- No new memory kind or SQLite trace schema migration.
- No new public CLI flags or IDL changes for `capture_trace`.
- No background sensor runtime, shell polling loop, or harness installer.
- No tree-structured session model beyond the existing session/turn links.

## Event Classification

The classifier in `cairn-core::pipeline::capture_trace` becomes the single
static mapping from `CapturePayload` to `TraceEvent`:

| Input event shape | Trace event |
| --- | --- |
| `CapturePayload::Hook { hook_name: "UserPromptSubmit" }` | `UserMessage` |
| `CapturePayload::Hook { hook_name: "PreToolUse" }` | `PreTool` |
| `CapturePayload::Hook { hook_name: "PostToolUse" }` | `PostTool` |
| `CapturePayload::Hook { hook_name: "ToolOutput" }` | `ToolOutput` |
| `CapturePayload::Hook { hook_name: "Stop" }` | `Stop` |
| `CapturePayload::Terminal { .. }` with `refs.tool_id` | `ToolOutput` |
| `CapturePayload::Proactive { kind: "agent_message", .. }` | `AgentMessage` |
| `CapturePayload::Proactive { kind: "assistant_message", .. }` | `AgentMessage` |

Terminal events without `refs.tool_id` remain unclassifiable for trace import.
That keeps ordinary terminal observations out of tool-call parent validation.
Proactive events only become `AgentMessage` when their kind explicitly says
they are an agent/assistant message; other proactive memory kinds still belong
to ingest/classification flows, not turn trace reconstruction.

## Body Resolution

`capture_trace` already resolves payload bytes from `sources/`, verifies
`payload_hash`, redacts, fences, and projects records. This PR keeps that flow
and inserts one source-specific transform before redaction:

- Non-terminal events use the verified payload text unchanged.
- Terminal `ToolOutput` events call a small core helper that composes
  `dispatch(event, registry)` with `squash`:
  - `TerminalContext::InteractiveTty` -> dispatch admits squash, compacted bytes
    become the projected trace body input.
  - `TerminalContext::NonInteractiveOrStructured` -> bypass squash and preserve
    verified text unchanged.
  - legacy `context: None` -> fail the turn with a migration-needed error rather
    than silently treating unknown terminal bytes as structured output.

The helper lives in `cairn-core` so the CLI does not need access to private
`SquashAdmission` internals. The output is still plain text passed through the
existing redaction/fence filter before `MemoryRecord` construction.

## Linkage

The existing parent resolution algorithm remains authoritative:

- `PreTool` rows register `tool_call_id -> capture_event_id`.
- `PostTool` and `ToolOutput` rows require `tool_call_id`.
- For `PostTool` and `ToolOutput`, `parent_event_id` resolves to the matching
  `PreTool` in the same batch, or to an existing persisted `PreTool` in the same
  `(session_id, turn_id)`.
- Missing or mismatched parents fail the whole turn transaction.

No separate edge table is needed; existing trace generated columns and
`StoreTx::validate_turn_links` already cover `tool_output`.

## Reconstruction

Turn reconstruction remains record-based:

- `StoreTx::list_trace_events(session_id, turn_id)` returns active non-summary
  trace rows in sequence order.
- `summarize_turn` generates exactly one summary row for a closed turn.
- `retrieve(target=Turn|Session)` keeps using `TurnItem` conversion from trace
  records, so the full imported turn can be reconstructed from the ordered rows
  and their `trace` linkage fields.

The new test fixture imports a complete turn with user message, agent message,
pre-tool, post-tool, terminal tool output, and stop. The test asserts all six
non-summary rows plus the deterministic summary are present, ordered, and
convertible into the expected turn roles.

## Privacy, Search, Retention, Forget

The PR does not create separate paths for these concerns:

- Privacy: all admitted payload text goes through `redact`, `fence`, and
  `should_memorize` before projection. Tests assert sensitive terminal output is
  redacted in persisted trace bodies.
- Search: trace rows remain `MemoryKind::Trace`, `MemoryVisibility::Private`,
  and `scope.session_id`-bound, so existing search and retrieve code can see
  them under authorized scope.
- Retention: trace rows inherit record lifecycle and active/tombstone handling.
- Forget: trace rows continue to participate in record-level forget and
  `payload_hash_count_in_scope`; the full-scope test covers that trace rows can
  be targeted through the existing forget path without a trace-specific bypass.

## Test Strategy

Implementation follows TDD:

- Core classifier tests for terminal `ToolOutput`, hook `ToolOutput`, proactive
  `AgentMessage`, and rejected ambiguous terminal/proactive events.
- Core terminal body-routing tests for interactive squash, structured bypass,
  and legacy-context failure.
- CLI importer integration test for a full all-variant turn, including
  parent/child links for `post_tool` and `tool_output`.
- CLI privacy test proving terminal secrets are redacted before persistence.
- Focused search/retrieve/forget participation tests where existing coverage is
  not already sufficient for the widened trace variants.

## Verification

Focused commands:

- `cargo test -p cairn-core pipeline::capture_trace`
- `cargo test -p cairn-core pipeline::dispatch`
- `cargo test -p cairn-cli --test capture_trace_verb`
- `cargo test -p cairn-store-sqlite trace`
- `cargo test -p cairn-cli --test forget_record`
- `cargo fmt --all`
- `cargo clippy -p cairn-core -p cairn-cli -p cairn-store-sqlite --all-targets -- -D warnings`

Before PR creation, run the repo-required boundary checks that are feasible in
the local workspace.
