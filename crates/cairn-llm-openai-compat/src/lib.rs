//! OpenAI-compatible [`LLMProvider`] adapter (ADR 0001, brief §4.0).
//!
//! Entry point: [`build_llm_provider`].

mod config;
mod error;
mod provider;
mod retry;

pub use provider::OpenAiCompatProvider;

use cairn_core::{
    config::LlmConfig,
    contract::{LLMProvider, LlmError},
};

/// Construct a boxed [`LLMProvider`] from the resolved config.
///
/// Returns [`LlmError::NotConfigured`] immediately (no network call) when
/// `config.provider` is `None`.
pub fn build_llm_provider(config: &LlmConfig) -> Result<Box<dyn LLMProvider>, LlmError> {
    if config.provider.is_none() {
        return Err(LlmError::NotConfigured {
            remediation: "cairn config set llm.provider ollama".into(),
        });
    }
    let provider = provider::OpenAiCompatProvider::from_config(config)?;
    Ok(Box::new(provider))
}
