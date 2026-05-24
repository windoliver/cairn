//! `EvolutionPayload` — input for one evolution state-machine run.

use cairn_core::contract::job_store::JobPayload;
use cairn_core::pipeline::evolution::{EvolutionArtifactRef, EvolutionGateResult, RollbackPlan};
use serde::{Deserialize, Serialize};

/// One enqueued evolution proposal decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionPayload {
    /// Stable proposal id.
    pub proposal_id: String,
    /// Artifact currently live before this proposal.
    pub previous_artifact: EvolutionArtifactRef,
    /// Candidate artifact being evaluated.
    pub candidate_artifact: EvolutionArtifactRef,
    /// Rollback plan that restores the previous artifact.
    pub rollback_plan: RollbackPlan,
    /// Gate results already produced by eval/privacy/version/canary workers.
    #[serde(default)]
    pub gates: Vec<EvolutionGateResult>,
    /// Canary rollout evidence, if a canary window was started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canary_ref: Option<String>,
    /// Failure evidence; when present the handler rolls back instead of promoting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canary_failure_ref: Option<String>,
    /// Reviewer identity for successful promotion. `None` means autonomous gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// Final decision evidence reference.
    pub decision_ref: String,
}

impl EvolutionPayload {
    /// Serialize to `JobPayload`.
    ///
    /// # Errors
    /// JSON encoding failure.
    pub fn to_bytes(&self) -> Result<JobPayload, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Deserialize from `JobPayload`.
    ///
    /// # Errors
    /// JSON decoding failure.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}
