//! Evolution workflow primitives (brief §11 and §11.3).
//!
//! This module is pure data plus state-transition validation. Workflow and
//! store crates own I/O, durable job dispatch, and side-effect application.

use serde::{Deserialize, Serialize};

/// Evolvable artifact classes from brief §11.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EvolutionArtifactKind {
    /// `skills/<skill>.md`.
    Skill,
    /// Tool registry metadata / descriptions.
    ToolDescription,
    /// System prompt fragments.
    Prompt,
    /// Classifiers or routing rules.
    Classifier,
    /// Pipeline configuration files.
    PipelineConfig,
}

/// Versioned artifact reference carried through proposal lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionArtifactRef {
    /// Artifact class.
    pub kind: EvolutionArtifactKind,
    /// Stable artifact id.
    pub artifact_id: String,
    /// Monotonic artifact version.
    pub version: u32,
    /// Content digest of the referenced version.
    pub content_sha256: String,
}

/// Rollback plan required before a candidate may be promoted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackPlan {
    /// Stable rollback plan id.
    pub plan_id: String,
    /// Artifact restored when the candidate is abandoned.
    pub restores_artifact: EvolutionArtifactRef,
    /// Evidence proving rollback was planned or dry-run.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

/// Required and optional promotion gates for evolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EvolutionGateKind {
    /// Evaluation result gate.
    Eval,
    /// Privacy and consent gate.
    Privacy,
    /// Version compatibility gate.
    Version,
    /// Rollback plan gate.
    RollbackPlan,
    /// Canary rollout gate.
    Canary,
    /// Human or autonomous review gate.
    Review,
    /// Held-out adversarial dataset gate.
    HeldOutAdversarial,
    /// Shared-tier consent gate.
    SharedTier,
}

/// Status for one evolution gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvolutionGateStatus {
    /// Gate passed.
    Passed,
    /// Gate failed.
    Failed,
    /// Gate could not run because another gate blocked it.
    Blocked,
    /// Gate was intentionally skipped for this contract phase.
    Skipped,
}

/// One gate result and its supporting evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionGateResult {
    /// Gate kind.
    pub kind: EvolutionGateKind,
    /// Gate status.
    pub status: EvolutionGateStatus,
    /// Optional operator-facing detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Evidence references supporting this gate result.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

/// Gate report for one proposal.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionGateReport {
    /// Gate results.
    #[serde(default)]
    pub gates: Vec<EvolutionGateResult>,
}

impl EvolutionGateReport {
    /// Gate subset #127 requires before promotion.
    pub const REQUIRED_FOR_PROMOTION: &'static [EvolutionGateKind] = &[
        EvolutionGateKind::Eval,
        EvolutionGateKind::Privacy,
        EvolutionGateKind::Version,
        EvolutionGateKind::RollbackPlan,
        EvolutionGateKind::Canary,
    ];

    /// Add or replace a gate result by kind.
    pub fn record(&mut self, gate: EvolutionGateResult) {
        if let Some(existing) = self
            .gates
            .iter_mut()
            .find(|existing| existing.kind == gate.kind)
        {
            *existing = gate;
        } else {
            self.gates.push(gate);
        }
        self.gates.sort_by_key(|gate| gate.kind);
    }

    /// Promotion blockers split into missing and present-but-not-passing gates.
    #[must_use]
    pub fn promotion_blockers(&self) -> (Vec<EvolutionGateKind>, Vec<EvolutionGateKind>) {
        let mut missing = Vec::new();
        let mut failed = Vec::new();

        for required in Self::REQUIRED_FOR_PROMOTION {
            match self.gates.iter().find(|gate| gate.kind == *required) {
                Some(gate) if gate.status == EvolutionGateStatus::Passed => {}
                Some(_) => failed.push(*required),
                None => missing.push(*required),
            }
        }

        (missing, failed)
    }

    /// Returns true when every configured promotion gate passed.
    #[must_use]
    pub fn ready_for_promotion(&self) -> bool {
        let (missing, failed) = self.promotion_blockers();
        missing.is_empty() && failed.is_empty()
    }

    fn evidence_for(&self, kinds: &[EvolutionGateKind]) -> Vec<String> {
        let mut evidence = Vec::new();
        for gate in &self.gates {
            if kinds.contains(&gate.kind) {
                evidence.extend(gate.evidence_refs.iter().cloned());
            }
        }
        evidence
    }
}

/// Evolution workflow lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvolutionState {
    /// Proposal has been staged but not evaluated.
    Proposed,
    /// Proposal is being evaluated.
    Evaluating,
    /// Candidate is live only in a canary window.
    Canarying,
    /// Candidate was promoted.
    Promoted,
    /// Candidate failed canary or apply and was rolled back.
    RolledBack,
    /// Candidate failed pre-canary gates.
    Rejected,
}

/// Durable lineage for an evolution proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionLineage {
    /// Stable proposal id.
    pub proposal_id: String,
    /// Artifact version before the proposal.
    pub previous_artifact: EvolutionArtifactRef,
    /// Proposed artifact version.
    pub proposed_artifact: EvolutionArtifactRef,
    /// Eval and canary result evidence.
    #[serde(default)]
    pub eval_result_refs: Vec<String>,
    /// Promoted artifact, if promotion happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_artifact: Option<EvolutionArtifactRef>,
    /// Decision references such as approval, rejection, or rollback evidence.
    #[serde(default)]
    pub decision_evidence: Vec<String>,
}

/// In-memory evolution state machine for one proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvolutionRun {
    proposal_id: String,
    previous_artifact: EvolutionArtifactRef,
    candidate_artifact: EvolutionArtifactRef,
    rollback_plan: RollbackPlan,
    gate_report: EvolutionGateReport,
    state: EvolutionState,
    active_artifact: EvolutionArtifactRef,
    promoted_artifact: Option<EvolutionArtifactRef>,
    decision_evidence: Vec<String>,
}

impl EvolutionRun {
    /// Create a staged proposal.
    ///
    /// # Errors
    /// Returns when required identity fields are empty, the candidate is not
    /// a new version of the same artifact, or the rollback plan restores a
    /// different previous artifact.
    pub fn new(
        proposal_id: impl Into<String>,
        previous_artifact: EvolutionArtifactRef,
        candidate_artifact: EvolutionArtifactRef,
        rollback_plan: RollbackPlan,
    ) -> Result<Self, EvolutionTransitionError> {
        let proposal_id = proposal_id.into();
        validate_not_empty("proposal_id", &proposal_id)?;
        validate_artifact_ref(&previous_artifact)?;
        validate_artifact_ref(&candidate_artifact)?;
        validate_not_empty("rollback_plan.plan_id", &rollback_plan.plan_id)?;
        if previous_artifact.kind != candidate_artifact.kind
            || previous_artifact.artifact_id != candidate_artifact.artifact_id
        {
            return Err(EvolutionTransitionError::InvalidProposal {
                field: "candidate_artifact".to_owned(),
                reason: "candidate must evolve the same artifact id and kind".to_owned(),
            });
        }
        if candidate_artifact.version <= previous_artifact.version {
            return Err(EvolutionTransitionError::InvalidProposal {
                field: "candidate_artifact.version".to_owned(),
                reason: "candidate version must be greater than previous version".to_owned(),
            });
        }
        if rollback_plan.restores_artifact != previous_artifact {
            return Err(EvolutionTransitionError::InvalidProposal {
                field: "rollback_plan.restores_artifact".to_owned(),
                reason: "rollback plan must restore the previous artifact".to_owned(),
            });
        }

        Ok(Self {
            proposal_id,
            active_artifact: previous_artifact.clone(),
            previous_artifact,
            candidate_artifact,
            rollback_plan,
            gate_report: EvolutionGateReport::default(),
            state: EvolutionState::Proposed,
            promoted_artifact: None,
            decision_evidence: Vec::new(),
        })
    }

    /// Current state.
    #[must_use]
    pub const fn state(&self) -> EvolutionState {
        self.state
    }

    /// Artifact currently visible to non-canary traffic.
    #[must_use]
    pub const fn active_artifact(&self) -> &EvolutionArtifactRef {
        &self.active_artifact
    }

    /// Decision evidence accumulated by transitions.
    #[must_use]
    pub fn decision_evidence(&self) -> &[String] {
        &self.decision_evidence
    }

    /// Gate report accumulated so far.
    #[must_use]
    pub const fn gate_report(&self) -> &EvolutionGateReport {
        &self.gate_report
    }

    /// Rollback plan for this proposal.
    #[must_use]
    pub const fn rollback_plan(&self) -> &RollbackPlan {
        &self.rollback_plan
    }

    /// Promoted artifact, if any.
    #[must_use]
    pub const fn promoted_artifact(&self) -> Option<&EvolutionArtifactRef> {
        self.promoted_artifact.as_ref()
    }

    /// Proposal id.
    #[must_use]
    pub fn proposal_id(&self) -> &str {
        &self.proposal_id
    }

    /// Add or replace one gate result.
    pub fn record_gate(&mut self, gate: EvolutionGateResult) {
        self.gate_report.record(gate);
    }

    /// Add or replace all gate results from `report`.
    pub fn extend_gates(&mut self, report: EvolutionGateReport) {
        for gate in report.gates {
            self.record_gate(gate);
        }
    }

    /// Return missing and failed promotion gates.
    #[must_use]
    pub fn promotion_blockers(&self) -> (Vec<EvolutionGateKind>, Vec<EvolutionGateKind>) {
        self.gate_report.promotion_blockers()
    }

    /// Start the canary window.
    ///
    /// # Errors
    /// Returns if the run is already terminal.
    pub fn start_canary(&mut self, evidence_ref: &str) -> Result<(), EvolutionTransitionError> {
        self.ensure_not_terminal(EvolutionState::Canarying)?;
        validate_not_empty("canary evidence", evidence_ref)?;
        self.state = EvolutionState::Canarying;
        push_unique(&mut self.decision_evidence, evidence_ref);
        Ok(())
    }

    /// Roll back after a canary failure.
    ///
    /// # Errors
    /// Returns if the run is not currently canarying or evidence is empty.
    pub fn fail_canary(&mut self, evidence_ref: &str) -> Result<(), EvolutionTransitionError> {
        if self.state != EvolutionState::Canarying {
            return Err(EvolutionTransitionError::InvalidTransition {
                from: self.state,
                to: EvolutionState::RolledBack,
            });
        }
        validate_not_empty("canary failure evidence", evidence_ref)?;
        self.state = EvolutionState::RolledBack;
        self.active_artifact = self.rollback_plan.restores_artifact.clone();
        self.promoted_artifact = None;
        push_unique(&mut self.decision_evidence, evidence_ref);
        Ok(())
    }

    /// Reject a proposal before promotion.
    ///
    /// # Errors
    /// Returns if the run is already terminal or evidence is empty.
    pub fn reject(&mut self, evidence_ref: &str) -> Result<(), EvolutionTransitionError> {
        self.ensure_not_terminal(EvolutionState::Rejected)?;
        validate_not_empty("rejection evidence", evidence_ref)?;
        self.state = EvolutionState::Rejected;
        self.active_artifact = self.rollback_plan.restores_artifact.clone();
        self.promoted_artifact = None;
        push_unique(&mut self.decision_evidence, evidence_ref);
        Ok(())
    }

    /// Promote the candidate artifact.
    ///
    /// # Errors
    /// Returns if configured gates are missing or failed, if this run is
    /// terminal, or if decision evidence is empty.
    pub fn promote(
        &mut self,
        reviewer: &str,
        decision_ref: &str,
    ) -> Result<(), EvolutionTransitionError> {
        self.ensure_not_terminal(EvolutionState::Promoted)?;
        validate_not_empty("reviewer", reviewer)?;
        validate_not_empty("decision evidence", decision_ref)?;
        let (missing, failed) = self.gate_report.promotion_blockers();
        if !missing.is_empty() || !failed.is_empty() {
            return Err(EvolutionTransitionError::PromotionBlocked { missing, failed });
        }

        for evidence_ref in self.gate_report.evidence_for(&[EvolutionGateKind::Canary]) {
            push_unique(&mut self.decision_evidence, &evidence_ref);
        }
        push_unique(&mut self.decision_evidence, decision_ref);
        self.state = EvolutionState::Promoted;
        self.active_artifact = self.candidate_artifact.clone();
        self.promoted_artifact = Some(self.candidate_artifact.clone());
        Ok(())
    }

    /// Build durable lineage for persistence.
    #[must_use]
    pub fn lineage(&self) -> EvolutionLineage {
        EvolutionLineage {
            proposal_id: self.proposal_id.clone(),
            previous_artifact: self.previous_artifact.clone(),
            proposed_artifact: self.candidate_artifact.clone(),
            eval_result_refs: self
                .gate_report
                .evidence_for(&[EvolutionGateKind::Eval, EvolutionGateKind::Canary]),
            promoted_artifact: self.promoted_artifact.clone(),
            decision_evidence: self.decision_evidence.clone(),
        }
    }

    fn ensure_not_terminal(&self, to: EvolutionState) -> Result<(), EvolutionTransitionError> {
        if matches!(
            self.state,
            EvolutionState::Promoted | EvolutionState::RolledBack | EvolutionState::Rejected
        ) {
            return Err(EvolutionTransitionError::InvalidTransition {
                from: self.state,
                to,
            });
        }
        Ok(())
    }
}

/// Transition or validation error for one evolution run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EvolutionTransitionError {
    /// Proposal data was malformed.
    #[error("invalid evolution proposal field `{field}`: {reason}")]
    InvalidProposal {
        /// Field name.
        field: String,
        /// Rejection reason.
        reason: String,
    },
    /// Promotion gates are not satisfied.
    #[error("evolution promotion blocked: missing={missing:?} failed={failed:?}")]
    PromotionBlocked {
        /// Required gates with no result.
        missing: Vec<EvolutionGateKind>,
        /// Required gates with a non-passing result.
        failed: Vec<EvolutionGateKind>,
    },
    /// State transition is not legal.
    #[error("invalid evolution transition {from:?} -> {to:?}")]
    InvalidTransition {
        /// Current state.
        from: EvolutionState,
        /// Requested state.
        to: EvolutionState,
    },
}

fn validate_artifact_ref(artifact: &EvolutionArtifactRef) -> Result<(), EvolutionTransitionError> {
    validate_not_empty("artifact_id", &artifact.artifact_id)?;
    validate_not_empty("content_sha256", &artifact.content_sha256)?;
    if !artifact.content_sha256.starts_with("sha256:") {
        return Err(EvolutionTransitionError::InvalidProposal {
            field: "content_sha256".to_owned(),
            reason: "digest must start with sha256:".to_owned(),
        });
    }
    Ok(())
}

fn validate_not_empty(field: &'static str, value: &str) -> Result<(), EvolutionTransitionError> {
    if value.trim().is_empty() {
        Err(EvolutionTransitionError::InvalidProposal {
            field: field.to_owned(),
            reason: "must not be empty".to_owned(),
        })
    } else {
        Ok(())
    }
}

fn push_unique(target: &mut Vec<String>, value: &str) {
    if !target.iter().any(|existing| existing == value) {
        target.push(value.to_owned());
    }
}
