//! Scheduler handler for skill emission jobs.

use std::path::PathBuf;
use std::sync::Arc;

use cairn_core::contract::job_store::{JobKind, JobPayload};
use cairn_core::contract::llm_provider::{LLMProvider, LlmError};
use cairn_core::pipeline::skillify::SkillifyStage;

use crate::scheduler::{HandlerOutcome, JobHandler};

use super::materialize::{SkillifyMaterializeError, candidate_materialized};
use super::pipeline::{SkillifyPipeline, SkillifyPipelineError};

/// The `JobKind` discriminator stored in `workflow_jobs.kind`.
pub const SKILLIFY_KIND: &str = "skillify.emit";

/// LLM-authored skill bundle materialization handler.
pub struct SkillifyHandler {
    vault_root: PathBuf,
    llm: Option<Arc<dyn LLMProvider>>,
}

/// Error from one decoded skillify handler run.
#[derive(Debug, thiserror::Error)]
pub enum SkillifyRunError {
    /// No LLM provider was configured.
    #[error("skillify: no llm provider configured")]
    NoLlm,
    /// LLM provider failed.
    #[error(transparent)]
    Llm(#[from] LlmError),
    /// Provider returned text or another unsupported output shape.
    #[error("skillify: llm did not return JSON")]
    NonJsonOutput,
    /// Materialization or authored bundle validation failed.
    #[error(transparent)]
    Materialize(#[from] SkillifyMaterializeError),
}

impl SkillifyRunError {
    fn is_permanent(&self) -> bool {
        match self {
            Self::NoLlm | Self::NonJsonOutput => true,
            Self::Materialize(e) => e.is_permanent(),
            Self::Llm(e) => !matches!(e, LlmError::ProviderUnreachable { .. }),
        }
    }
}

impl SkillifyHandler {
    /// Construct a handler. Pass `llm = None` when no provider is configured;
    /// queued jobs fail permanently instead of retrying forever.
    #[must_use]
    pub fn new(vault_root: PathBuf, llm: Option<Arc<dyn LLMProvider>>) -> Self {
        Self { vault_root, llm }
    }

    /// Run one decoded skillify payload.
    ///
    /// # Errors
    /// Returns when no LLM is configured, the provider fails, the response is
    /// not valid skill bundle JSON, or bundle materialization fails.
    pub async fn run_once(&self, payload: super::SkillifyPayload) -> Result<(), SkillifyRunError> {
        let candidate_id = payload.candidate_id_or_derive();
        if candidate_materialized(&self.vault_root, &candidate_id)? {
            return Ok(());
        }

        let pipeline = SkillifyPipeline::new(self.vault_root.clone(), self.llm.clone());
        let result = pipeline.run(payload).await.map_err(|e| match e {
            SkillifyPipelineError::NoLlm => SkillifyRunError::NoLlm,
            SkillifyPipelineError::Llm(e) => SkillifyRunError::Llm(e),
            SkillifyPipelineError::Materialize(e) => SkillifyRunError::Materialize(e),
            SkillifyPipelineError::Io(e) => {
                SkillifyRunError::Materialize(SkillifyMaterializeError::Io(e))
            }
            SkillifyPipelineError::Json(e) => {
                SkillifyRunError::Materialize(SkillifyMaterializeError::Json(e))
            }
        })?;

        // Blocked means no LLM was available; the pipeline wrote a blocked marker.
        if result.final_stage == SkillifyStage::Blocked {
            return Err(SkillifyRunError::NoLlm);
        }

        // If the candidate bundle was successfully written to disk — which happens
        // when the Author stage completes — treat this as success even if subsequent
        // Gate or Promote stages failed. Gate failures leave the bundle materialized
        // but not promoted; the caller may retry or escalate via other workflows.
        if candidate_materialized(&self.vault_root, &candidate_id)? {
            return Ok(());
        }

        // The pipeline failed before materializing a bundle (extract or author error).
        let msg = result.errors.join("; ");
        Err(SkillifyRunError::Materialize(
            SkillifyMaterializeError::InvalidPathToken {
                label: "slug",
                value: msg,
            },
        ))
    }
}

#[async_trait::async_trait]
impl JobHandler for SkillifyHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(SKILLIFY_KIND)
    }

    async fn handle(&self, payload: &JobPayload) -> HandlerOutcome {
        let payload = match super::SkillifyPayload::from_bytes(payload) {
            Ok(payload) => payload,
            Err(e) => {
                return HandlerOutcome::validation_permanent(format!(
                    "invalid skillify payload: {e}"
                ));
            }
        };

        match self.run_once(payload).await {
            Ok(()) => HandlerOutcome::Done,
            Err(e) if e.is_permanent() => HandlerOutcome::validation_permanent(e.to_string()),
            Err(e) => HandlerOutcome::transient_retry(e.to_string()),
        }
    }
}
