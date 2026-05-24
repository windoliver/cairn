//! Skillify 5-stage pipeline orchestrator.

use std::path::PathBuf;
use std::sync::Arc;

use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LlmError,
};
use cairn_core::pipeline::skillify::{
    SkillSpecDraft, SkillifyGateReport, SkillifyGateStatus, SkillifyPipelineState, SkillifyStage,
};

use super::SkillifyPayload;
use super::gate_registry::GateRunnerRegistry;
use super::gate_runner::GateRunContext;
use super::materialize::{
    AuthoredSkillBundle, SkillifyMaterializeError, materialize_blocked_candidate,
    materialize_bundle,
};
use super::planner::{SkillifyPlanSource, SkillifyPromotionInput};
use super::snapshot::build_vault_snapshot;

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
    #[allow(
        clippy::too_many_lines,
        reason = "linear 5-stage flow; each stage is documented in-line and splitting obscures the state transitions"
    )]
    pub async fn run(
        &self,
        payload: SkillifyPayload,
    ) -> Result<SkillifyPipelineResult, SkillifyPipelineError> {
        let start = std::time::Instant::now();
        let candidate_id = payload.candidate_id_or_derive();
        let mut state = SkillifyPipelineState::new(candidate_id.clone());
        let mut errors = Vec::new();

        // STAGE 1: Extract — fail-closed without LLM
        let Some(llm) = &self.llm else {
            materialize_blocked_candidate(
                &self.vault_root,
                &candidate_id,
                "llm provider not configured",
            )?;
            let _ = state.block("no LLM provider configured".to_owned());
            return Ok(Self::build_result(&state, errors, start));
        };

        let spec = match self.extract(llm, &payload).await {
            Ok(spec) => spec,
            // Transient LLM outages must propagate as a retriable error,
            // not be absorbed into a permanent "failed candidate". Without
            // this special case, a brief network blip during STAGE 1 burns
            // the candidate as Failed and the scheduler never retries.
            Err(SkillifyPipelineError::Llm(
                e @ cairn_core::contract::llm_provider::LlmError::ProviderUnreachable { .. },
            )) => {
                return Err(SkillifyPipelineError::Llm(e));
            }
            Err(e) => {
                errors.push(e.to_string());
                let _ = state.fail(e.to_string());
                return Ok(Self::build_result(&state, errors, start));
            }
        };

        let candidate_dir = self
            .vault_root
            .join(".cairn/evolution/skillify")
            .join(&candidate_id);

        let _ = state.advance_to_author(spec.clone());

        // STAGE 2: Author
        let authored = match self.author(llm, &spec).await {
            Ok(a) => a,
            // Same transient-error handling as STAGE 1: a brief LLM outage
            // during authoring must trigger a retry, not a permanent failure.
            Err(SkillifyPipelineError::Llm(
                e @ cairn_core::contract::llm_provider::LlmError::ProviderUnreachable { .. },
            )) => {
                return Err(SkillifyPipelineError::Llm(e));
            }
            Err(e) => {
                errors.push(e.to_string());
                let _ = state.fail(e.to_string());
                return Ok(Self::build_result(&state, errors, start));
            }
        };

        let payload_source_refs = payload.source_record_ids.clone();
        let bundle = materialize_bundle(
            &self.vault_root,
            &candidate_id,
            &authored,
            &payload_source_refs,
        )?;

        // Write the spec draft alongside the materialized bundle (informational).
        std::fs::write(
            candidate_dir.join("skill-spec.draft.json"),
            serde_json::to_vec_pretty(&spec)?,
        )?;

        let _ = state.advance_to_gate(bundle.clone());

        // STAGE 3: Gate
        // Build the real vault snapshot so collision-detection gates
        // (ResolverTrigger, CheckResolvableAndDry) see existing live skills
        // and other materialized candidates. Exclude the current candidate
        // from the snapshot so it does not collide with itself.
        //
        // Round 4 hardening: fail closed if the snapshot cannot be built.
        // Silently substituting an empty snapshot would let a candidate with
        // a colliding lane/trigger pass gates whenever `.cairn/...` is
        // temporarily unreadable, then route future intents to the wrong
        // skill.
        let snapshot = match build_vault_snapshot(&self.vault_root, Some(&candidate_id)) {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("vault snapshot build failed: {e}");
                errors.push(msg.clone());
                let _ = state.fail(msg);
                return Ok(Self::build_result(&state, errors, start));
            }
        };
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

        // Transient errors (e.g. LLM provider unreachable) must propagate as
        // a retriable error rather than a permanent gate failure. Surface
        // them BEFORE writing the gate report or advancing state so the
        // scheduler retries cleanly on the next tick.
        if let Some(transient) = results
            .iter()
            .find_map(|r| r.transient_error_detail.clone())
        {
            return Err(SkillifyPipelineError::Llm(
                cairn_core::contract::llm_provider::LlmError::ProviderUnreachable {
                    detail: transient,
                },
            ));
        }

        for result in &results {
            state.record_gate(result.clone().into_gate());
        }

        // Always persist the authoritative gate-report — failures must be
        // visible on disk so lint, the handler, and human reviewers can see
        // what failed and why. The initial "all blocked" marker from
        // materialize_bundle is replaced with the actual gate results.
        let gate_report = state.gate_report().clone();
        std::fs::write(
            candidate_dir.join("gate-report.json"),
            serde_json::to_vec_pretty(&gate_report)?,
        )?;

        let any_not_passed = results
            .iter()
            .any(|r| r.status != SkillifyGateStatus::Passed);

        if any_not_passed {
            let blocked_or_failed: Vec<String> = results
                .iter()
                .filter(|r| r.status != SkillifyGateStatus::Passed)
                .map(|r| r.kind.as_str().to_owned())
                .collect();
            let msg = format!("gates not passing: {}", blocked_or_failed.join(", "));
            errors.push(msg.clone());
            let _ = state.fail(msg);
            return Ok(Self::build_result(&state, errors, start));
        }

        // STAGE 4: Promote — build a durable FlushPlan and write it before
        // marking the candidate as promoted. The plan goes to a human-review
        // queue per design brief §11 (autonomous evolution requires a review
        // gate before merging skill changes into the live `skills/` set).
        //
        // The planner emits a `diff_ref` pointing at
        // `versions/v1/manifest.json` under the candidate dir. We materialize
        // that file here (copy of the candidate manifest) so the FlushPlan
        // references an artifact that actually exists on disk — without
        // this, the human-review apply step would fail to load the diff.
        let versions_dir = candidate_dir.join("versions/v1");
        std::fs::create_dir_all(&versions_dir)?;
        let bundle_manifest_json = serde_json::to_vec_pretty(&bundle)?;
        std::fs::write(versions_dir.join("manifest.json"), &bundle_manifest_json)?;

        let promotion_input = SkillifyPromotionInput {
            candidate_id: candidate_id.clone(),
            skill_target_id: derive_skill_target_id(&candidate_id),
            evidence_refs: payload_source_refs.clone(),
            gate_count: u32::try_from(results.len()).unwrap_or(u32::MAX),
        };
        let plan = SkillifyPlanSource::plan_promotion(&promotion_input).map_err(|e| {
            SkillifyPipelineError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("plan promotion: {e}"),
            ))
        })?;
        std::fs::write(
            candidate_dir.join("promotion-plan.json"),
            serde_json::to_vec_pretty(&plan)?,
        )?;
        let _ = state.advance_to_promote();

        Ok(Self::build_result(&state, errors, start))
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

        // Strict parse — if the LLM returns wrong-schema JSON, fail loudly
        // rather than silently synthesizing a degraded spec.
        let spec: SkillSpecDraft = serde_json::from_value(value)?;
        spec.validate().map_err(|e| {
            SkillifyPipelineError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            ))
        })?;
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
                spec.lane, spec.slug, spec.decision_tree
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
        state: &SkillifyPipelineState,
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

/// Derive a stable 26-char Crockford-base32 ULID for a brand-new skill
/// target, so the `FlushPlan` has a valid `TargetId` even when promoting a
/// candidate with no pre-existing target. Hex digits (0-9, A-F) are all valid
/// Crockford characters, so the SHA-256-derived suffix is safe to use
/// directly without remapping.
fn derive_skill_target_id(candidate_id: &str) -> String {
    let hex = crate::synthetic::sha256_hex(candidate_id.as_bytes());
    let suffix = hex
        .chars()
        .take(15)
        .collect::<String>()
        .to_ascii_uppercase();
    format!("01HQZX9F5N1{suffix}")
}
