# Design — `PreCompact` reinjection + transcript snapshot (issue #310)

**Status:** Approved (brainstorm 2026-05-09).
**Brief refs:** §4 SensorIngress contract; §5 hot memory pipeline; §8.0.f `assemble_hot`; §9.3 five-hook lifecycle.
**Issue:** [#310](https://github.com/windoliver/cairn/issues/310).

## 1. Goal

Add a first-class `PreCompact` hook path that runs immediately before a harness compacts its rolling context window. The hook must do two things in one ordered flow:

1. Re-assemble hot memory with a `pre_compact`-specific recipe and budget so the harness can splice fresh Cairn context into the post-compaction prefix.
2. Persist a pre-compaction transcript snapshot for later trace distillation, preserving the behavior already described in the design brief.

The result is a single, typed hook surface that keeps Cairn's read-path reinjection and write-path trace capture in sync at the exact lifecycle boundary where context would otherwise be lost.

## 2. Non-goals

- Predicting when compaction should happen without a harness signal.
- Mutating the harness transcript directly. Cairn assembles and returns payloads; the harness performs the splice.
- Cross-session reinjection or snapshotting. Every `PreCompact` event is scoped to one session.
- Adding provider-specific prompt-cache API fields to Cairn surfaces.
- Folding the entire implementation into `assemble_hot` itself. The orchestration belongs to the hook path, not the verb renderer.

## 3. Problem statement

Long-running agent sessions eventually trigger harness-side compaction. Today, Cairn can inject hot memory at session start, but once the harness summarizes and truncates the rolling window, the injected context is no longer recoverable because the harness cannot distinguish Cairn-sourced prefix text from ordinary conversation history.

Issue #310 closes that loop by adding a pre-compaction trigger. Just before the harness compacts, it asks Cairn for a fresh, budget-bounded reinjection payload. At the same boundary, Cairn should also snapshot the transcript so later distillation still sees the raw pre-compaction state instead of only the already-compressed conversation.

## 4. Proposed architecture

Add a dedicated `PreCompact` orchestration path that sits above `assemble_hot` and above trace persistence:

- `SensorEvent::PreCompact` becomes a typed core event carrying `session_id`, `token_count_before`, `compaction_target`, and `last_user_turn_index`.
- A new hook orchestrator handles the event, computes the reinjection budget, invokes `assemble_hot` with the configured pre-compaction recipe override, persists the transcript snapshot, emits telemetry, and returns a structured response to the harness.
- `assemble_hot` remains the text renderer for hot memory. It does not gain responsibility for transcript writes or sensor sequencing.
- Capability advertisement gains the explicit flat capability string `cairn.mcp.v1.sensors.pre_compact` so harnesses can negotiate support safely.

This keeps the boundaries clean: config owns policy, `assemble_hot` owns assembly, and the `PreCompact` path owns lifecycle sequencing and fail-closed behavior.

## 5. Contract shape

### 5.1 Sensor event

Add a typed event alongside the existing hook taxonomy:

```rust
pub enum SensorEvent {
    // ... existing variants ...
    PreCompact {
        session_id: SessionId,
        token_count_before: u32,
        compaction_target: u32,
        last_user_turn_index: u64,
    },
}
```

Field semantics:

- `token_count_before` is harness-reported and may be approximate.
- `compaction_target` is a hint, not a contract. It drives reinjection budgeting only.
- `last_user_turn_index` anchors snapshot naming / correlation and makes the event auditable within the session timeline.

### 5.2 Harness-facing result

The `PreCompact` path returns structured data, not just a raw string:

```rust
pub struct PreCompactOutput {
    pub reinjection_text: String,
    pub output_bytes: u64,
    pub budget_bytes: u64,
    pub recipe: String,
}
```

The harness uses `reinjection_text` for splicing. The remaining fields make the call observable, testable, and easy to log.

## 6. Configuration

Extend the hot-memory config block with two new fields:

```yaml
assemble_hot:
  pre_compact_recipe: handoff
  pre_compact_safety_ratio: 0.30
```

Rules:

- `pre_compact_recipe` defaults to `handoff`.
- `pre_compact_safety_ratio` defaults to `0.30`.
- The computed budget is `min(hot_memory.max_bytes, floor(compaction_target * pre_compact_safety_ratio))`.
- Ratios must be greater than `0.0` and less than or equal to `1.0`.
- A computed budget of `0` is valid and yields an empty reinjection payload.

The existing `hot_memory.recipe` remains the default session-start recipe. `pre_compact_recipe` is an explicit override used only for `PreCompact`.

## 7. Execution flow

The `PreCompact` path runs in this order:

1. Validate that the runtime advertises `cairn.mcp.v1.sensors.pre_compact`.
2. Compute the reinjection budget from `compaction_target` and `pre_compact_safety_ratio`, capped by `hot_memory.max_bytes`.
3. Resolve the `pre_compact_recipe` and invoke `assemble_hot` with the event `session_id` and computed budget.
4. Persist the pre-compaction transcript snapshot to the existing trace storage path.
5. Emit `sensor.pre_compact` telemetry with `session_id`, `budget`, `output_bytes`, and `recipe`.
6. Return `PreCompactOutput` to the harness.

This ordering is intentional: the reinjection payload is computed from the current session state, then the same lifecycle boundary is snapshotted for offline learning.

## 8. Failure semantics

`PreCompact` is fail-closed.

- If the feature is not wired or not advertised, return `CapabilityUnavailable` and sysexit `69`.
- If budget computation or config resolution fails, reject the hook.
- If `assemble_hot` fails, reject the hook.
- If transcript snapshot persistence fails, reject the hook.

Partial success is forbidden. Returning reinjection text while silently dropping the trace snapshot would make the hook's semantics unpredictable and hide distillation gaps. Likewise, writing a snapshot without returning the reinjection payload would violate the harness contract for the hook.

## 9. Capability and status

Expose `cairn.mcp.v1.sensors.pre_compact` in `status.capabilities` only when the full path is wired end-to-end:

- typed `PreCompact` event surface exists
- config fields are recognized
- orchestration path is enabled
- `assemble_hot` pre-compaction dispatch is available
- transcript snapshot persistence is available

Harnesses that do not support the hook simply never fire it. Harnesses that do support it can negotiate from `status` before attempting the call.

## 10. Telemetry

Emit a span named `sensor.pre_compact` with these fields:

- `session_id`
- `budget`
- `output_bytes`
- `recipe`

If snapshot persistence already has an internal trace identifier, include it as an additional field, but it is not required for the initial contract.

## 11. Testing strategy

Implementation must follow TDD and cover three layers.

### 11.1 Config tests

- Default config round-trips with `pre_compact_recipe: handoff` and `pre_compact_safety_ratio: 0.30`.
- Invalid ratios (`<= 0`, `> 1`) reject at config validation time.

### 11.2 Unit tests

- Budget math: `compaction_target = 8000` with default ratio yields `2400`, then caps against `hot_memory.max_bytes`.
- `compaction_target = 0` yields `budget = 0`.
- The orchestrator calls `assemble_hot` before returning and snapshot persistence before completing.
- Snapshot failure rejects the entire hook.
- `CapabilityUnavailable` is returned when `pre_compact` support is not wired.

### 11.3 Integration / snapshot tests

- A `PreCompact` event with budget `8000` produces reinjection output no larger than `2400` bytes under the default ratio.
- Status advertisement includes `cairn.mcp.v1.sensors.pre_compact` only when the path is fully wired.
- Telemetry for a successful run includes `session_id`, `budget`, `output_bytes`, and `recipe`.

## 12. File-level decomposition

The implementation should split responsibilities along these lines:

- `crates/cairn-core/src/contract/` for the typed sensor event surface and capability shape.
- `crates/cairn-core/src/config/` for `pre_compact_recipe` and `pre_compact_safety_ratio`.
- `crates/cairn-core/src/verbs/assemble_hot/` for budget-aware pre-compaction assembly entrypoints, without adding transcript write side effects to the assembler itself.
- `crates/cairn-core/src/pipeline/capture_trace.rs` and `crates/cairn-cli/src/verbs/capture_trace.rs` for sequencing `assemble_hot`, snapshot persistence, and telemetry around hook-triggered trace capture.
- `crates/cairn-core/src/status/`, `crates/cairn-cli/src/verbs/status.rs`, and matching SDK / MCP status surfaces for the `cairn.mcp.v1.sensors.pre_compact` entry in `status.capabilities`.

## 13. Alternatives considered

### 13.1 Put snapshot writes inside `assemble_hot`

Rejected. `assemble_hot` is a read-oriented verb surface. Adding transcript persistence side effects there would blur contract boundaries and make testing / capability gating harder.

### 13.2 Return only a raw reinjection string

Rejected. A structured result gives the harness stable metadata for logs, assertions, and future compatibility without materially increasing complexity.

### 13.3 Reinjection only, no snapshotting

Rejected for this design because the brief already assigns transcript preservation semantics to `PreCompact`. Keeping both responsibilities on the same lifecycle boundary is the least surprising outcome.

## 14. Open implementation note

`assemble_hot` is still partly stubbed in the current tree. The `PreCompact` design assumes the implementation either:

- threads a real enough loader into `assemble_hot` to produce meaningful reinjection text, or
- introduces a narrow test seam that proves the orchestration and budgeting behavior even while the full hot-memory source loader remains incomplete.

That constraint should be called out explicitly in the implementation plan so the worker does not accidentally over-promise user-visible reinjection quality from a still-stubbed assembler.
