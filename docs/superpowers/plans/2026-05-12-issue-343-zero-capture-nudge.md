# Zero-Capture Session Nudge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a pure `cairn-core` zero-capture audit that decides whether a session with activity but no successful `ingest` or `capture_trace` writes should surface a retrospective reminder, plus config and reporting types for future consumer wiring.

**Architecture:** Keep this slice fully inside `cairn-core` and fully body-free. Add one focused domain module for the decision logic, extend `CairnConfig` with a dedicated reference-consumer toggle, and cover the behavior with unit tests before any implementation code.

**Tech Stack:** Rust 2024, `serde`, `thiserror`-free pure domain types, existing `cairn-core` unit tests via `cargo test`

---

### Task 1: Add failing zero-capture decision tests

**Files:**
- Create: `crates/cairn-core/src/domain/zero_capture.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Test: `crates/cairn-core/src/domain/zero_capture.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SessionId;

    fn input() -> ZeroCaptureAuditInput {
        ZeroCaptureAuditInput {
            session_id: SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .expect("invariant: valid session id"),
            activity_count: 3,
            successful_ingest_writes: 0,
            successful_capture_trace_writes: 0,
            nudges_enabled: true,
            reminder_allowed: true,
            trigger: ZeroCaptureTrigger::Stop,
        }
    }

    #[test]
    fn emit_nudge_for_activity_and_zero_writes() {
        let decision = decide_zero_capture_nudge(&input());
        assert!(matches!(decision, ZeroCaptureDecision::EmitNudge(_)));
    }

    #[test]
    fn suppress_when_any_ingest_write_present() {
        let mut input = input();
        input.successful_ingest_writes = 1;
        let decision = decide_zero_capture_nudge(&input);
        assert!(matches!(
            decision,
            ZeroCaptureDecision::NoNudge { reason: ZeroCaptureSuppression::WritesPresent }
        ));
    }

    #[test]
    fn suppress_when_any_capture_trace_write_present() {
        let mut input = input();
        input.successful_capture_trace_writes = 1;
        let decision = decide_zero_capture_nudge(&input);
        assert!(matches!(
            decision,
            ZeroCaptureDecision::NoNudge { reason: ZeroCaptureSuppression::WritesPresent }
        ));
    }

    #[test]
    fn suppress_when_disabled_in_config() {
        let mut input = input();
        input.nudges_enabled = false;
        let decision = decide_zero_capture_nudge(&input);
        assert!(matches!(
            decision,
            ZeroCaptureDecision::NoNudge { reason: ZeroCaptureSuppression::DisabledByConfig }
        ));
    }

    #[test]
    fn suppress_when_policy_blocked() {
        let mut input = input();
        input.reminder_allowed = false;
        let decision = decide_zero_capture_nudge(&input);
        assert!(matches!(
            decision,
            ZeroCaptureDecision::NoNudge { reason: ZeroCaptureSuppression::PolicyBlocked }
        ));
    }

    #[test]
    fn suppress_when_no_activity() {
        let mut input = input();
        input.activity_count = 0;
        let decision = decide_zero_capture_nudge(&input);
        assert!(matches!(
            decision,
            ZeroCaptureDecision::NoNudge { reason: ZeroCaptureSuppression::NoMeaningfulActivity }
        ));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p cairn-core zero_capture -- --nocapture`
Expected: FAIL because `zero_capture` module and decision types do not exist yet.

- [ ] **Step 3: Write minimal implementation**

```rust
pub fn decide_zero_capture_nudge(input: &ZeroCaptureAuditInput) -> ZeroCaptureDecision {
    if input.activity_count == 0 {
        return ZeroCaptureDecision::NoNudge {
            reason: ZeroCaptureSuppression::NoMeaningfulActivity,
        };
    }
    if !input.nudges_enabled {
        return ZeroCaptureDecision::NoNudge {
            reason: ZeroCaptureSuppression::DisabledByConfig,
        };
    }
    if !input.reminder_allowed {
        return ZeroCaptureDecision::NoNudge {
            reason: ZeroCaptureSuppression::PolicyBlocked,
        };
    }
    let successful_write_count =
        input.successful_ingest_writes + input.successful_capture_trace_writes;
    if successful_write_count > 0 {
        return ZeroCaptureDecision::NoNudge {
            reason: ZeroCaptureSuppression::WritesPresent,
        };
    }
    ZeroCaptureDecision::EmitNudge(ZeroCaptureNudge {
        session_id: input.session_id.clone(),
        activity_count: input.activity_count,
        successful_write_count,
        trigger: input.trigger,
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p cairn-core zero_capture -- --nocapture`
Expected: PASS for all six zero-capture decision tests.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/zero_capture.rs crates/cairn-core/src/domain/mod.rs
git commit -m "feat(core): add zero-capture decision model"
```

### Task 2: Add config toggle with failing tests first

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`
- Test: `crates/cairn-core/src/config/mod.rs`

- [ ] **Step 1: Write the failing config tests**

```rust
#[test]
fn default_zero_capture_nudge_is_enabled() {
    assert!(CairnConfig::default().reference_consumer.zero_capture_nudge.enabled);
}

#[test]
fn config_parses_reference_consumer_zero_capture_nudge_toggle() {
    let raw = r#"
        {
          "reference_consumer": {
            "zero_capture_nudge": { "enabled": false }
          }
        }
    "#;
    let config: CairnConfig = serde_json::from_str(raw).expect("config parses");
    assert!(!config.reference_consumer.zero_capture_nudge.enabled);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p cairn-core default_zero_capture_nudge_is_enabled config_parses_reference_consumer_zero_capture_nudge_toggle -- --nocapture`
Expected: FAIL because `reference_consumer` config does not exist yet.

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ReferenceConsumerConfig {
    pub zero_capture_nudge: ZeroCaptureNudgeConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ZeroCaptureNudgeConfig {
    pub enabled: bool,
}

impl Default for ZeroCaptureNudgeConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}
```

and add:

```rust
pub reference_consumer: ReferenceConsumerConfig,
```

to `CairnConfig`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p cairn-core default_zero_capture_nudge_is_enabled config_parses_reference_consumer_zero_capture_nudge_toggle -- --nocapture`
Expected: PASS for both config tests.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/config/mod.rs
git commit -m "feat(config): add zero-capture nudge toggle"
```

### Task 3: Add report type and verify the focused suite

**Files:**
- Modify: `crates/cairn-core/src/domain/zero_capture.rs`
- Modify: `docs/design/traceability.md`
- Test: `crates/cairn-core/src/domain/zero_capture.rs`

- [ ] **Step 1: Write the failing report test**

```rust
#[test]
fn emit_nudge_report_is_body_free_and_derived() {
    let decision = decide_zero_capture_nudge(&input());
    let report = ZeroCaptureReport::from_decision(&input(), &decision);
    assert_eq!(report.activity_count, 3);
    assert_eq!(report.successful_write_count, 0);
    assert_eq!(report.decision, ZeroCaptureDecisionCode::EmitNudge);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p cairn-core emit_nudge_report_is_body_free_and_derived -- --nocapture`
Expected: FAIL because `ZeroCaptureReport` and `ZeroCaptureDecisionCode` are not implemented yet.

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZeroCaptureDecisionCode {
    NoMeaningfulActivity,
    WritesPresent,
    DisabledByConfig,
    PolicyBlocked,
    EmitNudge,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ZeroCaptureReport {
    pub session_id: SessionId,
    pub activity_count: u64,
    pub successful_write_count: u64,
    pub decision: ZeroCaptureDecisionCode,
}
```

with a pure conversion helper:

```rust
impl ZeroCaptureReport {
    pub fn from_decision(
        input: &ZeroCaptureAuditInput,
        decision: &ZeroCaptureDecision,
    ) -> Self { /* map decision to code */ }
}
```

- [ ] **Step 4: Run focused verification**

Run: `cargo test -p cairn-core zero_capture -- --nocapture`
Expected: PASS for the zero-capture domain tests and config coverage.

Run: `cargo check -p cairn-core --locked`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/zero_capture.rs docs/design/traceability.md
git commit -m "feat(core): add zero-capture reporting metadata"
```

### Task 4: Run final verification for the slice

**Files:**
- Modify: none
- Test: existing workspace checks

- [ ] **Step 1: Run targeted crate tests**

Run: `cargo test -p cairn-core zero_capture -- --nocapture`
Expected: PASS

- [ ] **Step 2: Run crate-wide verification**

Run: `cargo test -p cairn-core --locked`
Expected: PASS

Run: `cargo check -p cairn-core --locked`
Expected: PASS

- [ ] **Step 3: Inspect diff**

Run: `git diff --stat HEAD~3..HEAD`
Expected: only `cairn-core` zero-capture files, config, and optional traceability doc touched.

- [ ] **Step 4: Commit any final tidy-ups**

```bash
git add crates/cairn-core/src/domain/zero_capture.rs crates/cairn-core/src/domain/mod.rs crates/cairn-core/src/config/mod.rs docs/design/traceability.md
git commit -m "test: finalize zero-capture slice verification"
```
