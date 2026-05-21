//! Skillify promotion and rollback planning.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cairn_core::domain::flush_plan::{FlushMode, FlushPlan, PlanReason, PlannedMutation};
use cairn_core::domain::{Identity, ScopeTuple, TargetId};
use cairn_core::generated::common::Ulid;
use chrono::{Duration, SecondsFormat};

use super::materialize::validate_path_token;

/// Input for planning a candidate skill promotion.
#[derive(Debug, Clone)]
pub struct SkillifyPromotionInput {
    /// Stable candidate id.
    pub candidate_id: String,
    /// Existing skill target id that will evolve.
    pub skill_target_id: String,
    /// Evidence references backing the skillify decision.
    pub evidence_refs: Vec<String>,
    /// Number of passed gates in the gate report.
    pub gate_count: u32,
}

/// Pure promotion plan source for Skillify candidates.
pub struct SkillifyPlanSource;

impl SkillifyPlanSource {
    /// Build a human-review evolution plan for a ready Skillify candidate.
    ///
    /// # Errors
    /// Returns an error when the target id or issuer identity is invalid.
    pub fn plan_promotion(input: &SkillifyPromotionInput) -> Result<FlushPlan, String> {
        validate_path_token("candidate id", &input.candidate_id).map_err(|e| e.to_string())?;
        let target = TargetId::parse(input.skill_target_id.clone())
            .map_err(|e| format!("invalid skill target id: {e}"))?;
        let evidence = input
            .evidence_refs
            .iter()
            .map(|id| parse_ulid(id, "evidence ref"))
            .collect::<Result<Vec<_>, _>>()?;
        let issued_at = now_rfc3339();
        let expires_at = expires_at_rfc3339();
        Ok(FlushPlan {
            operation_id: stable_ulid(&input.candidate_id),
            issued_at,
            issuer: Identity::parse("agt:cairn-workflows:skillify-handler:v1")
                .map_err(|e| e.to_string())?,
            principal: None,
            scope: ScopeTuple::default(),
            mode: FlushMode::HumanReview,
            mutations: vec![PlannedMutation::Evolve {
                skill: target,
                diff_ref: PathBuf::from(format!(
                    ".cairn/evolution/skillify/{}/versions/v1/manifest.json",
                    input.candidate_id
                )),
            }],
            reason: PlanReason::Skillify {
                candidate_id: input.candidate_id.clone(),
                gate_count: input.gate_count,
            },
            source_events: evidence,
            target_hashes: BTreeMap::new(),
            dependencies: Vec::new(),
            expires_at,
            placeholder: true,
        })
    }
}

fn stable_ulid(seed: &str) -> Ulid {
    let hex = crate::synthetic::sha256_hex(seed.as_bytes());
    let suffix = &hex[..15].to_ascii_uppercase();
    Ulid(format!("01HQZX9F5N0{suffix}"))
}

fn parse_ulid(value: &str, label: &str) -> Result<Ulid, String> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|e| format!("invalid {label} `{value}`: {e}"))
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn expires_at_rfc3339() -> String {
    (chrono::Utc::now() + Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Secs, true)
}
