//! [`OpenAiCompatProvider`] — implements [`LLMProvider`] over `async-openai`.

use async_openai::{Client, config::OpenAIConfig};
use cairn_core::{
    config::LlmConfig,
    contract::version::{ContractVersion, VersionRange},
    contract::{
        CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities,
        LLMProviderPlugin, LlmError,
    },
};

use crate::config::to_openai_config;

/// OpenAI-compatible [`LLMProvider`] adapter.
pub struct OpenAiCompatProvider {
    /// Underlying async-openai HTTP client. Used by `complete()` in Tasks 6–8.
    // Fields wired in Tasks 6–8; dead_code until then.
    #[allow(dead_code)]
    client: Client<OpenAIConfig>,
    /// Model name resolved at construction time. Used by `complete()` in Tasks 6–8.
    #[allow(dead_code)]
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

    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        // Full implementation in Tasks 6–8.
        Err(LlmError::CapabilityMissing {
            capability: "not yet implemented".into(),
        })
    }
}
