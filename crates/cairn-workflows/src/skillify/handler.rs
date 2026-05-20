//! Scheduler handler for skill emission jobs.

use std::path::PathBuf;
use std::sync::Arc;

use cairn_core::contract::job_store::{JobKind, JobPayload};
use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LlmError,
};

use crate::scheduler::{HandlerOutcome, JobHandler};

use super::materialize::{AuthoredSkillBundle, SkillifyMaterializeError, materialize_bundle};

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
        if super::materialize::candidate_ready(&self.vault_root, &candidate_id)? {
            return Ok(());
        }

        let Some(llm) = &self.llm else {
            return Err(SkillifyRunError::NoLlm);
        };

        let request = CompletionRequest::builder()
            .prompt(format!(
                "Create a section 11.b Skillify bundle for key {} with sources {:?}. Return JSON only.",
                payload.key, payload.source_record_ids
            ))
            .schema(serde_json::json!({
                "type": "object",
                "required": [
                    "lane",
                    "slug",
                    "skill_markdown",
                    "script",
                    "unit_tests",
                    "integration_tests",
                    "llm_evals",
                    "resolver_triggers",
                    "resolver_eval",
                    "smoke",
                    "filing_rules"
                ]
            }))
            .build();
        let value = match llm.complete(&request).await? {
            CompletionOutput::Json(value) => value,
            CompletionOutput::Text(_) => return Err(SkillifyRunError::NonJsonOutput),
            _ => return Err(SkillifyRunError::NonJsonOutput),
        };
        let authored = AuthoredSkillBundle::try_from(value)?;
        materialize_bundle(
            &self.vault_root,
            &candidate_id,
            &authored,
            &payload.source_record_ids,
        )?;
        Ok(())
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
