//! Typed config structs for `.cairn/config.yaml` (brief §3.1, §4.1, §5.2.a).

use crate::contract::registry::PluginError;

/// Errors produced during config validation or env-var interpolation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// A `custom:<name>` plugin name failed the `PluginName` grammar check.
    #[error("invalid plugin name for {field}: {source}")]
    InvalidPluginName {
        /// The config field name that contained the invalid plugin name.
        field: &'static str,
        /// The underlying plugin name validation error.
        #[source]
        source: PluginError,
    },
    /// A numeric budget field was set to zero.
    #[error("invalid budget for {field}: value {value} must be > 0")]
    InvalidBudget {
        /// The config field name containing the zero budget.
        field: &'static str,
        /// The invalid budget value.
        value: u64,
    },
    /// A retention key glob is malformed.
    #[error("invalid retention key pattern: {0}")]
    InvalidRetentionKey(String),
    /// The pipeline chain contains an `llm` worker but no `llm.provider` is set.
    #[error("pipeline chain has llm worker but llm.provider is not configured")]
    LlmExtractorWithoutProvider,
    /// A `${VAR}` placeholder in the YAML file references an unset env var.
    #[error("unresolved env var in config: ${{{0}}}")]
    UnresolvedEnvVar(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_budget_display() {
        let e = ConfigError::InvalidBudget {
            field: "vault.hot_memory.max_bytes",
            value: 0,
        };
        assert_eq!(
            e.to_string(),
            "invalid budget for vault.hot_memory.max_bytes: value 0 must be > 0"
        );
    }

    #[test]
    fn config_error_env_var_display() {
        let e = ConfigError::UnresolvedEnvVar("OPENAI_API_KEY".into());
        assert_eq!(e.to_string(), "unresolved env var in config: ${OPENAI_API_KEY}");
    }
}
