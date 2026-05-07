//! Integration tests: `build_llm_provider` when no provider is configured.

use cairn_core::{
    config::{LlmConfig, LlmProvider},
    contract::LlmError,
};
use cairn_llm_openai_compat::build_llm_provider;

#[test]
fn no_provider_returns_not_configured() {
    let config = LlmConfig::default(); // provider is None
    let result = build_llm_provider(&config);
    assert!(result.is_err(), "expected Err(_), got Ok(_)");
    // SAFETY: guarded by the assert above.
    let err = result.err().expect("invariant: is_err checked above");
    assert!(
        matches!(err, LlmError::NotConfigured { .. }),
        "expected NotConfigured, got {err}"
    );
    assert!(err.to_string().contains("llm.not_configured"));
}

#[test]
fn provider_set_no_base_url_constructs_ok() {
    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        model: Some("gpt-4o-mini".into()),
        ..LlmConfig::default()
    };
    // No base_url → defaults to https://api.openai.com/v1
    // Does not make a network call — just constructs the client.
    assert!(build_llm_provider(&config).is_ok());
}

#[test]
fn provider_set_ollama_base_url_constructs_ok() {
    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some("http://localhost:11434/v1".into()),
        model: Some("llama3.2".into()),
        ..LlmConfig::default()
    };
    assert!(build_llm_provider(&config).is_ok());
}
