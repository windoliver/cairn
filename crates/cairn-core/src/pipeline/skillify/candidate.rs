//! Skillify candidate data model and validation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How a skillify trajectory was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyTrigger {
    /// User or operator explicitly requested authoring.
    Explicit,
    /// Candidate discovered from Deep Dream source windows.
    DeepDream,
    /// Operator-initiated administrative candidate.
    ManualAdmin,
    /// Candidate selected during health recheck.
    HealthRecheck,
}

impl SkillifyTrigger {
    const fn as_str_name(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::DeepDream => "deep_dream",
            Self::ManualAdmin => "manual_admin",
            Self::HealthRecheck => "health_recheck",
        }
    }
}

/// Observed outcome for the source trajectory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyOutcome {
    /// Trajectory met its success criteria.
    Success,
    /// Trajectory failed and must not be authored.
    Failure,
    /// Trajectory outcome is not known yet.
    Unknown,
    /// Trajectory has not been verified and must not be authored.
    Unverified,
}

impl SkillifyOutcome {
    /// Returns true when this outcome may proceed to authoring.
    pub const fn is_eligible(self) -> bool {
        matches!(self, Self::Success)
    }
}

impl std::fmt::Display for SkillifyOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Unknown => "unknown",
            Self::Unverified => "unverified",
        };
        f.write_str(value)
    }
}

/// Lifecycle status for a skillify record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyStatus {
    /// Validated candidate awaiting authoring.
    Candidate,
    /// Candidate was blocked by validation or gate failures.
    Blocked,
    /// Candidate passed gates and is ready for human review.
    ReadyForReview,
    /// Candidate is live.
    Live,
    /// Live skill failed a health check.
    Unhealthy,
    /// Live skill was rolled back.
    RolledBack,
    /// Candidate or skill was archived.
    Archived,
}

/// Source material backing a skillify candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifySource {
    /// Source memory record id.
    pub record_id: String,
    /// Source kind, such as `trace` or `strategy_success`.
    pub kind: String,
    /// SHA-256 digest of the source body.
    pub body_sha256: String,
}

/// Raw candidate input before validation and id assignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifyCandidateInput {
    /// Selection trigger for this candidate.
    pub trigger: SkillifyTrigger,
    /// Skill lane, such as `deploy.hotfix`.
    pub lane: String,
    /// Natural-language triggers that should invoke the skill.
    pub triggers: Vec<String>,
    /// Source memory record ids used for stable identity.
    pub source_record_ids: Vec<String>,
    /// Source metadata available to later authoring stages.
    pub sources: Vec<SkillifySource>,
    /// Criteria that made the trajectory successful.
    pub success_criteria: Vec<String>,
    /// Required capabilities.
    pub requires: Vec<String>,
    /// Capabilities or artifacts this candidate provides.
    pub provides: Vec<String>,
    /// Observed trajectory outcome.
    pub outcome: SkillifyOutcome,
    /// Confidence score in the closed interval `[0.0, 1.0]`.
    pub confidence: f32,
}

impl SkillifyCandidateInput {
    /// Validate this input and assign a stable candidate id.
    ///
    /// # Errors
    /// Returns [`SkillifyCandidateReject`] when the trajectory is not
    /// eligible for authoring or required candidate fields are empty.
    pub fn into_candidate(self) -> Result<SkillifyCandidate, SkillifyCandidateReject> {
        if !self.outcome.is_eligible() {
            return Err(SkillifyCandidateReject::IneligibleOutcome {
                outcome: self.outcome.to_string(),
            });
        }
        validate_not_empty("lane", self.lane.trim())?;
        validate_vec_not_empty("triggers", &self.triggers)?;
        validate_vec_not_empty("source_record_ids", &self.source_record_ids)?;
        validate_vec_not_empty("success_criteria", &self.success_criteria)?;
        if self.confidence.is_nan() || !(0.0..=1.0).contains(&self.confidence) {
            return Err(SkillifyCandidateReject::InvalidConfidence {
                value: self.confidence.to_string(),
            });
        }

        let candidate_id = candidate_id(
            &self.lane,
            self.trigger,
            &self.source_record_ids,
            &self.success_criteria,
        );
        Ok(SkillifyCandidate {
            candidate_id,
            status: SkillifyStatus::Candidate,
            trigger: self.trigger,
            lane: self.lane,
            triggers: self.triggers,
            source_record_ids: self.source_record_ids,
            sources: self.sources,
            success_criteria: self.success_criteria,
            requires: self.requires,
            provides: self.provides,
            outcome: self.outcome,
            confidence: self.confidence,
        })
    }
}

/// Validated skillify candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifyCandidate {
    /// Stable candidate id with the `skc_` prefix.
    pub candidate_id: String,
    /// Candidate lifecycle status.
    pub status: SkillifyStatus,
    /// Selection trigger for this candidate.
    pub trigger: SkillifyTrigger,
    /// Skill lane, such as `deploy.hotfix`.
    pub lane: String,
    /// Natural-language triggers that should invoke the skill.
    pub triggers: Vec<String>,
    /// Source memory record ids used for stable identity.
    pub source_record_ids: Vec<String>,
    /// Source metadata available to later authoring stages.
    pub sources: Vec<SkillifySource>,
    /// Criteria that made the trajectory successful.
    pub success_criteria: Vec<String>,
    /// Required capabilities.
    pub requires: Vec<String>,
    /// Capabilities or artifacts this candidate provides.
    pub provides: Vec<String>,
    /// Observed trajectory outcome.
    pub outcome: SkillifyOutcome,
    /// Confidence score in the closed interval `[0.0, 1.0]`.
    pub confidence: f32,
}

/// Reason a skillify candidate was rejected before authoring.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillifyCandidateReject {
    /// The trajectory outcome cannot proceed to authoring.
    #[error("skillify candidate rejected: outcome {outcome} is not eligible for authoring")]
    IneligibleOutcome {
        /// Ineligible outcome string.
        outcome: String,
    },
    /// A required field had no usable value.
    #[error("skillify candidate rejected: {field} must not be empty")]
    EmptyField {
        /// Name of the empty field.
        field: &'static str,
    },
    /// Confidence was NaN or outside the closed unit interval.
    #[error("skillify candidate rejected: confidence {value} outside [0.0, 1.0]")]
    InvalidConfidence {
        /// Rejected confidence value.
        value: String,
    },
}

fn validate_not_empty(field: &'static str, value: &str) -> Result<(), SkillifyCandidateReject> {
    if value.is_empty() {
        Err(SkillifyCandidateReject::EmptyField { field })
    } else {
        Ok(())
    }
}

fn validate_vec_not_empty(
    field: &'static str,
    value: &[String],
) -> Result<(), SkillifyCandidateReject> {
    if value.is_empty() {
        Err(SkillifyCandidateReject::EmptyField { field })
    } else {
        Ok(())
    }
}

fn candidate_id(
    lane: &str,
    trigger: SkillifyTrigger,
    source_record_ids: &[String],
    success_criteria: &[String],
) -> String {
    let mut source_record_ids = source_record_ids.to_vec();
    source_record_ids.sort();
    let mut success_criteria = success_criteria.to_vec();
    success_criteria.sort();

    let mut hasher = Sha256::new();
    hash_field(&mut hasher, "lane", lane);
    hash_field(&mut hasher, "trigger", trigger.as_str_name());
    hash_list_start(&mut hasher, "source_record_ids", source_record_ids.len());
    for source_record_id in &source_record_ids {
        hash_part(&mut hasher, &source_record_id);
    }
    hash_list_start(&mut hasher, "success_criteria", success_criteria.len());
    for criterion in &success_criteria {
        hash_part(&mut hasher, &criterion);
    }
    format!("skc_{:x}", hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, label: &str, value: &str) {
    hash_part(hasher, label);
    hash_part(hasher, value);
}

fn hash_list_start(hasher: &mut Sha256, label: &str, len: usize) {
    hash_part(hasher, label);
    hasher.update(u64::try_from(len).unwrap_or(u64::MAX).to_le_bytes());
}

fn hash_part(hasher: &mut Sha256, value: &str) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value.as_bytes());
}
