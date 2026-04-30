//! `LLMProvider` contract (brief §4 row 2).
//!
//! P0 scaffold: surface only. The single `complete(prompt, schema?)` method
//! and structured-output enforcement arrive in #144.

use crate::config::ExtractBudget;
use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `LLMProvider`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

/// Errors returned by [`LLMProvider::complete`] (ADR 0001 error codes).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LlmError {
    /// No provider configured; LLM-dependent verbs fail closed (exit 78).
    #[error("llm.not_configured: {remediation}")]
    NotConfigured {
        /// Suggested remediation command, e.g. `cairn config set llm.provider ollama`.
        remediation: String,
    },
    /// Provider host/port refused connection or DNS failed or timed out.
    #[error("llm.provider_unreachable: {detail}")]
    ProviderUnreachable {
        /// Error detail from the transport layer.
        detail: String,
    },
    /// Provider returned HTTP 401 or 403.
    #[error("llm.auth_denied")]
    AuthDenied,
    /// Provider is reachable but lacks a required capability (e.g. `json_mode`).
    #[error("llm.capability_missing: {capability}")]
    CapabilityMissing {
        /// Name of the missing capability, e.g. `json_mode`.
        capability: String,
    },
    /// Provider returned output that failed JSON parse or schema validation.
    #[error("llm.invalid_json_output: {detail}")]
    InvalidJsonOutput {
        /// Validation or parse error message.
        detail: String,
        /// Raw string returned by the model.
        raw: String,
    },
    /// Completion exceeded the configured token or time budget.
    #[error("llm.budget_exceeded")]
    BudgetExceeded,
}

/// Input to [`LLMProvider::complete`].
#[derive(Debug, Clone, bon::Builder)]
pub struct CompletionRequest {
    /// The user/system prompt to send to the model.
    pub prompt: String,
    /// Optional JSON Schema. When `Some`, triggers JSON-mode enforcement:
    /// the adapter sends `response_format: json_schema` and validates the
    /// returned value against this schema before returning.
    pub schema: Option<serde_json::Value>,
    /// Override the model configured in `LlmConfig`. `None` uses the default.
    pub model: Option<String>,
    /// Token and wall-clock budget. `None` means unlimited.
    pub budget: Option<ExtractBudget>,
}

/// Output from [`LLMProvider::complete`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CompletionOutput {
    /// Free-form text response (no schema was requested).
    Text(String),
    /// Validated JSON response (schema was provided and output matched).
    Json(serde_json::Value),
}

/// Static capability declaration for a `LLMProvider` impl.
// Three flags cover distinct LLM API dimensions; a state machine adds
// indirection with no clarity gain here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LLMProviderCapabilities {
    /// Whether the provider supports structured JSON output mode.
    pub json_mode: bool,
    /// Whether the provider supports streaming completions.
    pub streaming: bool,
    /// Whether the provider supports parallel tool calls.
    pub tool_calls: bool,
}

/// LLM contract — `complete(prompt, schema?) → text | json`.
///
/// Default impl in #144: `cairn-llm-openai-compat` over `async-openai`
/// with configurable `base_url` (`OpenAI` / `Ollama` / `vLLM` / `LiteLLM` / …).
#[async_trait::async_trait]
pub trait LLMProvider: Send + Sync {
    /// Stable identifier of the registered plugin instance.
    fn name(&self) -> &str;

    /// Static capability advertisement (brief §4.1).
    fn capabilities(&self) -> &LLMProviderCapabilities;

    /// Range of `LLMProvider::CONTRACT_VERSION` values this impl accepts.
    fn supported_contract_versions(&self) -> VersionRange;
}

/// Static identity descriptor for a [`LLMProvider`] plugin (§4.1).
///
/// Carries the two associated consts the `register_plugin_with!` macro checks
/// before construction. See [`MemoryStorePlugin`](crate::contract::MemoryStorePlugin)
/// for the design rationale.
pub trait LLMProviderPlugin: LLMProvider + Sized {
    /// Stable plugin name, checked statically before construction (§4.1).
    const NAME: &'static str;
    /// Version range checked statically before construction (§4.1).
    const SUPPORTED_VERSIONS: VersionRange;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubLlm;

    #[async_trait::async_trait]
    impl LLMProvider for StubLlm {
        fn name(&self) -> &'static str {
            Self::NAME
        }
        fn capabilities(&self) -> &LLMProviderCapabilities {
            static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
                json_mode: true,
                streaming: false,
                tool_calls: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
    }

    impl LLMProviderPlugin for StubLlm {
        const NAME: &'static str = "stub-llm";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    #[test]
    fn dyn_compatible() {
        let l: Box<dyn LLMProvider> = Box::new(StubLlm);
        assert_eq!(l.name(), "stub-llm");
        assert!(l.supported_contract_versions().accepts(CONTRACT_VERSION));
    }

    #[test]
    fn static_consts_accessible() {
        assert_eq!(StubLlm::NAME, "stub-llm");
        assert!(StubLlm::SUPPORTED_VERSIONS.accepts(CONTRACT_VERSION));
    }

    #[test]
    fn lm_error_not_configured_display() {
        let e = LlmError::NotConfigured {
            remediation: "cairn config set llm.provider ollama".into(),
        };
        assert_eq!(
            e.to_string(),
            "llm.not_configured: cairn config set llm.provider ollama"
        );
    }

    #[test]
    fn lm_error_invalid_json_display() {
        let e = LlmError::InvalidJsonOutput {
            detail: "missing field `kind`".into(),
            raw: "{}".into(),
        };
        assert!(e.to_string().contains("llm.invalid_json_output"));
    }

    #[test]
    fn completion_request_builder_minimal() {
        let req = CompletionRequest::builder()
            .prompt("hello".to_string())
            .build();
        assert_eq!(req.prompt, "hello");
        assert!(req.schema.is_none());
        assert!(req.model.is_none());
        assert!(req.budget.is_none());
    }

    #[test]
    fn completion_request_builder_with_schema() {
        let schema = serde_json::json!({ "type": "object" });
        let req = CompletionRequest::builder()
            .prompt("hello".to_string())
            .schema(schema.clone())
            .build();
        assert_eq!(req.schema.as_ref().unwrap(), &schema);
    }

    #[test]
    fn completion_output_is_text() {
        let out = CompletionOutput::Text("hi".into());
        assert!(matches!(out, CompletionOutput::Text(_)));
    }

    #[test]
    fn completion_output_is_json() {
        let out = CompletionOutput::Json(serde_json::json!({"k": "v"}));
        assert!(matches!(out, CompletionOutput::Json(_)));
    }
}
