//! [`OpenAiCompatProvider`] — implements [`LLMProvider`] over `async-openai`.

use std::time::Duration;

use async_openai::{
    Client,
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs, FinishReason,
        ResponseFormat, ResponseFormatJsonSchema,
    },
};
use backoff::ExponentialBackoffBuilder;
use cairn_core::{
    config::LlmConfig,
    contract::version::{ContractVersion, VersionRange},
    contract::{
        CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities,
        LLMProviderPlugin, LlmError,
    },
};

use crate::config::to_openai_config;
use crate::retry::{RetryPolicy, with_retries};

/// OpenAI-compatible [`LLMProvider`] adapter.
pub struct OpenAiCompatProvider {
    /// Underlying async-openai HTTP client.
    client: Client<OpenAIConfig>,
    /// Model name resolved at construction time.
    model: String,
    /// Static capability advertisement.
    capabilities: LLMProviderCapabilities,
}

/// Backoff with `max_elapsed_time = 0` so async-openai's internal retry
/// loop returns immediately — our `RetryPolicy` is the sole retry layer.
fn no_inner_backoff() -> backoff::ExponentialBackoff {
    ExponentialBackoffBuilder::new()
        .with_max_elapsed_time(Some(Duration::from_millis(0)))
        .build()
}

impl OpenAiCompatProvider {
    /// Construct from a resolved [`LlmConfig`].
    pub fn from_config(config: &LlmConfig) -> Result<Self, LlmError> {
        let openai_cfg = to_openai_config(config);
        let model = config.model.clone().unwrap_or_else(|| "gpt-4o-mini".into());
        Ok(Self {
            client: Client::with_config(openai_cfg).with_backoff(no_inner_backoff()),
            model,
            capabilities: LLMProviderCapabilities {
                json_mode: true,
                streaming: false,
                tool_calls: false,
            },
        })
    }

    /// Test-only constructor with explicit capabilities.
    ///
    /// # Stability
    /// Only available with the `testing` feature. Semver-exempt — do not rely
    /// on this in production code.
    #[cfg(feature = "testing")]
    #[must_use]
    pub fn with_capabilities(
        base_url: &str,
        model: &str,
        capabilities: LLMProviderCapabilities,
    ) -> Self {
        let openai_cfg = OpenAIConfig::new()
            .with_api_base(base_url)
            .with_api_key("test-key");
        Self {
            client: Client::with_config(openai_cfg).with_backoff(no_inner_backoff()),
            model: model.into(),
            capabilities,
        }
    }
}

impl LLMProviderPlugin for OpenAiCompatProvider {
    const NAME: &'static str = "openai-compatible";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

#[async_trait::async_trait]
impl LLMProvider for OpenAiCompatProvider {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn capabilities(&self) -> &LLMProviderCapabilities {
        &self.capabilities
    }

    fn supported_contract_versions(&self) -> VersionRange {
        Self::SUPPORTED_VERSIONS
    }

    #[tracing::instrument(skip(self, req), err, fields(model, schema_mode = req.schema.is_some()))]
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        // JSON schema path requires json_mode capability — guard it now,
        // full validation wired in Task 7.
        if req.schema.is_some() && !self.capabilities.json_mode {
            return Err(LlmError::CapabilityMissing {
                capability: "json_mode".into(),
            });
        }

        // Choose the model: per-request override or the instance default.
        let model = req.model.as_deref().unwrap_or(&self.model);
        tracing::Span::current().record("model", model);

        // Build the user message from the prompt string.
        let user_msg = ChatCompletionRequestUserMessageArgs::default()
            .content(req.prompt.as_str())
            .build()
            .map_err(|e| LlmError::ProviderUnreachable {
                detail: e.to_string(),
            })?;

        // Build the chat completion request.
        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(model).messages([user_msg.into()]);

        // Apply token budget when set.
        if let Some(budget) = &req.budget
            && let Some(max_tok) = budget.max_tokens
        {
            builder.max_completion_tokens(max_tok);
        }

        // JSON schema path: pass the caller's schema to the endpoint so it returns
        // structured JSON conforming to that schema.
        if let Some(schema) = &req.schema {
            builder.response_format(ResponseFormat::JsonSchema {
                json_schema: ResponseFormatJsonSchema {
                    name: "output".to_string(),
                    description: None,
                    schema: Some(schema.clone()),
                    strict: Some(true),
                },
            });
        }

        let request = builder.build().map_err(|e| LlmError::ProviderUnreachable {
            detail: e.to_string(),
        })?;

        // Retry transient failures (429, 5xx). The `request` clone is cheap —
        // it's `serde_json::Value` under the hood.
        let response = with_retries(RetryPolicy::standard(), || {
            let req = request.clone();
            let client = &self.client;
            async move { client.chat().create(req).await }
        })
        .await?;

        // Extract the first choice.
        let choice =
            response
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| LlmError::ProviderUnreachable {
                    detail: "provider returned empty choices".into(),
                })?;

        // Treat a truncated response as a budget overrun.
        if matches!(choice.finish_reason, Some(FinishReason::Length)) {
            return Err(LlmError::BudgetExceeded);
        }

        let content = choice
            .message
            .content
            .ok_or_else(|| LlmError::ProviderUnreachable {
                detail: "provider returned null content".into(),
            })?;

        // JSON schema path: parse and validate the response body.
        if let Some(schema) = &req.schema {
            // Parse the response as JSON.
            let value: serde_json::Value =
                serde_json::from_str(&content).map_err(|e| LlmError::InvalidJsonOutput {
                    detail: e.to_string(),
                    raw: content.clone(),
                })?;

            // Compile the schema and validate the parsed value.
            let compiled =
                jsonschema::validator_for(schema).map_err(|e| LlmError::InvalidJsonOutput {
                    detail: format!("invalid schema: {e}"),
                    raw: content.clone(),
                })?;

            compiled
                .validate(&value)
                .map_err(|e| LlmError::InvalidJsonOutput {
                    detail: e.to_string(),
                    raw: content.clone(),
                })?;

            return Ok(CompletionOutput::Json(value));
        }

        Ok(CompletionOutput::Text(content))
    }
}

#[cfg(test)]
mod tests {
    use cairn_core::contract::LlmError;

    #[test]
    fn lm_error_display_snapshots() {
        insta::assert_snapshot!(
            "not_configured",
            LlmError::NotConfigured {
                remediation: "cairn config set llm.provider ollama".into(),
            }
            .to_string()
        );
        insta::assert_snapshot!("auth_denied", LlmError::AuthDenied.to_string());
        insta::assert_snapshot!("budget_exceeded", LlmError::BudgetExceeded.to_string());
        insta::assert_snapshot!(
            "capability_missing",
            LlmError::CapabilityMissing {
                capability: "json_mode".into(),
            }
            .to_string()
        );
    }
}
