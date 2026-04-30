//! [`OpenAiCompatProvider`] — implements [`LLMProvider`] over `async-openai`.

use async_openai::{
    Client,
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs,
    },
};
use cairn_core::{
    config::LlmConfig,
    contract::version::{ContractVersion, VersionRange},
    contract::{
        CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities,
        LLMProviderPlugin, LlmError,
    },
};

use crate::config::to_openai_config;
use crate::error::map_openai_error;

/// OpenAI-compatible [`LLMProvider`] adapter.
pub struct OpenAiCompatProvider {
    /// Underlying async-openai HTTP client.
    client: Client<OpenAIConfig>,
    /// Model name resolved at construction time.
    model: String,
    /// Static capability advertisement.
    capabilities: LLMProviderCapabilities,
}

impl OpenAiCompatProvider {
    /// Construct from a resolved [`LlmConfig`].
    pub fn from_config(config: &LlmConfig) -> Result<Self, LlmError> {
        let openai_cfg = to_openai_config(config);
        let model = config.model.clone().unwrap_or_else(|| "gpt-4o-mini".into());
        Ok(Self {
            client: Client::with_config(openai_cfg),
            model,
            capabilities: LLMProviderCapabilities {
                json_mode: true,
                streaming: false,
                tool_calls: false,
            },
        })
    }

    /// Test-only constructor with explicit capabilities.
    // Used in Tasks 6–8 integration tests.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn with_capabilities(
        base_url: &str,
        model: &str,
        capabilities: LLMProviderCapabilities,
    ) -> Self {
        let openai_cfg = OpenAIConfig::new()
            .with_api_base(base_url)
            .with_api_key("test-key");
        Self {
            client: Client::with_config(openai_cfg),
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
        if let Some(budget) = &req.budget && let Some(max_tok) = budget.max_tokens {
            builder.max_completion_tokens(max_tok);
        }

        // JSON schema path — Task 7 wires full structured-output validation.
        if req.schema.is_some() {
            todo!("JSON schema enforcement — implemented in Task 7")
        }

        let request = builder.build().map_err(|e| LlmError::ProviderUnreachable {
            detail: e.to_string(),
        })?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|e| map_openai_error(&e))?;

        // Extract content from the first choice.
        let content = response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| LlmError::ProviderUnreachable {
                detail: "provider returned empty choices or null content".into(),
            })?;

        Ok(CompletionOutput::Text(content))
    }
}
