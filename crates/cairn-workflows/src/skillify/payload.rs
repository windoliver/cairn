//! `SkillifyPayload` — what an enqueued skill emission job carries.

use cairn_core::contract::job_store::JobPayload;
use cairn_core::domain::ScopeTuple;
use serde::{Deserialize, Serialize};

/// Source trigger for a skill emission attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyTrigger {
    /// User- or caller-explicit trigger.
    Explicit,
    /// Deep-dream workflow trigger.
    DeepDream,
    /// Administrative manual trigger.
    ManualAdmin,
    /// Health-based recheck trigger.
    HealthRecheck,
}

impl SkillifyTrigger {
    /// Stable snake-case trigger label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::DeepDream => "deep_dream",
            Self::ManualAdmin => "manual_admin",
            Self::HealthRecheck => "health_recheck",
        }
    }
}

/// One enqueued skill emission request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifyPayload {
    /// Source trigger for this request.
    pub trigger: SkillifyTrigger,
    /// Logical key the handler uses to group and serialize work.
    pub key: String,
    /// Optional candidate bundle identifier selected by an upstream step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<String>,
    /// Bound scope (tenant / workspace / user / agent) verified by the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_scope: Option<ScopeTuple>,
    /// Source records feeding the candidate bundle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_record_ids: Vec<String>,
}

impl SkillifyPayload {
    /// Deterministic candidate id for this source-specific emission request.
    #[must_use]
    pub fn derived_candidate_id(&self) -> String {
        let scope_wire = self
            .bound_scope
            .as_ref()
            .map(ScopeTuple::canonical_wire)
            .unwrap_or_default();
        let mut source_record_ids = self.source_record_ids.clone();
        source_record_ids.sort();
        let material = format!(
            "{trigger}\0{scope_wire}\0{key}\0{sources}",
            trigger = self.trigger.as_str(),
            key = self.key,
            sources = source_record_ids.join(",")
        );
        format!("skc_{}", crate::synthetic::sha256_hex(material.as_bytes()))
    }

    /// Explicit candidate id, falling back to the deterministic source id.
    #[must_use]
    pub fn candidate_id_or_derive(&self) -> String {
        self.candidate_id
            .clone()
            .unwrap_or_else(|| self.derived_candidate_id())
    }

    /// Recommended `EnqueueRequest::queue_key` for this payload.
    #[must_use]
    pub fn recommended_queue_key(&self) -> String {
        let scope_wire = self
            .bound_scope
            .as_ref()
            .map(ScopeTuple::canonical_wire)
            .unwrap_or_default();
        format!(
            "skillify:{trigger}:{scope_wire}:{key}",
            trigger = self.trigger.as_str(),
            key = self.key
        )
    }

    /// Serialize to `JobPayload`.
    ///
    /// # Errors
    /// JSON encoding failure (effectively unreachable for this struct).
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
