//! Skillify pipeline state machine (brief §11.b stages 1-5).
//!
//! Pure state transitions. No I/O, no async.

use serde::{Deserialize, Serialize};

use super::artifact::{SkillArtifactBundle, SkillArtifactKind};
use super::gate::{SkillifyGate, SkillifyGateReport, SkillifyGateStatus};
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
            candidate_id: candidate_id.clone(),
            stage: SkillifyStage::Extract,
            spec: None,
            bundle: None,
            gate_report: SkillifyGateReport {
                candidate_id,
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
    pub fn advance_to_author(&mut self, spec: SkillSpecDraft) -> Result<(), SkillifyStageError> {
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

    /// Advance from Promote to `HealthCheck`.
    ///
    /// # Errors
    /// Returns [`SkillifyStageError::InvalidTransition`] if not at Promote.
    pub fn advance_to_health(&mut self, plan_ref: String) -> Result<(), SkillifyStageError> {
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
