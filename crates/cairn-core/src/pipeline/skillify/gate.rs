//! Skillify gate report data model.

use serde::{Deserialize, Serialize};

use super::SkillArtifactKind;

/// Status for one skillify promotion gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyGateStatus {
    /// Gate passed.
    Passed,
    /// Gate failed.
    Failed,
    /// Gate could not run because a prerequisite blocked it.
    Blocked,
    /// Gate was skipped.
    Skipped,
}

/// Individual skillify promotion gate result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifyGate {
    /// Stable gate name.
    pub name: String,
    /// Gate status.
    pub status: SkillifyGateStatus,
    /// Optional human-readable gate detail.
    pub message: Option<String>,
}

/// Promotion gate report for a skillify candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifyGateReport {
    /// Candidate id this report belongs to.
    pub candidate_id: String,
    /// Gate results.
    pub gates: Vec<SkillifyGate>,
}

impl SkillifyGateReport {
    /// Returns true when every required gate is present and all reported gates passed.
    pub fn ready_for_promotion(&self) -> bool {
        self.gates
            .iter()
            .all(|gate| gate.status == SkillifyGateStatus::Passed)
            && SkillArtifactKind::required()
                .iter()
                .all(|required| self.gates.iter().any(|gate| gate.name == required.as_str()))
    }
}
