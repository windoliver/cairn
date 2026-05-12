# Five-Hook Harness Integration Design — Issue #79

**Date:** 2026-05-08  
**Issue:** [#79 — Implement five-hook command handlers for harness integration](https://github.com/windoliver/cairn/issues/79)  
**Brief sections:** §5.0 End-to-end turn journey · §9.3 Five-hook lifecycle · §19.a KISS v0.1 subset  
**Status:** Approved

---

## 1. Scope

Implement the v0.1 harness hook contract for Cairn as a stable CLI integration surface:
`cairn hook <name> ...`.

This issue defines and implements exactly five canonical hooks:

1. `SessionStart`
2. `UserPromptSubmit`
3. `PreToolUse`
4. `PostToolUse`
5. `Stop`

The goal is to make hook execution explicit, typed, and testable without adding a parallel memory
API. The CLI remains the ground-truth integration surface. Hook handlers may reuse existing verbs
such as `assemble_hot` and `capture_trace`, but hooks are not new core verbs themselves.

Out of scope for this issue:

- a sixth canonical `PreCompact` hook
- a full workflow runner or background scheduler implementation
- consumer-specific Claude Code / Codex / Gemini installation files
- broader lifecycle IDL changes for new MCP hook tools

---

## 2. Canonical Hook Set

The authoritative v0.1 hook set is:

- `SessionStart`
- `UserPromptSubmit`
- `PreToolUse`
- `PostToolUse`
- `Stop`

`PreCompact` appears in older brief text, but §19.a and issue #79 define the actual v0.1 contract
as the five-hook set above. For this issue, `PreCompact` is treated as stale design text rather
than an additional supported lifecycle event.

This design intentionally rejects two weaker alternatives:

1. Keep `PreCompact` as a deprecated alias now. This makes the contract ambiguous and expands the
   test matrix before the first stable integration surface exists.
2. Support six first-class hooks. This conflicts with the issue scope and the KISS subset in
   §19.a.

If a future harness needs compaction-specific capture, it should be introduced in a separate issue
as either an explicit compatibility alias or a sixth optional hook, not folded into the canonical
five-hook contract for v0.1.

---

## 3. Public CLI Surface

### 3.1 Command shape

Add a new top-level integration entrypoint:

```text
cairn hook <name> [hook-specific flags]
```

`<name>` is a closed-set enum over the five canonical hook names. The CLI owns parsing and
dispatch. Harness-specific shell glue should invoke this command rather than reaching into
crate-private logic.

This is preferred over five top-level commands such as `cairn hook-stop` because:

- the brief describes hooks as `cairn hook <name>`
- one dispatcher keeps lifecycle logic discoverable
- harness docs stay stable even if payload details evolve

### 3.2 Relationship to verbs

Hooks are integration commands, not additions to the eight core verbs. They may delegate into
shared implementations behind:

- `assemble_hot` for `SessionStart`
- `capture_trace`-style persistence for trace-oriented hooks

This preserves the brief invariant that the public memory contract remains the eight verbs, while
still giving harnesses a stable command to execute at lifecycle boundaries.

### 3.3 MCP equivalents

The CLI is sufficient to satisfy v0.1 acceptance. MCP equivalents are optional and only belong in
this issue if they are thin wrappers over the exact same handler functions. No parallel lifecycle
implementation should be introduced in `cairn-mcp`.

---

## 4. Architecture

### 4.1 Ownership boundaries

The first implementation slice should stay primarily in `cairn-cli`.

| Responsibility | Location | Notes |
|---|---|---|
| `hook` subcommand registration and dispatch | `crates/cairn-cli/src/main.rs` | Closed-set hook name parsing |
| Shared hook types and helpers | `crates/cairn-cli/src/hooks/mod.rs` | Common JSON/stdout helpers and hook result type |
| Per-hook handlers | `crates/cairn-cli/src/hooks/*.rs` | One handler module per canonical hook |
| Shared hot-memory logic | factored from `crates/cairn-cli/src/verbs/assemble_hot.rs` | Reused by verb and hook path |
| Shared trace persistence logic | factored from `crates/cairn-cli/src/verbs/capture_trace.rs` | Reused by verb and hook path |
| Stop enqueue artifact | `crates/cairn-cli/src/hooks/queue.rs` | Durable request boundary for post-turn work |

Harness payload parsing stays in `cairn-cli`. Harness-specific JSON should not leak into
`cairn-core` domain types. Core and workflow crates remain reusable boundaries; this issue does not
require adding a new core contract.

### 4.2 Why not `cairn-sensors-local`

Placing the hook dispatcher in `cairn-sensors-local` would weaken the "CLI is ground truth"
invariant and make cross-harness integration harder to exercise through the existing binary tests.
Sensor crates can remain the long-term home for shared sensor logic, but the public lifecycle
entrypoint belongs in the CLI surface.

---

## 5. Hook Behavior

### 5.1 `SessionStart`

Purpose: startup / resume path that assembles hot memory for the active session.

Behavior:

- validate hook payload
- resolve session context
- invoke the shared hot-memory assembly path
- return the hot-prefix payload synchronously

This hook is latency-sensitive and should do no background scheduling on the request path.

### 5.2 `UserPromptSubmit`

Purpose: capture the incoming user prompt and emit routing hints.

Behavior:

- validate hook payload
- normalize the incoming prompt event into the hook trace shape
- persist the prompt event through the shared trace path
- return lightweight routing metadata synchronously

This keeps the trace substrate authoritative while leaving richer classification or search fan-out
as later work.

### 5.3 `PreToolUse`

Purpose: record the intent to execute a tool.

Behavior:

- validate hook payload
- normalize the tool call frame into a trace event
- persist it synchronously
- return success without waiting on any downstream workflow

This hook exists so failed or partial tool execution still leaves a durable trace boundary.

### 5.4 `PostToolUse`

Purpose: record the tool result and attach it to the correct turn/session.

Behavior:

- validate hook payload
- normalize result metadata and linkage
- persist the resulting trace event synchronously
- return success after the write boundary completes

This is the hook that must preserve parent/child linkage with turn reconstruction work from #77.

### 5.5 `Stop`

Purpose: close the session turn and schedule post-turn work.

Behavior:

- validate hook payload
- persist the terminal stop trace event
- append a durable post-turn work request
- return immediately once the enqueue artifact is durably written

`Stop` must not block on the eventual work itself. The contract is "accepted and durably queued",
not "all downstream work completed".

---

## 6. Execution Model

### 6.1 Hook result schema

The eight-verb response envelope only supports `committed`, `aborted`, and `rejected`, and `hook`
is not one of the eight public verbs. For this integration surface, Cairn should use a small hook
result schema rather than inventing a fake core-verb response.

Recommended shape:

```json
{
  "ok": true,
  "hook": "Stop",
  "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
  "artifacts": {
    "trace_id": "01ARZ3NDEKTSV4RRFFQ69G5FAA",
    "queued_jobs": ["01ARZ3NDEKTSV4RRFFQ69G5FAB"]
  }
}
```

Failure responses use:

```json
{
  "ok": false,
  "hook": "PostToolUse",
  "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
  "error": {
    "code": "Internal",
    "message": "failed to persist hook trace",
    "retry_guidance": "retry cairn hook PostToolUse after restoring store access"
  }
}
```

This keeps the hook surface typed and auditable without expanding the core MCP wire contract.

### 6.2 Synchronous boundaries

Synchronous work is limited to:

- `SessionStart`: hot-memory assembly
- `UserPromptSubmit`: prompt trace write + routing hint generation
- `PreToolUse`: trace write
- `PostToolUse`: trace write
- `Stop`: stop trace write + durable enqueue write

Everything beyond those boundaries is deferred.

### 6.3 Stop enqueue boundary

The current repository does not yet contain a real workflow runner; `cairn-workflows` is still a
stub capability host. This issue should therefore introduce a durable enqueue artifact owned by the
hook path rather than pretending a background executor already exists.

The first version of `Stop` should:

1. write the stop trace event
2. append a durable post-turn work request
3. fsync that request boundary
4. return

The later orchestrator implementation can consume the same artifact. This keeps the hook contract
honest and lets `Stop` satisfy the acceptance criterion that it "returns quickly" after enqueueing.

---

## 7. Error Model

Hook failures must include:

- a typed error code
- the `operation_id`
- retry guidance

Recommended families:

| Failure | Code | Expected behavior |
|---|---|---|
| Unknown hook name | `InvalidArgs` | caller fixes invocation and retries |
| Malformed hook payload | `InvalidArgs` | caller fixes payload shape and retries |
| Trace persistence failure | `Internal` | include operation id and retry guidance |
| Stop enqueue failure | `Internal` | explicitly advise retrying `cairn hook Stop` |
| Capability intentionally unavailable | `CapabilityUnavailable` when applicable | fail closed, do not silently degrade |

Hooks should not crash the harness process unless an operator policy explicitly requires fail-closed
behavior. The default v0.1 posture is bounded failure reporting with typed output.

---

## 8. File Layout

Expected file additions and edits:

- `crates/cairn-cli/src/main.rs`
- `crates/cairn-cli/src/hooks/mod.rs`
- `crates/cairn-cli/src/hooks/session_start.rs`
- `crates/cairn-cli/src/hooks/user_prompt_submit.rs`
- `crates/cairn-cli/src/hooks/pre_tool_use.rs`
- `crates/cairn-cli/src/hooks/post_tool_use.rs`
- `crates/cairn-cli/src/hooks/stop.rs`
- `crates/cairn-cli/src/hooks/queue.rs`
- `crates/cairn-cli/src/verbs/assemble_hot.rs`
- `crates/cairn-cli/src/verbs/capture_trace.rs`
- `crates/cairn-cli/tests/*` for CLI and JSON-shape integration coverage

The design intentionally avoids broad edits in `cairn-core` and `cairn-mcp` unless a shared helper
or type becomes unavoidable. The primary objective is a stable hook surface with reused verb logic,
not a lifecycle-wide refactor.

---

## 9. Verification

Verification should prove contract shape, latency boundaries, and failure behavior.

### 9.1 Lifecycle integration tests

- one integration test per hook
- one happy-path lifecycle test covering:
  `SessionStart -> UserPromptSubmit -> PreToolUse -> PostToolUse -> Stop`

### 9.2 Latency smoke tests

- `SessionStart` returns after hot-memory assembly without waiting on deferred work
- `Stop` returns after the durable enqueue boundary, not after workflow execution

These are synchronous-boundary tests, not full performance benchmarks.

### 9.3 Failure-mode tests

- malformed hook name is rejected
- malformed hook payload returns typed error + retry guidance
- trace-write failure returns typed error + operation id
- stop-enqueue failure returns typed error + operation id + retry guidance

### 9.4 Contract tests

- the canonical supported hook set is exactly five names
- `PreCompact` is not accepted as a canonical v0.1 hook name
- hook outputs are machine-parseable and stable in JSON mode

---

## 10. Follow-Up Work

This design intentionally leaves several adjacent items for later issues:

- brief cleanup to replace stale `PreCompact` references where the v0.1 contract is now concrete
- real post-turn workflow consumption by the orchestrator implementation
- optional MCP hook wrappers if they can be exposed without duplicating lifecycle logic
- broader classifier widening and richer trace payload capture that remains blocked outside #79

The immediate objective of #79 is smaller: ship the five-hook harness contract with a truthful stop
enqueue boundary and typed failure reporting.
