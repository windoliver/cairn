# Issue #128: Skillify Pipeline & SkillPack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the 5-stage Skillify pipeline state machine, 10 gate runners, SkillPack packaging with dependency metadata, and fail-closed enforcement at every boundary.

**Architecture:** Three-layer split — pure state machine + data types in `cairn-core`, async pipeline orchestration + gate runners in `cairn-workflows`, CLI surface in `cairn-cli`. The pipeline drives candidates through Extract → Author → Gate → Promote → HealthCheck stages. SkillPacks bundle multiple candidates into portable `.cairnpack` tar.gz archives.

**Tech Stack:** Rust 2024, `tokio`, `serde`, `sha2`, `thiserror`, `flate2` + `tar` (new deps), `insta` (snapshots), `tempfile` (tests).

**Spec:** `docs/superpowers/specs/2026-05-23-issue-128-skillify-pipeline-skillpack-design.md`

---

## File Map

### New files — cairn-core

| File | Responsibility |
|------|---------------|
| `crates/cairn-core/src/pipeline/skillify/spec.rs` | `SkillSpecDraft` data model + validation (STAGE 1 output) |
| `crates/cairn-core/src/pipeline/skillify/stage.rs` | `SkillifyStage` enum, `SkillifyPipelineState` pure state machine, `SkillifyStageError` |
| `crates/cairn-core/src/pipeline/skillify/pack.rs` | `SkillPackManifest`, `SkillPackEntry`, `SkillPackError`, validation |
| `crates/cairn-core/tests/skillify_stage.rs` | State machine transition tests |
| `crates/cairn-core/tests/skillify_pack.rs` | SkillPack validation tests |

### New files — cairn-workflows

| File | Responsibility |
|------|---------------|
| `crates/cairn-workflows/src/skillify/gate_runner.rs` | `GateRunner` trait, `GateRunContext`, `GateRunResult`, 10 runner structs |
| `crates/cairn-workflows/src/skillify/gate_registry.rs` | `GateRunnerRegistry` — ordered execution with dependency blocking |
| `crates/cairn-workflows/src/skillify/pipeline.rs` | `SkillifyPipeline` orchestrator, `SkillifyPipelineResult`, `SkillifyPipelineError` |
| `crates/cairn-workflows/src/skillify/packer.rs` | `SkillPackBuilder`, `SkillPackArchive`, pack/unpack functions |
| `crates/cairn-workflows/src/skillify/health.rs` | `HealthCheckRunner` — daily re-gate of promoted skills |
| `crates/cairn-workflows/tests/skillify_gate_runners.rs` | Individual gate runner tests |
| `crates/cairn-workflows/tests/skillify_pipeline.rs` | Pipeline orchestration tests |
| `crates/cairn-workflows/tests/skillify_packer.rs` | Pack/unpack round-trip tests |

### Modified files

| File | Change |
|------|--------|
| `crates/cairn-core/src/pipeline/skillify/mod.rs` | Add `pub mod spec; pub mod stage; pub mod pack;` + re-exports |
| `crates/cairn-core/src/pipeline/skillify/lint.rs` | Make `valid_relative_dir` `pub(crate)` so pack validation can reuse it |
| `crates/cairn-workflows/src/skillify/mod.rs` | Add `pub mod gate_runner; pub mod gate_registry; pub mod pipeline; pub mod packer; pub mod health;` + re-exports |
| `crates/cairn-workflows/src/skillify/handler.rs` | Refactor `run_once()` to delegate to `SkillifyPipeline::run()` |
| `Cargo.toml` | Add `flate2` and `tar` to workspace deps |
| `crates/cairn-workflows/Cargo.toml` | Add `flate2` and `tar` deps |

---

## Task 1: `SkillSpecDraft` data model

**Files:**
- Create: `crates/cairn-core/src/pipeline/skillify/spec.rs`
- Modify: `crates/cairn-core/src/pipeline/skillify/mod.rs`

- [ ] **Step 1: Write failing test for SkillSpecDraft validation**

Add inline `#[cfg(test)] mod tests` in `spec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn valid_draft() -> SkillSpecDraft {
        SkillSpecDraft {
            lane: "deploy.hotfix".to_owned(),
            slug: "deploy-hotfix".to_owned(),
            decision_tree: serde_json::json!({"root": "check_env"}),
            triggers: vec!["deploy hotfix".to_owned()],
            success_criteria: vec!["script exits 0".to_owned()],
            source_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
            requires: vec![],
            provides: vec!["deploy.hotfix".to_owned()],
        }
    }

    #[test]
    fn valid_draft_passes() {
        assert!(valid_draft().validate().is_ok());
    }

    #[test]
    fn empty_lane_rejected() {
        let mut draft = valid_draft();
        draft.lane = String::new();
        let err = draft.validate().unwrap_err();
        assert!(matches!(err, SkillSpecError::EmptyField { .. }));
    }

    #[test]
    fn unsafe_slug_rejected() {
        let mut draft = valid_draft();
        draft.slug = "../escape".to_owned();
        let err = draft.validate().unwrap_err();
        assert!(matches!(err, SkillSpecError::InvalidSlug { .. }));
    }

    #[test]
    fn empty_triggers_rejected() {
        let mut draft = valid_draft();
        draft.triggers.clear();
        let err = draft.validate().unwrap_err();
        assert!(matches!(err, SkillSpecError::EmptyField { .. }));
    }

    #[test]
    fn empty_source_refs_rejected() {
        let mut draft = valid_draft();
        draft.source_refs.clear();
        let err = draft.validate().unwrap_err();
        assert!(matches!(err, SkillSpecError::EmptyField { .. }));
    }

    #[test]
    fn serde_round_trip() {
        let draft = valid_draft();
        let json = serde_json::to_string(&draft).unwrap();
        let parsed: SkillSpecDraft = serde_json::from_str(&json).unwrap();
        assert_eq!(draft, parsed);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p cairn-core --no-fail-fast 2>&1 | tail -20`
Expected: compilation error — `SkillSpecDraft` not defined

- [ ] **Step 3: Implement `SkillSpecDraft` and validation**

```rust
//! Skillify spec draft — STAGE 1 extraction output.

use serde::{Deserialize, Serialize};

/// Error from [`SkillSpecDraft::validate`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillSpecError {
    /// A required field was empty.
    #[error("skill spec field `{field}` must not be empty")]
    EmptyField {
        /// Field name.
        field: &'static str,
    },
    /// Slug contained unsafe characters.
    #[error("skill spec slug `{slug}` is not a safe path token")]
    InvalidSlug {
        /// Rejected slug.
        slug: String,
    },
}

/// Extracted skill specification from a conversation trace (STAGE 1 output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillSpecDraft {
    /// Skill lane, e.g. `deploy.hotfix`.
    pub lane: String,
    /// Filesystem-safe slug.
    pub slug: String,
    /// Decision tree extracted from the trace.
    pub decision_tree: serde_json::Value,
    /// Natural-language triggers.
    pub triggers: Vec<String>,
    /// Criteria that made the trajectory successful.
    pub success_criteria: Vec<String>,
    /// Source record ids.
    pub source_refs: Vec<String>,
    /// Required capabilities.
    pub requires: Vec<String>,
    /// Capabilities this skill provides.
    pub provides: Vec<String>,
}

impl SkillSpecDraft {
    /// Validate required fields and slug safety.
    ///
    /// # Errors
    /// Returns [`SkillSpecError`] when a required field is empty or the slug
    /// contains unsafe characters.
    pub fn validate(&self) -> Result<(), SkillSpecError> {
        validate_not_empty("lane", &self.lane)?;
        validate_slug(&self.slug)?;
        validate_vec_not_empty("triggers", &self.triggers)?;
        validate_vec_not_empty("source_refs", &self.source_refs)?;
        Ok(())
    }
}

fn validate_not_empty(field: &'static str, value: &str) -> Result<(), SkillSpecError> {
    if value.trim().is_empty() {
        Err(SkillSpecError::EmptyField { field })
    } else {
        Ok(())
    }
}

fn validate_vec_not_empty(field: &'static str, value: &[String]) -> Result<(), SkillSpecError> {
    if value.is_empty() {
        Err(SkillSpecError::EmptyField { field })
    } else {
        Ok(())
    }
}

fn validate_slug(slug: &str) -> Result<(), SkillSpecError> {
    if slug.is_empty()
        || slug == "."
        || slug == ".."
        || slug.contains('/')
        || slug.contains('\\')
        || !slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(SkillSpecError::InvalidSlug {
            slug: slug.to_owned(),
        });
    }
    Ok(())
}
```

- [ ] **Step 4: Wire into mod.rs**

Add to `crates/cairn-core/src/pipeline/skillify/mod.rs`:

```rust
pub mod spec;

pub use spec::{SkillSpecDraft, SkillSpecError};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-core --no-fail-fast 2>&1 | tail -10`
Expected: all tests PASS

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/pipeline/skillify/spec.rs crates/cairn-core/src/pipeline/skillify/mod.rs
git commit -m "feat(core): add SkillSpecDraft data model and validation (#128)"
```

---

## Task 2: `SkillifyStage` pipeline state machine

**Files:**
- Create: `crates/cairn-core/src/pipeline/skillify/stage.rs`
- Create: `crates/cairn-core/tests/skillify_stage.rs`
- Modify: `crates/cairn-core/src/pipeline/skillify/mod.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/cairn-core/tests/skillify_stage.rs`:

```rust
#![allow(missing_docs)]

use cairn_core::pipeline::skillify::{
    SkillArtifactBundle, SkillArtifactKind, SkillSpecDraft, SkillifyGate,
    SkillifyGateStatus, SkillifyPipelineState, SkillifyStage, SkillifyStageError,
};

fn draft() -> SkillSpecDraft {
    SkillSpecDraft {
        lane: "deploy.hotfix".to_owned(),
        slug: "deploy-hotfix".to_owned(),
        decision_tree: serde_json::json!({"root": "check_env"}),
        triggers: vec!["deploy hotfix".to_owned()],
        success_criteria: vec!["script exits 0".to_owned()],
        source_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
        requires: vec![],
        provides: vec!["deploy.hotfix".to_owned()],
    }
}

fn passing_gates() -> Vec<SkillifyGate> {
    SkillArtifactKind::required()
        .iter()
        .map(|kind| SkillifyGate {
            name: kind.as_str().to_owned(),
            status: SkillifyGateStatus::Passed,
            message: None,
        })
        .collect()
}

fn valid_bundle() -> SkillArtifactBundle {
    use cairn_core::pipeline::skillify::SkillArtifact;
    SkillArtifactBundle {
        candidate_id: "skc_test".to_owned(),
        version: 1,
        artifacts: SkillArtifactKind::required()
            .iter()
            .map(|kind| SkillArtifact {
                kind: *kind,
                path: kind.default_relative_path("deploy-hotfix"),
                content_sha256: "sha256:aaaa".to_owned(),
                evidence_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
                status: "generated".to_owned(),
            })
            .collect(),
    }
}

#[test]
fn new_state_starts_at_extract() {
    let state = SkillifyPipelineState::new("skc_test".to_owned());
    assert_eq!(state.stage(), SkillifyStage::Extract);
}

#[test]
fn happy_path_extract_to_promote() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());

    state.advance_to_author(draft()).unwrap();
    assert_eq!(state.stage(), SkillifyStage::Author);

    state.advance_to_gate(valid_bundle()).unwrap();
    assert_eq!(state.stage(), SkillifyStage::Gate);

    for gate in passing_gates() {
        state.record_gate(gate);
    }

    state.advance_to_promote().unwrap();
    assert_eq!(state.stage(), SkillifyStage::Promote);

    state
        .advance_to_health("plan_ref_001".to_owned())
        .unwrap();
    assert_eq!(state.stage(), SkillifyStage::HealthCheck);
}

#[test]
fn cannot_skip_from_extract_to_gate() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    let err = state.advance_to_gate(valid_bundle()).unwrap_err();
    assert!(matches!(err, SkillifyStageError::InvalidTransition { .. }));
}

#[test]
fn cannot_skip_from_extract_to_promote() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    let err = state.advance_to_promote().unwrap_err();
    assert!(matches!(err, SkillifyStageError::InvalidTransition { .. }));
}

#[test]
fn promote_fails_without_gates() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    state.advance_to_author(draft()).unwrap();
    state.advance_to_gate(valid_bundle()).unwrap();
    let err = state.advance_to_promote().unwrap_err();
    assert!(matches!(err, SkillifyStageError::GatesNotSatisfied { .. }));
}

#[test]
fn promote_fails_with_failing_gate() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    state.advance_to_author(draft()).unwrap();
    state.advance_to_gate(valid_bundle()).unwrap();

    let mut gates = passing_gates();
    gates[0].status = SkillifyGateStatus::Failed;
    for gate in gates {
        state.record_gate(gate);
    }

    let err = state.advance_to_promote().unwrap_err();
    assert!(matches!(err, SkillifyStageError::GatesNotSatisfied { .. }));
}

#[test]
fn fail_from_any_non_terminal() {
    for start_stage in [
        SkillifyStage::Extract,
        SkillifyStage::Author,
        SkillifyStage::Gate,
    ] {
        let mut state = SkillifyPipelineState::new("skc_test".to_owned());
        match start_stage {
            SkillifyStage::Author => {
                state.advance_to_author(draft()).unwrap();
            }
            SkillifyStage::Gate => {
                state.advance_to_author(draft()).unwrap();
                state.advance_to_gate(valid_bundle()).unwrap();
            }
            _ => {}
        }
        state.fail("test failure".to_owned()).unwrap();
        assert_eq!(state.stage(), SkillifyStage::Failed);
    }
}

#[test]
fn block_from_any_non_terminal() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    state.block("no llm".to_owned()).unwrap();
    assert_eq!(state.stage(), SkillifyStage::Blocked);
}

#[test]
fn cannot_transition_from_failed() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    state.fail("done".to_owned()).unwrap();
    let err = state.advance_to_author(draft()).unwrap_err();
    assert!(matches!(err, SkillifyStageError::InvalidTransition { .. }));
}

#[test]
fn cannot_transition_from_blocked() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    state.block("done".to_owned()).unwrap();
    let err = state.fail("double".to_owned()).unwrap_err();
    assert!(matches!(err, SkillifyStageError::InvalidTransition { .. }));
}

#[test]
fn serde_stage_round_trip() {
    let json = serde_json::to_string(&SkillifyStage::Gate).unwrap();
    assert_eq!(json, "\"gate\"");
    let parsed: SkillifyStage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, SkillifyStage::Gate);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p cairn-core --no-fail-fast 2>&1 | tail -20`
Expected: compilation error — `SkillifyPipelineState`, `SkillifyStage`, `SkillifyStageError` not defined

- [ ] **Step 3: Implement `stage.rs`**

Create `crates/cairn-core/src/pipeline/skillify/stage.rs`:

```rust
//! Skillify pipeline state machine (brief §11.b stages 1-5).
//!
//! Pure state transitions. No I/O, no async.

use serde::{Deserialize, Serialize};

use super::gate::{SkillifyGate, SkillifyGateReport, SkillifyGateStatus};
use super::artifact::{SkillArtifactBundle, SkillArtifactKind};
use super::spec::SkillSpecDraft;

/// Pipeline stage for a skillify candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyStage {
    /// STAGE 1: extracting decision tree from trace.
    Extract,
    /// STAGE 2: LLM authoring the 10 artifacts.
    Author,
    /// STAGE 3: running promotion gates.
    Gate,
    /// STAGE 4: candidate promoted.
    Promote,
    /// STAGE 5: post-promotion health check.
    HealthCheck,
    /// Terminal: pipeline failed.
    Failed,
    /// Terminal: pipeline blocked (e.g. no LLM).
    Blocked,
}

impl SkillifyStage {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Failed | Self::Blocked)
    }
}

/// Transition or validation error for the pipeline state machine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SkillifyStageError {
    /// Attempted an illegal stage transition.
    #[error("invalid skillify transition {from:?} -> {to:?}")]
    InvalidTransition {
        /// Current stage.
        from: SkillifyStage,
        /// Requested stage.
        to: SkillifyStage,
    },
    /// Promotion gates not satisfied.
    #[error("skillify promotion blocked: missing={missing:?} failed={failed:?}")]
    GatesNotSatisfied {
        /// Required gate names with no result.
        missing: Vec<String>,
        /// Required gate names with a non-passing result.
        failed: Vec<String>,
    },
    /// Required data not set for this transition.
    #[error("skillify missing precondition: {field}")]
    MissingPrecondition {
        /// Field name.
        field: String,
    },
}

/// In-memory pipeline state for one skillify candidate.
#[derive(Debug, Clone)]
pub struct SkillifyPipelineState {
    candidate_id: String,
    stage: SkillifyStage,
    spec: Option<SkillSpecDraft>,
    bundle: Option<SkillArtifactBundle>,
    gate_report: SkillifyGateReport,
    promotion_plan_ref: Option<String>,
    failure_reason: Option<String>,
}

impl SkillifyPipelineState {
    /// Create a new pipeline state at the Extract stage.
    #[must_use]
    pub fn new(candidate_id: String) -> Self {
        Self {
            candidate_id,
            stage: SkillifyStage::Extract,
            spec: None,
            bundle: None,
            gate_report: SkillifyGateReport {
                candidate_id: String::new(),
                gates: Vec::new(),
            },
            promotion_plan_ref: None,
            failure_reason: None,
        }
    }

    /// Current pipeline stage.
    #[must_use]
    pub const fn stage(&self) -> SkillifyStage {
        self.stage
    }

    /// Candidate id.
    #[must_use]
    pub fn candidate_id(&self) -> &str {
        &self.candidate_id
    }

    /// Spec draft, if extraction completed.
    #[must_use]
    pub fn spec(&self) -> Option<&SkillSpecDraft> {
        self.spec.as_ref()
    }

    /// Artifact bundle, if authoring completed.
    #[must_use]
    pub fn bundle(&self) -> Option<&SkillArtifactBundle> {
        self.bundle.as_ref()
    }

    /// Current gate report.
    #[must_use]
    pub const fn gate_report(&self) -> &SkillifyGateReport {
        &self.gate_report
    }

    /// Failure reason, if failed or blocked.
    #[must_use]
    pub fn failure_reason(&self) -> Option<&str> {
        self.failure_reason.as_deref()
    }

    /// Advance from Extract to Author with a validated spec.
    ///
    /// # Errors
    /// Returns [`SkillifyStageError::InvalidTransition`] if not at Extract.
    pub fn advance_to_author(
        &mut self,
        spec: SkillSpecDraft,
    ) -> Result<(), SkillifyStageError> {
        self.require_stage(SkillifyStage::Extract, SkillifyStage::Author)?;
        self.spec = Some(spec);
        self.stage = SkillifyStage::Author;
        Ok(())
    }

    /// Advance from Author to Gate with a validated bundle.
    ///
    /// # Errors
    /// Returns [`SkillifyStageError::InvalidTransition`] if not at Author.
    pub fn advance_to_gate(
        &mut self,
        bundle: SkillArtifactBundle,
    ) -> Result<(), SkillifyStageError> {
        self.require_stage(SkillifyStage::Author, SkillifyStage::Gate)?;
        self.gate_report.candidate_id = self.candidate_id.clone();
        self.bundle = Some(bundle);
        self.stage = SkillifyStage::Gate;
        Ok(())
    }

    /// Record one gate result during the Gate stage.
    pub fn record_gate(&mut self, gate: SkillifyGate) {
        if let Some(existing) = self
            .gate_report
            .gates
            .iter_mut()
            .find(|g| g.name == gate.name)
        {
            *existing = gate;
        } else {
            self.gate_report.gates.push(gate);
        }
    }

    /// Advance from Gate to Promote.
    ///
    /// # Errors
    /// Returns [`SkillifyStageError::GatesNotSatisfied`] if any required gate
    /// is missing or failed. Returns [`SkillifyStageError::InvalidTransition`]
    /// if not at Gate.
    pub fn advance_to_promote(&mut self) -> Result<(), SkillifyStageError> {
        self.require_stage(SkillifyStage::Gate, SkillifyStage::Promote)?;

        let required = SkillArtifactKind::required();
        let mut missing = Vec::new();
        let mut failed = Vec::new();

        for kind in required {
            let name = kind.as_str();
            match self.gate_report.gates.iter().find(|g| g.name == name) {
                Some(g) if g.status == SkillifyGateStatus::Passed => {}
                Some(_) => failed.push(name.to_owned()),
                None => missing.push(name.to_owned()),
            }
        }

        if !missing.is_empty() || !failed.is_empty() {
            return Err(SkillifyStageError::GatesNotSatisfied { missing, failed });
        }

        self.stage = SkillifyStage::Promote;
        Ok(())
    }

    /// Advance from Promote to HealthCheck.
    ///
    /// # Errors
    /// Returns [`SkillifyStageError::InvalidTransition`] if not at Promote.
    pub fn advance_to_health(
        &mut self,
        plan_ref: String,
    ) -> Result<(), SkillifyStageError> {
        self.require_stage(SkillifyStage::Promote, SkillifyStage::HealthCheck)?;
        self.promotion_plan_ref = Some(plan_ref);
        self.stage = SkillifyStage::HealthCheck;
        Ok(())
    }

    /// Transition to Failed from any non-terminal stage.
    ///
    /// # Errors
    /// Returns [`SkillifyStageError::InvalidTransition`] if already terminal.
    pub fn fail(&mut self, reason: String) -> Result<(), SkillifyStageError> {
        if self.stage.is_terminal() {
            return Err(SkillifyStageError::InvalidTransition {
                from: self.stage,
                to: SkillifyStage::Failed,
            });
        }
        self.failure_reason = Some(reason);
        self.stage = SkillifyStage::Failed;
        Ok(())
    }

    /// Transition to Blocked from any non-terminal stage.
    ///
    /// # Errors
    /// Returns [`SkillifyStageError::InvalidTransition`] if already terminal.
    pub fn block(&mut self, reason: String) -> Result<(), SkillifyStageError> {
        if self.stage.is_terminal() {
            return Err(SkillifyStageError::InvalidTransition {
                from: self.stage,
                to: SkillifyStage::Blocked,
            });
        }
        self.failure_reason = Some(reason);
        self.stage = SkillifyStage::Blocked;
        Ok(())
    }

    fn require_stage(
        &self,
        expected: SkillifyStage,
        to: SkillifyStage,
    ) -> Result<(), SkillifyStageError> {
        if self.stage != expected {
            return Err(SkillifyStageError::InvalidTransition {
                from: self.stage,
                to,
            });
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Wire into mod.rs**

Add to `crates/cairn-core/src/pipeline/skillify/mod.rs`:

```rust
pub mod stage;

pub use stage::{SkillifyPipelineState, SkillifyStage, SkillifyStageError};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-core --no-fail-fast 2>&1 | tail -10`
Expected: all tests PASS including the new `skillify_stage` tests

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/pipeline/skillify/stage.rs crates/cairn-core/src/pipeline/skillify/mod.rs crates/cairn-core/tests/skillify_stage.rs
git commit -m "feat(core): add SkillifyPipelineState state machine (#128)"
```

---

## Task 3: `SkillPackManifest` data model

**Files:**
- Create: `crates/cairn-core/src/pipeline/skillify/pack.rs`
- Create: `crates/cairn-core/tests/skillify_pack.rs`
- Modify: `crates/cairn-core/src/pipeline/skillify/mod.rs`
- Modify: `crates/cairn-core/src/pipeline/skillify/lint.rs`

- [ ] **Step 1: Make `valid_relative_dir` pub(crate)**

In `crates/cairn-core/src/pipeline/skillify/lint.rs`, change:

```rust
fn valid_relative_dir(value: &str) -> bool {
```
to:
```rust
pub(crate) fn valid_relative_dir(value: &str) -> bool {
```

- [ ] **Step 2: Write failing tests**

Create `crates/cairn-core/tests/skillify_pack.rs`:

```rust
#![allow(missing_docs)]

use cairn_core::pipeline::skillify::{
    SkillPackEntry, SkillPackError, SkillPackManifest,
};

fn valid_entry(candidate_id: &str, lane: &str, slug: &str) -> SkillPackEntry {
    SkillPackEntry {
        candidate_id: candidate_id.to_owned(),
        lane: lane.to_owned(),
        slug: slug.to_owned(),
        bundle_version: 1,
        artifact_sha256: "sha256:aaaa".to_owned(),
    }
}

fn valid_manifest() -> SkillPackManifest {
    SkillPackManifest {
        pack_id: "skp_test".to_owned(),
        name: "test-pack".to_owned(),
        version: "0.1.0".to_owned(),
        cairn_compat: ">=0.1.0".to_owned(),
        description: "A test skill pack".to_owned(),
        skills: vec![
            valid_entry("skc_a", "deploy.hotfix", "deploy-hotfix"),
            valid_entry("skc_b", "test.smoke", "test-smoke"),
        ],
        requires: vec![],
        provides: vec!["deploy.hotfix".to_owned(), "test.smoke".to_owned()],
        content_sha256: "sha256:bbbb".to_owned(),
    }
}

#[test]
fn valid_manifest_passes() {
    assert!(valid_manifest().validate("0.1.0").is_ok());
}

#[test]
fn duplicate_lane_rejected() {
    let mut manifest = valid_manifest();
    manifest.skills[1].lane = "deploy.hotfix".to_owned();
    let err = manifest.validate("0.1.0").unwrap_err();
    assert!(matches!(err, SkillPackError::DuplicateLane { .. }));
}

#[test]
fn incompatible_cairn_version_rejected() {
    let manifest = valid_manifest();
    let err = manifest.validate("0.0.1").unwrap_err();
    assert!(matches!(err, SkillPackError::IncompatibleCairn { .. }));
}

#[test]
fn missing_dependency_rejected() {
    let mut manifest = valid_manifest();
    manifest.requires = vec!["database.backup".to_owned()];
    let err = manifest.validate("0.1.0").unwrap_err();
    assert!(matches!(err, SkillPackError::DependencyMissing { .. }));
}

#[test]
fn dependency_satisfied_by_provides() {
    let mut manifest = valid_manifest();
    manifest.requires = vec!["deploy.hotfix".to_owned()];
    assert!(manifest.validate("0.1.0").is_ok());
}

#[test]
fn empty_name_rejected() {
    let mut manifest = valid_manifest();
    manifest.name = String::new();
    let err = manifest.validate("0.1.0").unwrap_err();
    assert!(matches!(err, SkillPackError::InvalidName { .. }));
}

#[test]
fn name_with_special_chars_rejected() {
    let mut manifest = valid_manifest();
    manifest.name = "test/../pack".to_owned();
    let err = manifest.validate("0.1.0").unwrap_err();
    assert!(matches!(err, SkillPackError::InvalidName { .. }));
}

#[test]
fn pack_id_derivation_is_deterministic() {
    let id1 = SkillPackManifest::derive_pack_id("test-pack", "0.1.0", &["skc_a", "skc_b"]);
    let id2 = SkillPackManifest::derive_pack_id("test-pack", "0.1.0", &["skc_b", "skc_a"]);
    assert_eq!(id1, id2);
    assert!(id1.starts_with("skp_"));
}

#[test]
fn serde_round_trip() {
    let manifest = valid_manifest();
    let json = serde_json::to_string(&manifest).unwrap();
    let parsed: SkillPackManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(manifest, parsed);
}

#[test]
fn higher_cairn_version_passes() {
    let manifest = valid_manifest();
    assert!(manifest.validate("1.0.0").is_ok());
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo nextest run -p cairn-core --no-fail-fast 2>&1 | tail -20`
Expected: compilation error — `SkillPackManifest` not defined

- [ ] **Step 4: Implement `pack.rs`**

Create `crates/cairn-core/src/pipeline/skillify/pack.rs`:

```rust
//! SkillPack manifest and validation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// One skill entry in a SkillPack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackEntry {
    /// Candidate id.
    pub candidate_id: String,
    /// Skill lane.
    pub lane: String,
    /// Filesystem-safe slug.
    pub slug: String,
    /// Bundle schema version.
    pub bundle_version: u32,
    /// SHA-256 digest of this skill's bundle.
    pub artifact_sha256: String,
}

/// SkillPack manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackManifest {
    /// Deterministic pack id.
    pub pack_id: String,
    /// Human-readable pack name.
    pub name: String,
    /// Semver pack version.
    pub version: String,
    /// Minimum Cairn version required, e.g. `>=0.1.0`.
    pub cairn_compat: String,
    /// Pack description.
    pub description: String,
    /// Skills in this pack.
    pub skills: Vec<SkillPackEntry>,
    /// Aggregated dependencies.
    pub requires: Vec<String>,
    /// Aggregated capabilities.
    pub provides: Vec<String>,
    /// SHA-256 digest of the packed archive.
    pub content_sha256: String,
}

/// SkillPack validation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillPackError {
    /// Pack skill not found in archive.
    #[error("pack skill `{candidate_id}` not found in archive")]
    MissingSkill {
        /// Candidate id.
        candidate_id: String,
    },
    /// Duplicate lane in pack.
    #[error("duplicate lane `{lane}` in pack")]
    DuplicateLane {
        /// Duplicated lane.
        lane: String,
    },
    /// Cairn version incompatibility.
    #[error("pack requires Cairn {required} but running {running}")]
    IncompatibleCairn {
        /// Required version string.
        required: String,
        /// Running version string.
        running: String,
    },
    /// Unsatisfied dependency.
    #[error("dependency `{dep}` not provided by any skill in pack")]
    DependencyMissing {
        /// Missing dependency.
        dep: String,
    },
    /// Content integrity check failed.
    #[error("content integrity check failed: expected {expected}, got {actual}")]
    IntegrityFailure {
        /// Expected digest.
        expected: String,
        /// Actual digest.
        actual: String,
    },
    /// Invalid pack name.
    #[error("invalid pack name: {reason}")]
    InvalidName {
        /// Rejection reason.
        reason: String,
    },
}

impl SkillPackManifest {
    /// Derive a deterministic pack id from name, version, and candidate ids.
    #[must_use]
    pub fn derive_pack_id(name: &str, version: &str, candidate_ids: &[&str]) -> String {
        let mut sorted = candidate_ids.to_vec();
        sorted.sort();
        let mut hasher = Sha256::new();
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(version.as_bytes());
        hasher.update(b"\0");
        for id in sorted {
            hasher.update(id.as_bytes());
            hasher.update(b"\0");
        }
        format!("skp_{:x}", hasher.finalize())
    }

    /// Validate the manifest against the running Cairn version.
    ///
    /// # Errors
    /// Returns [`SkillPackError`] on validation failure.
    pub fn validate(&self, cairn_version: &str) -> Result<(), SkillPackError> {
        self.validate_name()?;
        self.validate_no_duplicate_lanes()?;
        self.validate_cairn_compat(cairn_version)?;
        self.validate_dependencies()?;
        Ok(())
    }

    fn validate_name(&self) -> Result<(), SkillPackError> {
        if self.name.is_empty() {
            return Err(SkillPackError::InvalidName {
                reason: "name must not be empty".to_owned(),
            });
        }
        if !self
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(SkillPackError::InvalidName {
                reason: format!(
                    "name `{}` contains invalid characters (only alphanumeric, hyphens, underscores)",
                    self.name
                ),
            });
        }
        Ok(())
    }

    fn validate_no_duplicate_lanes(&self) -> Result<(), SkillPackError> {
        let mut seen = BTreeSet::new();
        for entry in &self.skills {
            if !seen.insert(&entry.lane) {
                return Err(SkillPackError::DuplicateLane {
                    lane: entry.lane.clone(),
                });
            }
        }
        Ok(())
    }

    fn validate_cairn_compat(&self, cairn_version: &str) -> Result<(), SkillPackError> {
        let required = self
            .cairn_compat
            .strip_prefix(">=")
            .unwrap_or(&self.cairn_compat);
        if !version_gte(cairn_version, required) {
            return Err(SkillPackError::IncompatibleCairn {
                required: self.cairn_compat.clone(),
                running: cairn_version.to_owned(),
            });
        }
        Ok(())
    }

    fn validate_dependencies(&self) -> Result<(), SkillPackError> {
        let provided: BTreeSet<&str> = self.provides.iter().map(String::as_str).collect();
        for dep in &self.requires {
            if !provided.contains(dep.as_str()) {
                return Err(SkillPackError::DependencyMissing {
                    dep: dep.clone(),
                });
            }
        }
        Ok(())
    }
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.');
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

fn version_gte(running: &str, required: &str) -> bool {
    match (parse_version(running), parse_version(required)) {
        (Some(r), Some(q)) => r >= q,
        _ => false,
    }
}
```

- [ ] **Step 5: Wire into mod.rs**

Add to `crates/cairn-core/src/pipeline/skillify/mod.rs`:

```rust
pub mod pack;

pub use pack::{SkillPackEntry, SkillPackError, SkillPackManifest};
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-core --no-fail-fast 2>&1 | tail -10`
Expected: all tests PASS

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/pipeline/skillify/pack.rs crates/cairn-core/src/pipeline/skillify/mod.rs crates/cairn-core/src/pipeline/skillify/lint.rs crates/cairn-core/tests/skillify_pack.rs
git commit -m "feat(core): add SkillPackManifest data model with validation (#128)"
```

---

## Task 4: `GateRunner` trait and `GateRunResult`

**Files:**
- Create: `crates/cairn-workflows/src/skillify/gate_runner.rs`
- Modify: `crates/cairn-workflows/src/skillify/mod.rs`

- [ ] **Step 1: Write the trait and result types**

Create `crates/cairn-workflows/src/skillify/gate_runner.rs`:

```rust
//! Gate runner trait and result types for Skillify pipeline gates.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cairn_core::contract::llm_provider::LLMProvider;
use cairn_core::pipeline::skillify::{
    SkillArtifactBundle, SkillArtifactKind, SkillLintSnapshot, SkillifyGate, SkillifyGateStatus,
};

use super::materialize::AuthoredSkillBundle;

/// Context passed to each gate runner.
pub struct GateRunContext<'a> {
    /// Vault root path.
    pub vault_root: &'a Path,
    /// Stable candidate id.
    pub candidate_id: &'a str,
    /// Candidate directory on disk.
    pub candidate_dir: PathBuf,
    /// Validated artifact bundle.
    pub bundle: &'a SkillArtifactBundle,
    /// Raw authored content.
    pub authored: &'a AuthoredSkillBundle,
    /// Optional LLM provider for eval gates.
    pub llm: Option<&'a dyn LLMProvider>,
    /// Current skill lint snapshot for DRY/resolvable checks.
    pub snapshot: &'a SkillLintSnapshot,
}

/// Result from one gate runner execution.
#[derive(Debug, Clone)]
pub struct GateRunResult {
    /// Artifact kind this gate evaluates.
    pub kind: SkillArtifactKind,
    /// Gate verdict.
    pub status: SkillifyGateStatus,
    /// Human-readable detail.
    pub message: Option<String>,
    /// Evidence references.
    pub evidence_refs: Vec<String>,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
}

impl GateRunResult {
    /// Convert to a core `SkillifyGate` for state machine recording.
    #[must_use]
    pub fn into_gate(self) -> SkillifyGate {
        SkillifyGate {
            name: self.kind.as_str().to_owned(),
            status: self.status,
            message: self.message,
        }
    }

    /// Create a passing result.
    #[must_use]
    pub fn passed(kind: SkillArtifactKind, duration_ms: u64) -> Self {
        Self {
            kind,
            status: SkillifyGateStatus::Passed,
            message: None,
            evidence_refs: Vec::new(),
            duration_ms,
        }
    }

    /// Create a failing result.
    #[must_use]
    pub fn failed(kind: SkillArtifactKind, message: String, duration_ms: u64) -> Self {
        Self {
            kind,
            status: SkillifyGateStatus::Failed,
            message: Some(message),
            evidence_refs: Vec::new(),
            duration_ms,
        }
    }

    /// Create a blocked result.
    #[must_use]
    pub fn blocked(kind: SkillArtifactKind, message: String) -> Self {
        Self {
            kind,
            status: SkillifyGateStatus::Blocked,
            message: Some(message),
            evidence_refs: Vec::new(),
            duration_ms: 0,
        }
    }
}

/// Measures wall-clock duration for a gate run.
pub struct GateTimer {
    start: Instant,
}

impl GateTimer {
    /// Start a new timer.
    #[must_use]
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Elapsed milliseconds since start.
    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
    }
}

/// Trait for individual gate runner implementations.
#[async_trait::async_trait]
pub trait GateRunner: Send + Sync {
    /// Which artifact kind this runner validates.
    fn artifact_kind(&self) -> SkillArtifactKind;

    /// Execute the gate check.
    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult;
}
```

- [ ] **Step 2: Wire into mod.rs**

Add to `crates/cairn-workflows/src/skillify/mod.rs`:

```rust
pub mod gate_runner;
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p cairn-workflows 2>&1 | tail -10`
Expected: compiles cleanly

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-workflows/src/skillify/gate_runner.rs crates/cairn-workflows/src/skillify/mod.rs
git commit -m "feat(workflows): add GateRunner trait and GateRunResult types (#128)"
```

---

## Task 5: 10 gate runner implementations

**Files:**
- Modify: `crates/cairn-workflows/src/skillify/gate_runner.rs` (add runner structs at end)
- Create: `crates/cairn-workflows/tests/skillify_gate_runners.rs`

- [ ] **Step 1: Write tests for the 4 pure-validation runners**

Create `crates/cairn-workflows/tests/skillify_gate_runners.rs`:

```rust
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::pipeline::skillify::{
    SkillArtifact, SkillArtifactBundle, SkillArtifactKind, SkillLintSkill, SkillLintSnapshot,
    SkillifyGateStatus,
};
use cairn_workflows::skillify::gate_runner::{GateRunContext, GateRunner};
use cairn_workflows::skillify::gate_runner::{
    CheckResolvableAndDryRunner, DeterministicScriptRunner, FilingRulesRunner,
    IntegrationTestRunner, LlmEvalRunner, ResolverEvalRunner, ResolverTriggerRunner,
    SkillContractRunner, UnitTestRunner, E2eSmokeRunner,
};
use cairn_workflows::skillify::materialize::AuthoredSkillBundle;
use serde_json::json;
use tempfile::TempDir;

fn authored(slug: &str) -> AuthoredSkillBundle {
    AuthoredSkillBundle {
        lane: "deploy.hotfix".to_owned(),
        slug: slug.to_owned(),
        skill_markdown: format!(
            "---\nname: {slug}\nlane: deploy.hotfix\ntriggers:\n  - deploy hotfix\nuses: scripts/{slug}.sh\nfiles_to: wiki/summaries/\n---\nRun the script."
        ),
        script: format!("#!/usr/bin/env bash\nset -euo pipefail\necho {slug}\n"),
        unit_tests: json!({"cases": [{"input": "", "expected_stdout": format!("{slug}\n"), "timeout_ms": 5000}]}),
        integration_tests: json!({"cases": [{"input": "", "expected_stdout": format!("{slug}\n"), "timeout_ms": 10000}]}),
        llm_evals: json!({"rubric": [{"prompt": "deploy hotfix", "expected_behavior": "calls the script", "scoring_criteria": "script invoked"}]}),
        resolver_triggers: json!(["deploy hotfix"]),
        resolver_eval: json!({"intents": [{"intent": "deploy hotfix", "expected_lane": "deploy.hotfix"}]}),
        smoke: json!({"cases": [{"trigger_phrase": "deploy hotfix", "expected_output": format!("{slug}\n")}]}),
        filing_rules: json!({"files_to": "wiki/summaries/"}),
    }
}

fn bundle(slug: &str) -> SkillArtifactBundle {
    SkillArtifactBundle {
        candidate_id: "skc_test".to_owned(),
        version: 1,
        artifacts: SkillArtifactKind::required()
            .iter()
            .map(|kind| SkillArtifact {
                kind: *kind,
                path: kind.default_relative_path(slug),
                content_sha256: "sha256:aaaa".to_owned(),
                evidence_refs: vec![],
                status: "generated".to_owned(),
            })
            .collect(),
    }
}

fn empty_snapshot() -> SkillLintSnapshot {
    SkillLintSnapshot { skills: vec![] }
}

fn materialize_script(dir: &std::path::Path, slug: &str, content: &str) {
    let scripts_dir = dir.join("bundle/scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    let script_path = scripts_dir.join(format!("{slug}.sh"));
    std::fs::write(&script_path, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

// -- SkillContractRunner --

#[tokio::test]
async fn skill_contract_passes_valid_markdown() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = SkillContractRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Passed);
}

#[tokio::test]
async fn skill_contract_fails_missing_lane() {
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    a.skill_markdown = "---\nname: test\n---\nNo lane.".to_owned();
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = SkillContractRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    assert!(result.message.unwrap().contains("lane"));
}

// -- DeterministicScriptRunner --

#[tokio::test]
async fn script_runner_passes_valid_script() {
    let temp = TempDir::new().unwrap();
    materialize_script(temp.path(), "deploy-hotfix", "#!/usr/bin/env bash\necho ok\n");
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = DeterministicScriptRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Passed);
}

#[tokio::test]
async fn script_runner_fails_missing_shebang() {
    let temp = TempDir::new().unwrap();
    materialize_script(temp.path(), "deploy-hotfix", "echo no shebang\n");
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = DeterministicScriptRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
}

// -- FilingRulesRunner --

#[tokio::test]
async fn filing_rules_passes_valid_path() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = FilingRulesRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Passed);
}

#[tokio::test]
async fn filing_rules_fails_absolute_path() {
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    a.filing_rules = json!({"files_to": "/etc/passwd/"});
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = FilingRulesRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
}

// -- ResolverTriggerRunner --

#[tokio::test]
async fn resolver_trigger_passes_valid_triggers() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = ResolverTriggerRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Passed);
}

#[tokio::test]
async fn resolver_trigger_fails_collision_with_snapshot() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let snapshot = SkillLintSnapshot {
        skills: vec![SkillLintSkill {
            skill_id: "existing".to_owned(),
            lane: "other.lane".to_owned(),
            path: "skills/skill_existing.md".to_owned(),
            uses: None,
            resolver_triggers: vec!["deploy hotfix".to_owned()],
            files_to: Some("wiki/".to_owned()),
            gate_report_passed: true,
            rollback_version_count: 1,
            existing_paths: vec![],
        }],
    };
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &snapshot,
    };
    let result = ResolverTriggerRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
}

// -- CheckResolvableAndDryRunner --

#[tokio::test]
async fn check_resolvable_passes_no_conflicts() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = CheckResolvableAndDryRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Passed);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p cairn-workflows --no-fail-fast 2>&1 | tail -20`
Expected: compilation error — runner structs not defined

- [ ] **Step 3: Implement the 10 runner structs**

Append to `crates/cairn-workflows/src/skillify/gate_runner.rs`:

```rust
// -- Runner implementations --

/// Gate 1: Validates skill contract markdown frontmatter.
pub struct SkillContractRunner;

#[async_trait::async_trait]
impl GateRunner for SkillContractRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::SkillContract
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let md = &ctx.authored.skill_markdown;
        let mut missing = Vec::new();

        if !md.contains("lane:") {
            missing.push("lane");
        }
        if !md.contains("triggers:") && !md.contains("triggers:") {
            missing.push("triggers");
        }
        if !md.contains("uses:") {
            missing.push("uses");
        }
        if !md.contains("files_to:") {
            missing.push("files_to");
        }

        if missing.is_empty() {
            GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
        } else {
            GateRunResult::failed(
                self.artifact_kind(),
                format!("skill contract missing required fields: {}", missing.join(", ")),
                timer.elapsed_ms(),
            )
        }
    }
}

/// Gate 2: Validates deterministic script exists and has a shebang.
pub struct DeterministicScriptRunner;

#[async_trait::async_trait]
impl GateRunner for DeterministicScriptRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::DeterministicScript
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let script_path = ctx
            .candidate_dir
            .join(format!("bundle/scripts/{}.sh", ctx.authored.slug));

        let content = match std::fs::read_to_string(&script_path) {
            Ok(c) => c,
            Err(e) => {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    format!("script not found: {e}"),
                    timer.elapsed_ms(),
                );
            }
        };

        if content.is_empty() {
            return GateRunResult::failed(
                self.artifact_kind(),
                "script is empty".to_owned(),
                timer.elapsed_ms(),
            );
        }

        if !content.starts_with("#!") {
            return GateRunResult::failed(
                self.artifact_kind(),
                "script missing shebang (#!) line".to_owned(),
                timer.elapsed_ms(),
            );
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 3: Runs unit test cases against the deterministic script.
pub struct UnitTestRunner;

#[async_trait::async_trait]
impl GateRunner for UnitTestRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::UnitTests
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let cases = match ctx.authored.unit_tests.get("cases").and_then(|v| v.as_array()) {
            Some(c) => c,
            None => {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    "unit_tests missing 'cases' array".to_owned(),
                    timer.elapsed_ms(),
                );
            }
        };

        let script_path = ctx
            .candidate_dir
            .join(format!("bundle/scripts/{}.sh", ctx.authored.slug));

        for (i, case) in cases.iter().enumerate() {
            let input = case.get("input").and_then(|v| v.as_str()).unwrap_or("");
            let expected = match case.get("expected_stdout").and_then(|v| v.as_str()) {
                Some(e) => e,
                None => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("case {i}: missing expected_stdout"),
                        timer.elapsed_ms(),
                    );
                }
            };
            let timeout_ms = case
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(10_000);

            match run_script(&script_path, input, timeout_ms, &[]).await {
                Ok(stdout) if stdout == expected => {}
                Ok(stdout) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("case {i}: expected {expected:?}, got {stdout:?}"),
                        timer.elapsed_ms(),
                    );
                }
                Err(e) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("case {i}: {e}"),
                        timer.elapsed_ms(),
                    );
                }
            }
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 4: Runs integration test cases with CAIRN_INTEGRATION=1.
pub struct IntegrationTestRunner;

#[async_trait::async_trait]
impl GateRunner for IntegrationTestRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::IntegrationTests
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let cases = match ctx
            .authored
            .integration_tests
            .get("cases")
            .and_then(|v| v.as_array())
        {
            Some(c) => c,
            None => {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    "integration_tests missing 'cases' array".to_owned(),
                    timer.elapsed_ms(),
                );
            }
        };

        let script_path = ctx
            .candidate_dir
            .join(format!("bundle/scripts/{}.sh", ctx.authored.slug));

        for (i, case) in cases.iter().enumerate() {
            let input = case.get("input").and_then(|v| v.as_str()).unwrap_or("");
            let expected = match case.get("expected_stdout").and_then(|v| v.as_str()) {
                Some(e) => e,
                None => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("case {i}: missing expected_stdout"),
                        timer.elapsed_ms(),
                    );
                }
            };
            let timeout_ms = case
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(30_000);

            match run_script(
                &script_path,
                input,
                timeout_ms,
                &[("CAIRN_INTEGRATION", "1")],
            )
            .await
            {
                Ok(stdout) if stdout == expected => {}
                Ok(stdout) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("case {i}: expected {expected:?}, got {stdout:?}"),
                        timer.elapsed_ms(),
                    );
                }
                Err(e) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("case {i}: {e}"),
                        timer.elapsed_ms(),
                    );
                }
            }
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 5: Runs LLM-based rubric evals.
pub struct LlmEvalRunner;

#[async_trait::async_trait]
impl GateRunner for LlmEvalRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::LlmEvals
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let Some(llm) = ctx.llm else {
            return GateRunResult::failed(
                self.artifact_kind(),
                "LLM provider required for eval gate".to_owned(),
                timer.elapsed_ms(),
            );
        };

        let rubric = match ctx.authored.llm_evals.get("rubric").and_then(|v| v.as_array()) {
            Some(r) => r,
            None => {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    "llm_evals missing 'rubric' array".to_owned(),
                    timer.elapsed_ms(),
                );
            }
        };

        for (i, item) in rubric.iter().enumerate() {
            let prompt = item
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let expected = item
                .get("expected_behavior")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let criteria = item
                .get("scoring_criteria")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let judge_prompt = format!(
                "Evaluate whether this skill correctly handles the following intent.\n\
                 Intent: {prompt}\n\
                 Expected behavior: {expected}\n\
                 Scoring criteria: {criteria}\n\
                 Skill contract:\n{}\n\n\
                 Respond with JSON: {{\"pass\": true/false, \"reason\": \"...\"}}",
                ctx.authored.skill_markdown
            );

            let req = cairn_core::contract::llm_provider::CompletionRequest::builder()
                .prompt(judge_prompt)
                .schema(serde_json::json!({
                    "type": "object",
                    "required": ["pass", "reason"],
                    "properties": {
                        "pass": {"type": "boolean"},
                        "reason": {"type": "string"}
                    }
                }))
                .build();

            match llm.complete(&req).await {
                Ok(cairn_core::contract::llm_provider::CompletionOutput::Json(v)) => {
                    if !v.get("pass").and_then(|p| p.as_bool()).unwrap_or(false) {
                        let reason = v
                            .get("reason")
                            .and_then(|r| r.as_str())
                            .unwrap_or("no reason");
                        return GateRunResult::failed(
                            self.artifact_kind(),
                            format!("rubric item {i} failed: {reason}"),
                            timer.elapsed_ms(),
                        );
                    }
                }
                Ok(_) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("rubric item {i}: LLM returned non-JSON"),
                        timer.elapsed_ms(),
                    );
                }
                Err(e) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("rubric item {i}: LLM error: {e}"),
                        timer.elapsed_ms(),
                    );
                }
            }
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 6: Validates resolver trigger entries.
pub struct ResolverTriggerRunner;

#[async_trait::async_trait]
impl GateRunner for ResolverTriggerRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::ResolverTrigger
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let triggers = match ctx.authored.resolver_triggers.as_array() {
            Some(arr) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>(),
            None => {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    "resolver_triggers must be a JSON array of strings".to_owned(),
                    timer.elapsed_ms(),
                );
            }
        };

        if triggers.is_empty() {
            return GateRunResult::failed(
                self.artifact_kind(),
                "resolver_triggers is empty".to_owned(),
                timer.elapsed_ms(),
            );
        }

        for trigger in &triggers {
            if trigger.trim().is_empty() {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    "resolver_triggers contains blank entry".to_owned(),
                    timer.elapsed_ms(),
                );
            }
        }

        for existing_skill in &ctx.snapshot.skills {
            if existing_skill.lane == ctx.authored.lane {
                continue;
            }
            for existing_trigger in &existing_skill.resolver_triggers {
                for candidate_trigger in &triggers {
                    if existing_trigger.trim().eq_ignore_ascii_case(candidate_trigger.trim()) {
                        return GateRunResult::failed(
                            self.artifact_kind(),
                            format!(
                                "trigger {:?} collides with skill {} (lane {})",
                                candidate_trigger, existing_skill.skill_id, existing_skill.lane
                            ),
                            timer.elapsed_ms(),
                        );
                    }
                }
            }
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 7: Evaluates resolver precision/recall against labelled intents.
pub struct ResolverEvalRunner;

#[async_trait::async_trait]
impl GateRunner for ResolverEvalRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::ResolverEval
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let intents = match ctx
            .authored
            .resolver_eval
            .get("intents")
            .and_then(|v| v.as_array())
        {
            Some(i) => i,
            None => {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    "resolver_eval missing 'intents' array".to_owned(),
                    timer.elapsed_ms(),
                );
            }
        };

        let triggers: Vec<String> = ctx
            .authored
            .resolver_triggers
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let mut total = 0u32;
        let mut hits = 0u32;

        for intent_obj in intents {
            let intent = intent_obj
                .get("intent")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let expected_lane = intent_obj
                .get("expected_lane")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            total += 1;
            let matched = triggers
                .iter()
                .any(|t| intent.to_lowercase().contains(&t.to_lowercase()));
            if matched && expected_lane == ctx.authored.lane {
                hits += 1;
            }
        }

        if total == 0 {
            return GateRunResult::failed(
                self.artifact_kind(),
                "no intents to evaluate".to_owned(),
                timer.elapsed_ms(),
            );
        }

        let recall = f64::from(hits) / f64::from(total);
        if recall < 0.8 {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!("recall {recall:.2} < 0.8 threshold ({hits}/{total} matched)"),
                timer.elapsed_ms(),
            );
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 8: Runs check-resolvable and DRY audit via lint snapshot merge.
pub struct CheckResolvableAndDryRunner;

#[async_trait::async_trait]
impl GateRunner for CheckResolvableAndDryRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::CheckResolvableAndDry
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let triggers: Vec<String> = ctx
            .authored
            .resolver_triggers
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let files_to = ctx
            .authored
            .filing_rules
            .get("files_to")
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        let candidate_skill = SkillLintSkill {
            skill_id: ctx.candidate_id.to_owned(),
            lane: ctx.authored.lane.clone(),
            path: format!("bundle/skills/skill_{}.md", ctx.authored.slug),
            uses: Some(format!("bundle/scripts/{}.sh", ctx.authored.slug)),
            resolver_triggers: triggers,
            files_to,
            gate_report_passed: true,
            rollback_version_count: 1,
            existing_paths: vec![
                format!("bundle/scripts/{}.sh", ctx.authored.slug),
            ],
        };

        let mut merged = ctx.snapshot.clone();
        merged.skills.push(candidate_skill);

        let issues = cairn_core::pipeline::skillify::lint_skill_snapshot(&merged);
        let candidate_issues: Vec<_> = issues
            .iter()
            .filter(|issue| issue.skill_id == ctx.candidate_id)
            .collect();

        if candidate_issues.is_empty() {
            GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
        } else {
            let messages: Vec<String> = candidate_issues
                .iter()
                .map(|issue| issue.message.clone())
                .collect();
            GateRunResult::failed(
                self.artifact_kind(),
                format!("lint issues: {}", messages.join("; ")),
                timer.elapsed_ms(),
            )
        }
    }
}

/// Gate 9: End-to-end smoke test — trigger → script → output.
pub struct E2eSmokeRunner;

#[async_trait::async_trait]
impl GateRunner for E2eSmokeRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::E2eSmoke
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let cases = match ctx.authored.smoke.get("cases").and_then(|v| v.as_array()) {
            Some(c) => c,
            None => {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    "smoke missing 'cases' array".to_owned(),
                    timer.elapsed_ms(),
                );
            }
        };

        let script_path = ctx
            .candidate_dir
            .join(format!("bundle/scripts/{}.sh", ctx.authored.slug));

        for (i, case) in cases.iter().enumerate() {
            let expected = case
                .get("expected_output")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match run_script(&script_path, "", 60_000, &[]).await {
                Ok(stdout) if stdout == expected => {}
                Ok(stdout) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("smoke case {i}: expected {expected:?}, got {stdout:?}"),
                        timer.elapsed_ms(),
                    );
                }
                Err(e) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("smoke case {i}: {e}"),
                        timer.elapsed_ms(),
                    );
                }
            }
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 10: Validates filing rules path.
pub struct FilingRulesRunner;

#[async_trait::async_trait]
impl GateRunner for FilingRulesRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::FilingRules
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let files_to = match ctx
            .authored
            .filing_rules
            .get("files_to")
            .and_then(|v| v.as_str())
        {
            Some(f) => f,
            None => {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    "filing_rules missing 'files_to' field".to_owned(),
                    timer.elapsed_ms(),
                );
            }
        };

        let path = std::path::Path::new(files_to);
        if path.is_absolute() {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!("files_to `{files_to}` must be relative"),
                timer.elapsed_ms(),
            );
        }
        if !files_to.ends_with('/') {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!("files_to `{files_to}` must end with /"),
                timer.elapsed_ms(),
            );
        }
        if path.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        }) {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!("files_to `{files_to}` contains unsafe path components"),
                timer.elapsed_ms(),
            );
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Execute a script via subprocess with optional stdin and env vars.
async fn run_script(
    script_path: &Path,
    input: &str,
    timeout_ms: u64,
    env: &[(&str, &str)],
) -> Result<String, String> {
    use tokio::process::Command;

    let mut cmd = Command::new("bash");
    cmd.arg(script_path);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(input.as_bytes())
            .await
            .map_err(|e| format!("stdin write: {e}"))?;
    }

    let timeout = tokio::time::Duration::from_millis(timeout_ms);
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(format!("script exited {}: {stderr}", output.status))
            }
        }
        Ok(Err(e)) => Err(format!("wait failed: {e}")),
        Err(_) => {
            let _ = child.kill().await;
            Err("script timed out".to_owned())
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-workflows --no-fail-fast -- skillify_gate 2>&1 | tail -15`
Expected: all gate runner tests PASS

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-workflows/src/skillify/gate_runner.rs crates/cairn-workflows/tests/skillify_gate_runners.rs
git commit -m "feat(workflows): implement 10 gate runners for Skillify pipeline (#128)"
```

---

## Task 6: `GateRunnerRegistry` with dependency ordering

**Files:**
- Create: `crates/cairn-workflows/src/skillify/gate_registry.rs`
- Modify: `crates/cairn-workflows/src/skillify/mod.rs`

- [ ] **Step 1: Implement the registry**

Create `crates/cairn-workflows/src/skillify/gate_registry.rs`:

```rust
//! Ordered gate runner execution with dependency blocking.

use cairn_core::pipeline::skillify::{SkillArtifactKind, SkillifyGateStatus};

use super::gate_runner::{
    CheckResolvableAndDryRunner, DeterministicScriptRunner, E2eSmokeRunner, FilingRulesRunner,
    GateRunContext, GateRunResult, GateRunner, IntegrationTestRunner, LlmEvalRunner,
    ResolverEvalRunner, ResolverTriggerRunner, SkillContractRunner, UnitTestRunner,
};

/// Dependency tier for gate execution ordering.
struct Tier {
    runners: Vec<Box<dyn GateRunner>>,
    depends_on: Vec<SkillArtifactKind>,
}

/// Registry of gate runners with dependency-ordered execution.
pub struct GateRunnerRegistry {
    tiers: Vec<Tier>,
}

impl GateRunnerRegistry {
    /// Create a registry with the default 10-runner suite in dependency order.
    #[must_use]
    pub fn default_suite() -> Self {
        Self {
            tiers: vec![
                Tier {
                    runners: vec![Box::new(SkillContractRunner)],
                    depends_on: vec![],
                },
                Tier {
                    runners: vec![Box::new(DeterministicScriptRunner)],
                    depends_on: vec![SkillArtifactKind::SkillContract],
                },
                Tier {
                    runners: vec![
                        Box::new(FilingRulesRunner),
                        Box::new(ResolverTriggerRunner),
                    ],
                    depends_on: vec![SkillArtifactKind::SkillContract],
                },
                Tier {
                    runners: vec![
                        Box::new(UnitTestRunner),
                        Box::new(IntegrationTestRunner),
                    ],
                    depends_on: vec![SkillArtifactKind::DeterministicScript],
                },
                Tier {
                    runners: vec![Box::new(LlmEvalRunner)],
                    depends_on: vec![
                        SkillArtifactKind::SkillContract,
                        SkillArtifactKind::DeterministicScript,
                    ],
                },
                Tier {
                    runners: vec![Box::new(ResolverEvalRunner)],
                    depends_on: vec![SkillArtifactKind::ResolverTrigger],
                },
                Tier {
                    runners: vec![Box::new(CheckResolvableAndDryRunner)],
                    depends_on: vec![
                        SkillArtifactKind::ResolverTrigger,
                        SkillArtifactKind::FilingRules,
                    ],
                },
                Tier {
                    runners: vec![Box::new(E2eSmokeRunner)],
                    depends_on: vec![
                        SkillArtifactKind::UnitTests,
                        SkillArtifactKind::ResolverTrigger,
                    ],
                },
            ],
        }
    }

    /// Run all gates in dependency order. If a dependency gate failed,
    /// downstream gates are marked Blocked.
    pub async fn run_all(&self, ctx: &GateRunContext<'_>) -> Vec<GateRunResult> {
        let mut results = Vec::new();

        for tier in &self.tiers {
            let dep_failed = tier.depends_on.iter().any(|dep| {
                results
                    .iter()
                    .any(|r: &GateRunResult| r.kind == *dep && r.status != SkillifyGateStatus::Passed)
            });

            for runner in &tier.runners {
                if dep_failed {
                    results.push(GateRunResult::blocked(
                        runner.artifact_kind(),
                        "dependency gate failed".to_owned(),
                    ));
                } else {
                    results.push(runner.run(ctx).await);
                }
            }
        }

        results
    }
}
```

- [ ] **Step 2: Wire into mod.rs**

Add to `crates/cairn-workflows/src/skillify/mod.rs`:

```rust
pub mod gate_registry;
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p cairn-workflows 2>&1 | tail -10`
Expected: compiles cleanly

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-workflows/src/skillify/gate_registry.rs crates/cairn-workflows/src/skillify/mod.rs
git commit -m "feat(workflows): add GateRunnerRegistry with dependency-ordered execution (#128)"
```

---

## Task 7: `SkillifyPipeline` orchestrator

**Files:**
- Create: `crates/cairn-workflows/src/skillify/pipeline.rs`
- Create: `crates/cairn-workflows/tests/skillify_pipeline.rs`
- Modify: `crates/cairn-workflows/src/skillify/mod.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/cairn-workflows/tests/skillify_pipeline.rs`:

```rust
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::pipeline::skillify::SkillifyStage;
use cairn_workflows::skillify::pipeline::SkillifyPipeline;
use cairn_workflows::{SkillifyPayload, SkillifyTrigger};
use serde_json::json;
use tempfile::TempDir;

struct PipelineLlm {
    call_count: std::sync::atomic::AtomicU32,
}

impl PipelineLlm {
    fn new() -> Self {
        Self {
            call_count: std::sync::atomic::AtomicU32::new(0),
        }
    }
}

#[async_trait::async_trait]
impl LLMProvider for PipelineLlm {
    fn name(&self) -> &'static str {
        "pipeline-llm"
    }

    fn capabilities(&self) -> &LLMProviderCapabilities {
        static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
            json_mode: true,
            streaming: false,
            tool_calls: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        let n = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n == 0 {
            // STAGE 1: extraction → spec draft
            Ok(CompletionOutput::Json(json!({
                "lane": "deploy.hotfix",
                "slug": "deploy-hotfix",
                "decision_tree": {"root": "check_env"},
                "triggers": ["deploy hotfix"],
                "success_criteria": ["script exits 0"],
                "source_refs": ["01HQZX9F5N0000000000000001"],
                "requires": [],
                "provides": ["deploy.hotfix"]
            })))
        } else if n == 1 {
            // STAGE 2: authoring → authored bundle
            Ok(CompletionOutput::Json(json!({
                "lane": "deploy.hotfix",
                "slug": "deploy-hotfix",
                "skill_markdown": "---\nname: deploy-hotfix\nlane: deploy.hotfix\ntriggers:\n  - deploy hotfix\nuses: scripts/deploy-hotfix.sh\nfiles_to: wiki/summaries/\n---\nRun the script.",
                "script": "#!/usr/bin/env bash\nset -euo pipefail\necho deploy-hotfix\n",
                "unit_tests": {"cases": [{"input": "", "expected_stdout": "deploy-hotfix\n", "timeout_ms": 5000}]},
                "integration_tests": {"cases": [{"input": "", "expected_stdout": "deploy-hotfix\n", "timeout_ms": 10000}]},
                "llm_evals": {"rubric": [{"prompt": "deploy hotfix", "expected_behavior": "calls script", "scoring_criteria": "invoked"}]},
                "resolver_triggers": ["deploy hotfix"],
                "resolver_eval": {"intents": [{"intent": "deploy hotfix", "expected_lane": "deploy.hotfix"}]},
                "smoke": {"cases": [{"trigger_phrase": "deploy hotfix", "expected_output": "deploy-hotfix\n"}]},
                "filing_rules": {"files_to": "wiki/summaries/"}
            })))
        } else {
            // STAGE 3 LLM eval: always pass
            Ok(CompletionOutput::Json(
                json!({"pass": true, "reason": "looks good"}),
            ))
        }
    }
}

fn payload() -> SkillifyPayload {
    SkillifyPayload {
        trigger: SkillifyTrigger::Explicit,
        key: "session-pipeline".to_owned(),
        candidate_id: Some("skc_pipeline_test".to_owned()),
        bound_scope: None,
        source_record_ids: vec!["01HQZX9F5N0000000000000001".to_owned()],
    }
}

#[tokio::test]
async fn pipeline_runs_all_stages_with_mock_llm() {
    let temp = TempDir::new().unwrap();
    let pipeline = SkillifyPipeline::new(
        temp.path().to_path_buf(),
        Some(Arc::new(PipelineLlm::new())),
    );

    let result = pipeline.run(payload()).await.unwrap();
    assert_eq!(result.final_stage, SkillifyStage::Promote);
    assert!(result.errors.is_empty());
    assert!(!result.gate_report.gates.is_empty());
}

#[tokio::test]
async fn pipeline_blocks_without_llm() {
    let temp = TempDir::new().unwrap();
    let pipeline = SkillifyPipeline::new(temp.path().to_path_buf(), None);

    let result = pipeline.run(payload()).await.unwrap();
    assert_eq!(result.final_stage, SkillifyStage::Blocked);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p cairn-workflows --no-fail-fast -- skillify_pipeline 2>&1 | tail -20`
Expected: compilation error — `SkillifyPipeline` not defined

- [ ] **Step 3: Implement `pipeline.rs`**

Create `crates/cairn-workflows/src/skillify/pipeline.rs`:

```rust
//! Skillify 5-stage pipeline orchestrator.

use std::path::PathBuf;
use std::sync::Arc;

use cairn_core::contract::llm_provider::{CompletionOutput, CompletionRequest, LLMProvider, LlmError};
use cairn_core::pipeline::skillify::{
    SkillArtifactBundle, SkillLintSnapshot, SkillSpecDraft, SkillifyGateReport,
    SkillifyPipelineState, SkillifyStage,
};

use super::gate_registry::GateRunnerRegistry;
use super::gate_runner::GateRunContext;
use super::materialize::{
    AuthoredSkillBundle, SkillifyMaterializeError, materialize_blocked_candidate, materialize_bundle,
};
use super::SkillifyPayload;

/// Pipeline orchestration error.
#[derive(Debug, thiserror::Error)]
pub enum SkillifyPipelineError {
    /// No LLM provider configured.
    #[error("skillify pipeline: no LLM provider configured")]
    NoLlm,
    /// LLM call failed.
    #[error(transparent)]
    Llm(#[from] LlmError),
    /// Bundle materialization failed.
    #[error(transparent)]
    Materialize(#[from] SkillifyMaterializeError),
    /// Filesystem I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Result from a complete pipeline run.
#[derive(Debug)]
pub struct SkillifyPipelineResult {
    /// Candidate id.
    pub candidate_id: String,
    /// Final stage reached.
    pub final_stage: SkillifyStage,
    /// Gate report (empty if pipeline did not reach Gate stage).
    pub gate_report: SkillifyGateReport,
    /// Collected error messages.
    pub errors: Vec<String>,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
}

/// Orchestrates the 5-stage Skillify pipeline.
pub struct SkillifyPipeline {
    vault_root: PathBuf,
    llm: Option<Arc<dyn LLMProvider>>,
    gate_registry: GateRunnerRegistry,
}

impl SkillifyPipeline {
    /// Create a new pipeline.
    #[must_use]
    pub fn new(vault_root: PathBuf, llm: Option<Arc<dyn LLMProvider>>) -> Self {
        Self {
            vault_root,
            llm,
            gate_registry: GateRunnerRegistry::default_suite(),
        }
    }

    /// Run the full pipeline for a payload.
    ///
    /// # Errors
    /// Returns on fatal I/O or serialization failures. Gate failures and
    /// LLM unavailability are captured in the result, not as errors.
    pub async fn run(
        &self,
        payload: SkillifyPayload,
    ) -> Result<SkillifyPipelineResult, SkillifyPipelineError> {
        let start = std::time::Instant::now();
        let candidate_id = payload.candidate_id_or_derive();
        let mut state = SkillifyPipelineState::new(candidate_id.clone());
        let mut errors = Vec::new();

        // STAGE 1: Extract
        let Some(llm) = &self.llm else {
            materialize_blocked_candidate(
                &self.vault_root,
                &candidate_id,
                "llm provider not configured",
            )?;
            let _ = state.block("no LLM provider configured".to_owned());
            return Ok(self.build_result(state, errors, start));
        };

        let spec = match self.extract(llm, &payload).await {
            Ok(spec) => spec,
            Err(e) => {
                errors.push(e.to_string());
                let _ = state.fail(e.to_string());
                return Ok(self.build_result(state, errors, start));
            }
        };

        let candidate_dir = self
            .vault_root
            .join(".cairn/evolution/skillify")
            .join(&candidate_id);
        std::fs::create_dir_all(&candidate_dir)?;
        std::fs::write(
            candidate_dir.join("skill-spec.draft.json"),
            serde_json::to_vec_pretty(&spec)?,
        )?;

        let _ = state.advance_to_author(spec.clone());

        // STAGE 2: Author
        let authored = match self.author(llm, &spec).await {
            Ok(a) => a,
            Err(e) => {
                errors.push(e.to_string());
                let _ = state.fail(e.to_string());
                return Ok(self.build_result(state, errors, start));
            }
        };

        let bundle = materialize_bundle(
            &self.vault_root,
            &candidate_id,
            &authored,
            &payload.source_record_ids,
        )?;

        let _ = state.advance_to_gate(bundle.clone());

        // STAGE 3: Gate
        let snapshot = SkillLintSnapshot { skills: vec![] };
        let ctx = GateRunContext {
            vault_root: &self.vault_root,
            candidate_id: &candidate_id,
            candidate_dir: candidate_dir.clone(),
            bundle: &bundle,
            authored: &authored,
            llm: Some(llm.as_ref()),
            snapshot: &snapshot,
        };

        let results = self.gate_registry.run_all(&ctx).await;
        for result in &results {
            state.record_gate(result.clone().into_gate());
        }

        let gate_report = state.gate_report().clone();
        std::fs::write(
            candidate_dir.join("gate-report.json"),
            serde_json::to_vec_pretty(&gate_report)?,
        )?;

        let any_failed = results.iter().any(|r| {
            r.status != cairn_core::pipeline::skillify::SkillifyGateStatus::Passed
        });

        if any_failed {
            let failed_names: Vec<String> = results
                .iter()
                .filter(|r| r.status != cairn_core::pipeline::skillify::SkillifyGateStatus::Passed)
                .map(|r| r.kind.as_str().to_owned())
                .collect();
            let msg = format!("gates failed: {}", failed_names.join(", "));
            errors.push(msg.clone());
            let _ = state.fail(msg);
            return Ok(self.build_result(state, errors, start));
        }

        // STAGE 4: Promote
        let _ = state.advance_to_promote();

        Ok(self.build_result(state, errors, start))
    }

    async fn extract(
        &self,
        llm: &Arc<dyn LLMProvider>,
        payload: &SkillifyPayload,
    ) -> Result<SkillSpecDraft, SkillifyPipelineError> {
        let req = CompletionRequest::builder()
            .prompt(format!(
                "Extract a skill specification from the following source records: {:?}. \
                 Return a JSON object with fields: lane, slug, decision_tree, triggers, \
                 success_criteria, source_refs, requires, provides.",
                payload.source_record_ids
            ))
            .schema(serde_json::json!({
                "type": "object",
                "required": ["lane", "slug", "decision_tree", "triggers", "success_criteria", "source_refs"]
            }))
            .build();

        let CompletionOutput::Json(value) = llm.complete(&req).await? else {
            return Err(SkillifyPipelineError::NoLlm);
        };

        let spec: SkillSpecDraft =
            serde_json::from_value(value).map_err(SkillifyPipelineError::Json)?;
        spec.validate()
            .map_err(|e| SkillifyPipelineError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())))?;
        Ok(spec)
    }

    async fn author(
        &self,
        llm: &Arc<dyn LLMProvider>,
        spec: &SkillSpecDraft,
    ) -> Result<AuthoredSkillBundle, SkillifyPipelineError> {
        let req = CompletionRequest::builder()
            .prompt(format!(
                "Create a section 11.b Skillify bundle for lane {} slug {}. \
                 Decision tree: {}. Return JSON only.",
                spec.lane,
                spec.slug,
                spec.decision_tree
            ))
            .schema(serde_json::json!({
                "type": "object",
                "required": [
                    "lane", "slug", "skill_markdown", "script",
                    "unit_tests", "integration_tests", "llm_evals",
                    "resolver_triggers", "resolver_eval", "smoke", "filing_rules"
                ]
            }))
            .build();

        let CompletionOutput::Json(value) = llm.complete(&req).await? else {
            return Err(SkillifyPipelineError::NoLlm);
        };

        let authored =
            AuthoredSkillBundle::try_from(value).map_err(SkillifyPipelineError::Materialize)?;
        Ok(authored)
    }

    fn build_result(
        &self,
        state: SkillifyPipelineState,
        errors: Vec<String>,
        start: std::time::Instant,
    ) -> SkillifyPipelineResult {
        SkillifyPipelineResult {
            candidate_id: state.candidate_id().to_owned(),
            final_stage: state.stage(),
            gate_report: state.gate_report().clone(),
            errors,
            duration_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        }
    }
}
```

- [ ] **Step 4: Wire into mod.rs**

Add to `crates/cairn-workflows/src/skillify/mod.rs`:

```rust
pub mod pipeline;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-workflows --no-fail-fast -- skillify_pipeline 2>&1 | tail -15`
Expected: all pipeline tests PASS

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-workflows/src/skillify/pipeline.rs crates/cairn-workflows/src/skillify/mod.rs crates/cairn-workflows/tests/skillify_pipeline.rs
git commit -m "feat(workflows): add SkillifyPipeline 5-stage orchestrator (#128)"
```

---

## Task 8: Refactor `SkillifyHandler` to delegate to pipeline

**Files:**
- Modify: `crates/cairn-workflows/src/skillify/handler.rs`

- [ ] **Step 1: Run existing handler tests to verify baseline**

Run: `cargo nextest run -p cairn-workflows --no-fail-fast -- skillify_handler 2>&1 | tail -15`
Expected: all existing tests PASS

- [ ] **Step 2: Refactor `run_once` to delegate**

Replace the body of `SkillifyHandler::run_once()` in `handler.rs`:

```rust
pub async fn run_once(&self, payload: super::SkillifyPayload) -> Result<(), SkillifyRunError> {
    let candidate_id = payload.candidate_id_or_derive();
    if candidate_materialized(&self.vault_root, &candidate_id)? {
        return Ok(());
    }

    let pipeline = super::pipeline::SkillifyPipeline::new(
        self.vault_root.clone(),
        self.llm.clone(),
    );

    let result = pipeline.run(payload).await.map_err(|e| match e {
        super::pipeline::SkillifyPipelineError::NoLlm => SkillifyRunError::NoLlm,
        super::pipeline::SkillifyPipelineError::Llm(e) => SkillifyRunError::Llm(e),
        super::pipeline::SkillifyPipelineError::Materialize(e) => {
            SkillifyRunError::Materialize(e)
        }
        super::pipeline::SkillifyPipelineError::Io(e) => {
            SkillifyRunError::Materialize(SkillifyMaterializeError::Io(e))
        }
        super::pipeline::SkillifyPipelineError::Json(e) => {
            SkillifyRunError::Materialize(SkillifyMaterializeError::Json(e))
        }
    })?;

    if !result.errors.is_empty()
        && matches!(
            result.final_stage,
            cairn_core::pipeline::skillify::SkillifyStage::Failed
                | cairn_core::pipeline::skillify::SkillifyStage::Blocked
        )
    {
        if matches!(
            result.final_stage,
            cairn_core::pipeline::skillify::SkillifyStage::Blocked
        ) {
            return Err(SkillifyRunError::NoLlm);
        }
        return Err(SkillifyRunError::NonJsonOutput);
    }

    Ok(())
}
```

- [ ] **Step 3: Run handler tests to verify backward compatibility**

Run: `cargo nextest run -p cairn-workflows --no-fail-fast -- skillify_handler 2>&1 | tail -15`
Expected: all existing handler tests still PASS

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-workflows/src/skillify/handler.rs
git commit -m "refactor(workflows): delegate SkillifyHandler to SkillifyPipeline (#128)"
```

---

## Task 9: `SkillPackBuilder` and archive packer

**Files:**
- Create: `crates/cairn-workflows/src/skillify/packer.rs`
- Create: `crates/cairn-workflows/tests/skillify_packer.rs`
- Modify: `crates/cairn-workflows/src/skillify/mod.rs`
- Modify: `Cargo.toml` (workspace deps)
- Modify: `crates/cairn-workflows/Cargo.toml`

- [ ] **Step 1: Add `flate2` and `tar` to workspace deps**

In root `Cargo.toml` under `[workspace.dependencies]`, add:

```toml
flate2 = "1"
tar = "0.4"
```

In `crates/cairn-workflows/Cargo.toml` under `[dependencies]`, add:

```toml
flate2 = { workspace = true }
tar = { workspace = true }
```

- [ ] **Step 2: Write failing packer tests**

Create `crates/cairn-workflows/tests/skillify_packer.rs`:

```rust
#![allow(missing_docs)]

use cairn_core::pipeline::skillify::{SkillPackError, SkillifyGateStatus};
use cairn_workflows::skillify::materialize::{AuthoredSkillBundle, materialize_bundle};
use cairn_workflows::skillify::packer::{SkillPackBuilder, unpack_archive};
use serde_json::json;
use tempfile::TempDir;

fn authored(slug: &str) -> AuthoredSkillBundle {
    AuthoredSkillBundle {
        lane: format!("test.{slug}"),
        slug: slug.to_owned(),
        skill_markdown: format!(
            "---\nname: {slug}\nlane: test.{slug}\ntriggers:\n  - {slug}\nuses: scripts/{slug}.sh\nfiles_to: wiki/test/\n---\nSkill."
        ),
        script: format!("#!/usr/bin/env bash\necho {slug}\n"),
        unit_tests: json!({"cases": []}),
        integration_tests: json!({"cases": []}),
        llm_evals: json!({"rubric": []}),
        resolver_triggers: json!([slug]),
        resolver_eval: json!({"intents": []}),
        smoke: json!({"cases": []}),
        filing_rules: json!({"files_to": "wiki/test/"}),
    }
}

fn setup_candidate(temp: &TempDir, candidate_id: &str, slug: &str) {
    let a = authored(slug);
    materialize_bundle(
        temp.path(),
        candidate_id,
        &a,
        &["01HQZX9F5N0000000000000001".to_owned()],
    )
    .unwrap();

    // Write a passing gate report so the packer accepts it
    let root = temp
        .path()
        .join(".cairn/evolution/skillify")
        .join(candidate_id);
    let report = cairn_core::pipeline::skillify::SkillifyGateReport {
        candidate_id: candidate_id.to_owned(),
        gates: cairn_core::pipeline::skillify::SkillArtifactKind::required()
            .iter()
            .map(|kind| cairn_core::pipeline::skillify::SkillifyGate {
                name: kind.as_str().to_owned(),
                status: SkillifyGateStatus::Passed,
                message: None,
            })
            .collect(),
    };
    std::fs::write(
        root.join("gate-report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn pack_and_unpack_round_trip() {
    let temp = TempDir::new().unwrap();
    setup_candidate(&temp, "skc_alpha", "alpha");
    setup_candidate(&temp, "skc_beta", "beta");

    let archive = SkillPackBuilder::new("test-pack", "0.1.0", ">=0.1.0", "Test pack")
        .add_candidate("skc_alpha")
        .add_candidate("skc_beta")
        .build(temp.path())
        .unwrap();

    assert!(archive.archive_path.exists());
    assert_eq!(archive.manifest.skills.len(), 2);
    assert!(archive.manifest.pack_id.starts_with("skp_"));

    // Unpack into a fresh vault
    let install_temp = TempDir::new().unwrap();
    unpack_archive(&archive.archive_path, install_temp.path(), "0.1.0").unwrap();

    assert!(
        install_temp
            .path()
            .join(".cairn/evolution/skillify/skc_alpha/manifest.json")
            .exists()
    );
    assert!(
        install_temp
            .path()
            .join(".cairn/evolution/skillify/skc_beta/manifest.json")
            .exists()
    );
}

#[test]
fn pack_rejects_candidate_with_failing_gates() {
    let temp = TempDir::new().unwrap();
    let a = authored("gamma");
    materialize_bundle(
        temp.path(),
        "skc_gamma",
        &a,
        &["01HQZX9F5N0000000000000001".to_owned()],
    )
    .unwrap();
    // Gate report is blocked (default from materialize_bundle) → packer should reject

    let err = SkillPackBuilder::new("fail-pack", "0.1.0", ">=0.1.0", "Fail")
        .add_candidate("skc_gamma")
        .build(temp.path())
        .unwrap_err();

    assert!(err.to_string().contains("gate"));
}

#[test]
fn unpack_rejects_incompatible_version() {
    let temp = TempDir::new().unwrap();
    setup_candidate(&temp, "skc_delta", "delta");

    let archive = SkillPackBuilder::new("version-pack", "0.1.0", ">=99.0.0", "Future pack")
        .add_candidate("skc_delta")
        .build(temp.path())
        .unwrap();

    let install_temp = TempDir::new().unwrap();
    let err = unpack_archive(&archive.archive_path, install_temp.path(), "0.1.0").unwrap_err();
    assert!(err.to_string().contains("Cairn"));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo nextest run -p cairn-workflows --no-fail-fast -- skillify_packer 2>&1 | tail -20`
Expected: compilation error — `SkillPackBuilder` not defined

- [ ] **Step 4: Implement `packer.rs`**

Create `crates/cairn-workflows/src/skillify/packer.rs`:

```rust
//! SkillPack archive builder and unpacker.

use std::fs;
use std::path::{Path, PathBuf};

use cairn_core::pipeline::skillify::{
    SkillArtifactBundle, SkillPackEntry, SkillPackError, SkillPackManifest, SkillifyGateReport,
};
use sha2::{Digest, Sha256};

/// Error from pack build or unpack.
#[derive(Debug, thiserror::Error)]
pub enum SkillPackBuildError {
    /// SkillPack validation failed.
    #[error(transparent)]
    Pack(#[from] SkillPackError),
    /// I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Candidate gate report not passing.
    #[error("candidate `{candidate_id}` gate report not passing")]
    GateNotPassing {
        /// Candidate id.
        candidate_id: String,
    },
    /// Candidate not found.
    #[error("candidate `{candidate_id}` not found")]
    CandidateNotFound {
        /// Candidate id.
        candidate_id: String,
    },
}

/// Built archive result.
pub struct SkillPackArchive {
    /// Validated manifest.
    pub manifest: SkillPackManifest,
    /// Path to the `.cairnpack` archive file.
    pub archive_path: PathBuf,
}

/// Builder for SkillPack archives.
pub struct SkillPackBuilder {
    name: String,
    version: String,
    cairn_compat: String,
    description: String,
    candidate_ids: Vec<String>,
}

impl SkillPackBuilder {
    /// Create a new builder.
    #[must_use]
    pub fn new(name: &str, version: &str, cairn_compat: &str, description: &str) -> Self {
        Self {
            name: name.to_owned(),
            version: version.to_owned(),
            cairn_compat: cairn_compat.to_owned(),
            description: description.to_owned(),
            candidate_ids: Vec::new(),
        }
    }

    /// Add a candidate to the pack.
    #[must_use]
    pub fn add_candidate(mut self, candidate_id: &str) -> Self {
        self.candidate_ids.push(candidate_id.to_owned());
        self
    }

    /// Build the `.cairnpack` archive.
    ///
    /// # Errors
    /// Returns when a candidate is missing, has failing gates, or archive
    /// creation fails.
    pub fn build(self, vault_root: &Path) -> Result<SkillPackArchive, SkillPackBuildError> {
        let mut entries = Vec::new();
        let mut all_provides = Vec::new();
        let mut all_requires = Vec::new();

        for cid in &self.candidate_ids {
            let cand_dir = vault_root
                .join(".cairn/evolution/skillify")
                .join(cid);

            let manifest_path = cand_dir.join("manifest.json");
            if !manifest_path.exists() {
                return Err(SkillPackBuildError::CandidateNotFound {
                    candidate_id: cid.clone(),
                });
            }

            let bundle: SkillArtifactBundle =
                serde_json::from_slice(&fs::read(&manifest_path)?)?;

            let report_path = cand_dir.join("gate-report.json");
            let report: SkillifyGateReport =
                serde_json::from_slice(&fs::read(&report_path)?)?;

            if !report.ready_for_promotion() {
                return Err(SkillPackBuildError::GateNotPassing {
                    candidate_id: cid.clone(),
                });
            }

            let artifact_hash = sha256_file(&manifest_path)?;

            let slug = bundle
                .artifacts
                .first()
                .map(|a| {
                    a.path
                        .rsplit('/')
                        .next()
                        .unwrap_or("")
                        .strip_prefix("skill_")
                        .and_then(|s| s.strip_suffix(".md"))
                        .unwrap_or(cid)
                        .to_owned()
                })
                .unwrap_or_else(|| cid.clone());

            let lane = find_lane_from_bundle(&cand_dir)?;

            entries.push(SkillPackEntry {
                candidate_id: cid.clone(),
                lane: lane.clone(),
                slug,
                bundle_version: bundle.version,
                artifact_sha256: artifact_hash,
            });

            all_provides.push(lane);
        }

        let candidate_ids_refs: Vec<&str> =
            self.candidate_ids.iter().map(String::as_str).collect();
        let pack_id =
            SkillPackManifest::derive_pack_id(&self.name, &self.version, &candidate_ids_refs);

        let manifest = SkillPackManifest {
            pack_id,
            name: self.name.clone(),
            version: self.version.clone(),
            cairn_compat: self.cairn_compat.clone(),
            description: self.description.clone(),
            skills: entries,
            requires: all_requires,
            provides: all_provides,
            content_sha256: String::new(),
        };

        // Build the tar.gz archive
        let archive_path = vault_root.join(format!("{}.cairnpack", self.name));
        let file = fs::File::create(&archive_path)?;
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        let manifest_json = serde_json::to_vec_pretty(&manifest)?;
        append_bytes(&mut tar, "manifest.json", &manifest_json)?;

        for cid in &self.candidate_ids {
            let cand_dir = vault_root
                .join(".cairn/evolution/skillify")
                .join(cid);
            append_dir_recursive(&mut tar, &cand_dir, &format!("skills/{cid}"))?;
        }

        let enc = tar.into_inner()?;
        enc.finish()?;

        // Recompute content hash
        let content_hash = sha256_file(&archive_path)?;
        let mut final_manifest = manifest;
        final_manifest.content_sha256 = content_hash;

        Ok(SkillPackArchive {
            manifest: final_manifest,
            archive_path,
        })
    }
}

/// Unpack a `.cairnpack` archive into a vault.
///
/// # Errors
/// Returns on incompatible version, corrupt archive, or I/O failure.
pub fn unpack_archive(
    archive_path: &Path,
    vault_root: &Path,
    cairn_version: &str,
) -> Result<SkillPackManifest, SkillPackBuildError> {
    let file = fs::File::open(archive_path)?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);

    let extract_dir = vault_root.join(".cairn/evolution/skillify/.unpack-tmp");
    fs::create_dir_all(&extract_dir)?;
    archive.unpack(&extract_dir)?;

    let manifest: SkillPackManifest = serde_json::from_slice(&fs::read(
        extract_dir.join("manifest.json"),
    )?)?;

    manifest
        .validate(cairn_version)
        .map_err(SkillPackBuildError::Pack)?;

    // Move skill directories to their final locations
    for entry in &manifest.skills {
        let src = extract_dir.join(format!("skills/{}", entry.candidate_id));
        let dst = vault_root
            .join(".cairn/evolution/skillify")
            .join(&entry.candidate_id);
        if src.exists() {
            if dst.exists() {
                fs::remove_dir_all(&dst)?;
            }
            fs::rename(&src, &dst)?;
        }
    }

    let _ = fs::remove_dir_all(&extract_dir);

    Ok(manifest)
}

fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let bytes = fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(format!("sha256:{:x}", h.finalize()))
}

fn find_lane_from_bundle(cand_dir: &Path) -> Result<String, SkillPackBuildError> {
    let skills_dir = cand_dir.join("bundle/skills");
    if skills_dir.exists() {
        for entry in fs::read_dir(&skills_dir)? {
            let entry = entry?;
            let content = fs::read_to_string(entry.path())?;
            for line in content.lines() {
                if let Some(lane) = line.strip_prefix("lane:") {
                    return Ok(lane.trim().to_owned());
                }
            }
        }
    }
    Ok("unknown".to_owned())
}

fn append_bytes<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    path: &str,
    data: &[u8],
) -> Result<(), std::io::Error> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, path, data)?;
    Ok(())
}

fn append_dir_recursive<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    prefix: &str,
) -> Result<(), std::io::Error> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let archive_path = format!("{prefix}/{name}");
        if path.is_dir() {
            append_dir_recursive(tar, &path, &archive_path)?;
        } else {
            let data = fs::read(&path)?;
            append_bytes(tar, &archive_path, &data)?;
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Wire into mod.rs**

Add to `crates/cairn-workflows/src/skillify/mod.rs`:

```rust
pub mod packer;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-workflows --no-fail-fast -- skillify_packer 2>&1 | tail -15`
Expected: all packer tests PASS

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/cairn-workflows/Cargo.toml crates/cairn-workflows/src/skillify/packer.rs crates/cairn-workflows/src/skillify/mod.rs crates/cairn-workflows/tests/skillify_packer.rs
git commit -m "feat(workflows): add SkillPackBuilder and archive packer (#128)"
```

---

## Task 10: `HealthCheckRunner` for daily re-gating

**Files:**
- Create: `crates/cairn-workflows/src/skillify/health.rs`
- Modify: `crates/cairn-workflows/src/skillify/mod.rs`

- [ ] **Step 1: Implement the health check runner**

Create `crates/cairn-workflows/src/skillify/health.rs`:

```rust
//! Daily health check for promoted skills.

use std::path::PathBuf;
use std::sync::Arc;

use cairn_core::contract::llm_provider::LLMProvider;
use cairn_core::pipeline::skillify::{
    SkillArtifactBundle, SkillLintSnapshot, SkillifyGateReport, SkillifyGateStatus,
};

use super::gate_registry::GateRunnerRegistry;
use super::gate_runner::GateRunContext;
use super::materialize::AuthoredSkillBundle;

/// Result of a health check on one promoted skill.
#[derive(Debug)]
pub struct HealthCheckResult {
    /// Candidate id.
    pub candidate_id: String,
    /// Whether all gates still pass.
    pub healthy: bool,
    /// Updated gate report.
    pub gate_report: SkillifyGateReport,
    /// Newly failed gate names.
    pub regressions: Vec<String>,
}

/// Runs health checks against promoted skills.
pub struct HealthCheckRunner {
    vault_root: PathBuf,
    llm: Option<Arc<dyn LLMProvider>>,
    gate_registry: GateRunnerRegistry,
}

impl HealthCheckRunner {
    /// Create a new health check runner.
    #[must_use]
    pub fn new(vault_root: PathBuf, llm: Option<Arc<dyn LLMProvider>>) -> Self {
        Self {
            vault_root,
            llm,
            gate_registry: GateRunnerRegistry::default_suite(),
        }
    }

    /// Run health check for one candidate.
    ///
    /// # Errors
    /// Returns on I/O or JSON failures.
    pub async fn check(
        &self,
        candidate_id: &str,
    ) -> Result<HealthCheckResult, Box<dyn std::error::Error + Send + Sync>> {
        let candidate_dir = self
            .vault_root
            .join(".cairn/evolution/skillify")
            .join(candidate_id);

        let bundle: SkillArtifactBundle = serde_json::from_slice(
            &std::fs::read(candidate_dir.join("manifest.json"))?,
        )?;

        let authored = reconstruct_authored(&candidate_dir, &bundle)?;
        let snapshot = SkillLintSnapshot { skills: vec![] };

        let ctx = GateRunContext {
            vault_root: &self.vault_root,
            candidate_id,
            candidate_dir: candidate_dir.clone(),
            bundle: &bundle,
            authored: &authored,
            llm: self.llm.as_deref(),
            snapshot: &snapshot,
        };

        let results = self.gate_registry.run_all(&ctx).await;

        let mut report = SkillifyGateReport {
            candidate_id: candidate_id.to_owned(),
            gates: Vec::new(),
        };
        let mut regressions = Vec::new();

        for result in &results {
            if result.status != SkillifyGateStatus::Passed {
                regressions.push(result.kind.as_str().to_owned());
            }
            report.gates.push(result.clone().into_gate());
        }

        std::fs::write(
            candidate_dir.join("gate-report.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;

        Ok(HealthCheckResult {
            candidate_id: candidate_id.to_owned(),
            healthy: regressions.is_empty(),
            gate_report: report,
            regressions,
        })
    }
}

fn reconstruct_authored(
    candidate_dir: &std::path::Path,
    bundle: &SkillArtifactBundle,
) -> Result<AuthoredSkillBundle, Box<dyn std::error::Error + Send + Sync>> {
    use cairn_core::pipeline::skillify::SkillArtifactKind;

    let read_artifact = |kind: SkillArtifactKind| -> Result<String, std::io::Error> {
        let artifact = bundle
            .artifacts
            .iter()
            .find(|a| a.kind == kind)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("missing {kind}")))?;
        std::fs::read_to_string(candidate_dir.join(&artifact.path))
    };

    let read_json = |kind: SkillArtifactKind| -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        let content = read_artifact(kind)?;
        Ok(serde_json::from_str(&content)?)
    };

    let slug = bundle
        .artifacts
        .iter()
        .find(|a| a.kind == SkillArtifactKind::SkillContract)
        .map(|a| {
            a.path
                .rsplit('/')
                .next()
                .unwrap_or("")
                .strip_prefix("skill_")
                .and_then(|s| s.strip_suffix(".md"))
                .unwrap_or("unknown")
                .to_owned()
        })
        .unwrap_or_else(|| "unknown".to_owned());

    let skill_markdown = read_artifact(SkillArtifactKind::SkillContract)?;
    let lane = skill_markdown
        .lines()
        .find_map(|l| l.strip_prefix("lane:").map(|v| v.trim().to_owned()))
        .unwrap_or_else(|| "unknown".to_owned());

    Ok(AuthoredSkillBundle {
        lane,
        slug,
        skill_markdown,
        script: read_artifact(SkillArtifactKind::DeterministicScript)?,
        unit_tests: read_json(SkillArtifactKind::UnitTests)?,
        integration_tests: read_json(SkillArtifactKind::IntegrationTests)?,
        llm_evals: read_json(SkillArtifactKind::LlmEvals)?,
        resolver_triggers: read_json(SkillArtifactKind::ResolverTrigger)?,
        resolver_eval: read_json(SkillArtifactKind::ResolverEval)?,
        smoke: read_json(SkillArtifactKind::E2eSmoke)?,
        filing_rules: read_json(SkillArtifactKind::FilingRules)?,
    })
}
```

- [ ] **Step 2: Wire into mod.rs**

Add to `crates/cairn-workflows/src/skillify/mod.rs`:

```rust
pub mod health;
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p cairn-workflows 2>&1 | tail -10`
Expected: compiles cleanly

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-workflows/src/skillify/health.rs crates/cairn-workflows/src/skillify/mod.rs
git commit -m "feat(workflows): add HealthCheckRunner for daily re-gating (#128)"
```

---

## Task 11: `cairn skillpack` CLI subcommands

**Files:**
- Modify: `crates/cairn-cli/src/verbs/mod.rs`
- Create: `crates/cairn-cli/src/verbs/skillpack.rs` (not a full file — wiring only)

This task wires the `cairn skillpack pack|install|inspect` subcommands. The
heavy lifting is in `cairn-workflows::skillify::packer`; the CLI just parses
args and calls through.

- [ ] **Step 1: Add the verb module**

In `crates/cairn-cli/src/verbs/mod.rs`, add:

```rust
pub mod skillpack;
```

- [ ] **Step 2: Implement the CLI verb**

Create `crates/cairn-cli/src/verbs/skillpack.rs`. This file parses clap args
and dispatches to `cairn_workflows::skillify::packer`. The exact shape depends
on the existing CLI patterns in `verbs/` — follow the same subcommand enum +
`run()` pattern used by other verbs (e.g. `lint.rs`, `flush.rs`).

Key subcommands:
- `pack --name <name> --version <ver> --cairn-compat <compat> --candidates <id1,id2,...> [--output <path>]`
- `install <path>`
- `inspect <path>`

Each dispatches to the corresponding function in `packer.rs` / `unpack_archive`.

- [ ] **Step 3: Wire into the top-level clap dispatch**

Add the `Skillpack` variant to the main CLI enum and wire the dispatch in
`main.rs` or `mod.rs` — follow the same pattern as existing verbs.

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p cairn-cli 2>&1 | tail -10`
Expected: compiles cleanly

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/skillpack.rs crates/cairn-cli/src/verbs/mod.rs
git commit -m "feat(cli): add cairn skillpack pack|install|inspect subcommands (#128)"
```

---

## Task 12: Full verification and supply-chain checks

**Files:** none (read-only)

- [ ] **Step 1: Run full workspace checks**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
```

- [ ] **Step 2: Fix any issues found**

Address clippy warnings, test failures, or core boundary violations.

- [ ] **Step 3: Final commit if needed**

```bash
git add -A
git commit -m "fix: address clippy and test issues for Skillify pipeline (#128)"
```

- [ ] **Step 4: Run supply chain checks**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Fix any new unused dependency warnings from `flate2`/`tar` if they arise.
