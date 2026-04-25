//! `LLMProvider` contract (brief §4 row 2).
//!
//! P0 scaffold: surface only. The single `complete(prompt, schema?)` method
//! and structured-output enforcement arrive in #144.

use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `LLMProvider`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

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

#[cfg(test)]
mod tests {
    use super::*;

    struct StubLlm;

    #[async_trait::async_trait]
    impl LLMProvider for StubLlm {
        fn name(&self) -> &'static str {
            "stub-llm"
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
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }
    }

    #[test]
    fn dyn_compatible() {
        let l: Box<dyn LLMProvider> = Box::new(StubLlm);
        assert_eq!(l.name(), "stub-llm");
        assert!(l.supported_contract_versions().accepts(CONTRACT_VERSION));
    }
}
