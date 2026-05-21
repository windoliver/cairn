//! Core-only builder for configured extractor chains.

use std::sync::Arc;

use crate::config::{ExtractConfig, ExtractorWorkerKind};
use crate::contract::agent_provider::AgentProvider;
use crate::contract::llm_provider::LLMProvider;
use crate::pipeline::extract::agent::AgentExtractor;
use crate::pipeline::extract::chain::ExtractChainBuildError;
use crate::pipeline::extract::llm::LLMExtractor;
use crate::pipeline::extract::{ExtractBudget, ExtractChain, ExtractorWorker, RegexExtractor};

/// Providers that outer crates may wire into extract workers.
///
/// This builder deliberately accepts providers instead of constructing runtime
/// adapters so `cairn-core` stays free of CLI/MCP/provider dependencies.
#[derive(Clone, Default)]
pub struct ExtractProviders {
    /// Optional LLM provider for `llm` chain entries.
    pub llm: Option<Arc<dyn LLMProvider>>,
    /// Optional agent provider for `agent` chain entries.
    pub agent: Option<Arc<dyn AgentProvider>>,
}

/// Errors returned while building an [`ExtractChain`] from config.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExtractBuildError {
    /// Config uses an LLM worker but no provider was supplied.
    #[error("extract chain entry uses llm worker but no LLMProvider is wired")]
    MissingLlmProvider,
    /// Config uses an agent worker but no provider was supplied.
    #[error("extract chain entry uses agent worker but no AgentProvider is wired")]
    MissingAgentProvider,
    /// Config named a custom worker that core cannot instantiate.
    #[error("unsupported extract worker `{worker}`")]
    UnsupportedWorker {
        /// Worker name as configured.
        worker: String,
    },
    /// Chain ordering failed validation.
    #[error(transparent)]
    InvalidChain(#[from] ExtractChainBuildError),
}

/// Build an extractor chain from config using caller-supplied providers.
///
/// # Errors
///
/// Returns [`ExtractBuildError`] when a configured provider-backed worker lacks
/// its provider, a custom worker is encountered, or chain ordering is invalid.
pub fn build_extract_chain(
    config: &ExtractConfig,
    providers: ExtractProviders,
) -> Result<ExtractChain, ExtractBuildError> {
    let mut workers: Vec<Box<dyn ExtractorWorker>> = Vec::with_capacity(config.chain.len());
    for entry in &config.chain {
        let budget = pipeline_budget_from_config(&entry.budget, &entry.worker);
        match &entry.worker {
            ExtractorWorkerKind::Regex => {
                workers.push(Box::new(RegexExtractor::from_parts(
                    crate::pipeline::extract::regex::RuleSet::builtin(),
                    budget,
                )));
            }
            ExtractorWorkerKind::Llm => {
                let provider = providers
                    .llm
                    .clone()
                    .ok_or(ExtractBuildError::MissingLlmProvider)?;
                workers.push(Box::new(LLMExtractor::new(provider).with_budget(budget)));
            }
            ExtractorWorkerKind::Agent => {
                let provider = providers
                    .agent
                    .clone()
                    .ok_or(ExtractBuildError::MissingAgentProvider)?;
                workers.push(Box::new(AgentExtractor::new(provider).with_budget(budget)));
            }
            ExtractorWorkerKind::Custom(name) => {
                return Err(ExtractBuildError::UnsupportedWorker {
                    worker: format!("custom:{name}"),
                });
            }
        }
    }
    Ok(ExtractChain::new(workers)?)
}

fn pipeline_budget_from_config(
    entry_budget: &crate::config::ExtractBudget,
    worker: &ExtractorWorkerKind,
) -> ExtractBudget {
    let mut b = match worker {
        ExtractorWorkerKind::Regex => ExtractBudget::regex_default(),
        ExtractorWorkerKind::Llm => ExtractBudget::llm_default(),
        ExtractorWorkerKind::Agent => ExtractBudget::agent_default(),
        ExtractorWorkerKind::Custom(_) => ExtractBudget::regex_default(),
    };
    if let Some(max_wall_ms) = entry_budget.max_wall_ms {
        b.max_wall_ms = max_wall_ms;
    }
    if let Some(max_tokens) = entry_budget.max_tokens {
        b.max_response_tokens = Some(max_tokens);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ExtractBudget as ConfigExtractBudget, ExtractorEntry};

    #[test]
    fn build_chain_rejects_custom_worker() {
        let config = ExtractConfig {
            chain: vec![ExtractorEntry {
                worker: ExtractorWorkerKind::Custom("third-party".to_owned()),
                kinds: vec![],
                trigger: None,
                budget: ConfigExtractBudget::default(),
            }],
        };

        assert!(matches!(
            build_extract_chain(&config, ExtractProviders::default()),
            Err(ExtractBuildError::UnsupportedWorker { .. })
        ));
    }

    #[test]
    fn build_chain_rejects_llm_entry_without_provider() {
        let config = ExtractConfig {
            chain: vec![
                ExtractorEntry {
                    worker: ExtractorWorkerKind::Regex,
                    kinds: vec![],
                    trigger: None,
                    budget: ConfigExtractBudget::default(),
                },
                ExtractorEntry {
                    worker: ExtractorWorkerKind::Llm,
                    kinds: vec![],
                    trigger: None,
                    budget: ConfigExtractBudget::default(),
                },
            ],
        };

        assert!(matches!(
            build_extract_chain(&config, ExtractProviders::default()),
            Err(ExtractBuildError::MissingLlmProvider)
        ));
    }
}
