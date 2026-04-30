//! Convert [`LlmConfig`] into an [`async_openai::config::OpenAIConfig`].

use async_openai::config::OpenAIConfig;
use cairn_core::config::LlmConfig;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Build an `OpenAIConfig` from Cairn's `LlmConfig`.
///
/// `base_url` defaults to `https://api.openai.com/v1` when not set.
/// `api_key` defaults to `"cairn"` (placeholder) when not set —
/// some providers (Ollama) require a non-empty key but ignore its value.
pub(crate) fn to_openai_config(config: &LlmConfig) -> OpenAIConfig {
    let base_url = config
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_BASE_URL)
        .to_owned();
    let api_key = config.api_key.clone().unwrap_or_else(|| "cairn".into());
    OpenAIConfig::new()
        .with_api_base(base_url)
        .with_api_key(api_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::config::Config as _;
    use cairn_core::config::LlmConfig;

    #[test]
    fn default_base_url_when_none() {
        let cfg = to_openai_config(&LlmConfig::default());
        assert_eq!(cfg.api_base(), DEFAULT_BASE_URL);
    }

    #[test]
    fn custom_base_url_propagated() {
        let config = LlmConfig {
            base_url: Some("http://localhost:11434/v1".into()),
            ..LlmConfig::default()
        };
        let cfg = to_openai_config(&config);
        assert_eq!(cfg.api_base(), "http://localhost:11434/v1");
    }
}
