//! Skillify promotion and rollback planning.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cairn_core::domain::flush_plan::{FlushMode, FlushPlan, PlanReason, PlannedMutation};
use cairn_core::domain::{Identity, ScopeTuple, TargetId};
use cairn_core::generated::common::Ulid;

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
    pub fn plan_promotion(input: SkillifyPromotionInput) -> Result<FlushPlan, String> {
        let target = TargetId::parse(input.skill_target_id.clone())
            .map_err(|e| format!("invalid skill target id: {e}"))?;
        let evidence = input
            .evidence_refs
            .iter()
            .cloned()
            .map(Ulid)
            .collect::<Vec<_>>();
        Ok(FlushPlan {
            operation_id: stable_ulid(&input.candidate_id),
            issued_at: "2026-05-20T00:00:00Z".to_owned(),
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
            expires_at: "2026-05-20T00:05:00Z".to_owned(),
            placeholder: false,
        })
    }
}

fn stable_ulid(seed: &str) -> Ulid {
    let hex = crate::synthetic::sha256_hex(seed.as_bytes());
    let suffix = &hex[..15].to_ascii_uppercase();
    Ulid(format!("01HQZX9F5N0{suffix}"))
}
