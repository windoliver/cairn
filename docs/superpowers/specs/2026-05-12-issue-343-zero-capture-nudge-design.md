# Issue #343 — Zero-Capture Session Nudge

**Status:** Draft
**Date:** 2026-05-12
**Branch:** `codex/issue-343-zero-capture-nudge`
**Brief sections:** §9.3 (five-hook lifecycle), §10.0 (ReflectionWorkflow), §19 (reference consumer)
**Issue:** [#343](https://github.com/windoliver/cairn/issues/343)
**Dependencies (open):** #102 (Claude Code hook map)
**Out of scope:** #79 (hook command handlers), full reference-consumer wiring, automatic durable writes

## 1. Goal

Implement the core decision logic for a "zero-capture session" reminder:
given session activity plus the set of successful Cairn writes, determine
whether Cairn should emit a retrospective-capture nudge, and expose enough
metadata for config gating and lint-style reporting.

This issue slice deliberately stops short of inventing a new hook command
surface. The result is a pure, testable audit module that the future
reference-consumer layer can call at `Stop` or the next safe hook point.

## 2. Non-goals

- Adding `cairn hook <name>` CLI support in this PR.
- Changing `capture_trace` import semantics or trace taxonomy.
- Persisting the nudge itself as a durable record.
- Auto-generating `ingest` or `capture_trace` writes from the nudge.
- Supporting harnesses beyond the v0.1 reference consumer.

## 3. Approach

Add a small session-audit domain in `cairn-core` that evaluates four inputs:

1. whether the session had meaningful activity,
2. how many successful Cairn writes occurred,
3. whether nudges are enabled for the reference consumer,
4. whether policy/consent allows a reminder to be surfaced.

The audit returns a typed decision:

- `NoNudge` when the session was inactive, already captured, disabled, or
  policy-gated.
- `EmitNudge` when the session had activity but zero successful `ingest` or
  `capture_trace` writes.

The returned payload is body-free. It carries only counters, decision reason,
and hook-timing metadata so the consumer can render its own reminder text
without Cairn persisting user content.

## 4. Core Model

Add a pure module under `crates/cairn-core/src/domain/` with:

```rust
pub enum ZeroCaptureDecision {
    NoNudge { reason: ZeroCaptureSuppression },
    EmitNudge(ZeroCaptureNudge),
}

pub enum ZeroCaptureSuppression {
    NoMeaningfulActivity,
    WritesPresent,
    DisabledByConfig,
    PolicyBlocked,
}

pub struct ZeroCaptureAuditInput {
    pub session_id: SessionId,
    pub activity_count: u64,
    pub successful_ingest_writes: u64,
    pub successful_capture_trace_writes: u64,
    pub nudges_enabled: bool,
    pub reminder_allowed: bool,
    pub trigger: ZeroCaptureTrigger,
}

pub enum ZeroCaptureTrigger {
    Stop,
    SafeHookPoint,
}

pub struct ZeroCaptureNudge {
    pub session_id: SessionId,
    pub activity_count: u64,
    pub successful_write_count: u64,
    pub trigger: ZeroCaptureTrigger,
}
```

`successful_write_count` is derived, not separately passed.

`meaningful activity` for this slice means `activity_count > 0`. The future
consumer integration can decide how to count activity; this PR only defines
how the decision behaves once that count is supplied.

## 5. Config

Extend `CairnConfig` with a narrowly-scoped reference-consumer block:

```rust
pub struct ReferenceConsumerConfig {
    pub zero_capture_nudge: ZeroCaptureNudgeConfig,
}

pub struct ZeroCaptureNudgeConfig {
    pub enabled: bool,
}
```

Default is `enabled = true` for the reference consumer path so the P0
acceptance behavior is on by default, while still allowing explicit opt-out.

This is intentionally separate from `sensors.hooks.enabled`: disabling hooks
turns off a sensor family, while disabling zero-capture nudges turns off only
this reminder behavior.

## 6. Reporting Surface

This PR does not add a new durable table or record kind. Instead it adds a
typed summary/report payload that a future `lint` or dogfood report layer can
consume:

```rust
pub struct ZeroCaptureReport {
    pub session_id: SessionId,
    pub activity_count: u64,
    pub successful_write_count: u64,
    pub decision: ZeroCaptureDecisionCode,
}
```

`ZeroCaptureDecisionCode` is a small serializable enum mirroring the decision
outcomes. The report remains body-free and contains no raw captured content.

## 7. Privacy And Consent

This slice preserves the invariants called out in issue #343 and AGENTS.md:

- No reminder text or transcript content is persisted by the audit.
- The decision input is numeric/session metadata only.
- Policy can suppress reminders completely via `reminder_allowed = false`.
- A reminder is advisory only; it never bypasses the normal `ingest` or
  `capture_trace` policy path.

## 8. Tests

Test-first in `cairn-core`:

- `emit_nudge_for_activity_and_zero_writes`
- `suppress_when_any_ingest_write_present`
- `suppress_when_any_capture_trace_write_present`
- `suppress_when_disabled_in_config`
- `suppress_when_policy_blocked`
- `suppress_when_no_activity`
- config serde/default test covering `reference_consumer.zero_capture_nudge`

No CLI or integration tests in this PR because the hook/consumer surface is
still tracked separately in #102.

## 9. File Plan

- `crates/cairn-core/src/domain/zero_capture.rs` — new pure decision types and
  evaluator
- `crates/cairn-core/src/domain/mod.rs` — export the new module
- `crates/cairn-core/src/config/mod.rs` — config structs + defaults + serde
  tests
- `docs/design/traceability.md` — add `#343` under the relevant sections if
  the implementation materially advances the mapped brief coverage

## 10. Risks And Follow-ups

- The biggest risk is accidentally baking harness-specific semantics into the
  core. This design avoids that by accepting counts and policy booleans rather
  than raw Claude Code payloads.
- A follow-up under #102 must decide where the activity count comes from and
  how the reminder is surfaced to the user at `Stop` or another safe hook.
- A later reporting issue can thread `ZeroCaptureReport` into `lint` or a
  dedicated dogfood report once the reference-consumer runtime exists.
