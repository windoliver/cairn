//! Skillify 5-stage pipeline orchestrator.

use std::path::PathBuf;
use std::sync::Arc;

use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LlmError,
};
use cairn_core::pipeline::skillify::{
    SkillLintSnapshot, SkillSpecDraft, SkillifyGateReport, SkillifyGateStatus,
    SkillifyPipelineState, SkillifyStage,
};

use super::SkillifyPayload;
use super::gate_registry::GateRunnerRegistry;
use super::gate_runner::GateRunContext;
use super::materialize::{
    AuthoredSkillBundle, SkillifyMaterializeError, materialize_blocked_candidate,
    materialize_bundle,
};

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
            Err(e) => {
                errors.push(e.to_string());
                let _ = state.fail(e.to_string());
                return Ok(Self::build_result(&state, errors, start));
            }
        };

        let bundle = materialize_bundle(
            &self.vault_root,
            &candidate_id,
            &authored,
            &payload.source_record_ids,
        )?;

        // Write the spec draft alongside the materialized bundle (informational).
        std::fs::write(
            candidate_dir.join("skill-spec.draft.json"),
            serde_json::to_vec_pretty(&spec)?,
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

        // STAGE 4: Promote
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
