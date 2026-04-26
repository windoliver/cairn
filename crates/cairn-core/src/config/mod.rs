//! Typed config structs for `.cairn/config.yaml` (brief §3.1, §4.1, §5.2.a).

use serde::{Deserialize, Serialize};

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

/// Vault storage tier (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VaultTier {
    /// Single-user, on-disk `SQLite` vault. P0 default.
    #[default]
    Local,
    /// Embedded in another process (library mode). P1.
    Embedded,
    /// Federated cloud vault. P2.
    Cloud,
}

/// Ordered steps in the hot-memory assembly recipe (§3.1 `hot_memory.recipe`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HotMemoryRecipeStep {
    /// Vault purpose (brief §2.2).
    Purpose,
    /// Vault index (brief §2.3).
    Index,
    /// Pinned feedback (brief §3.1).
    PinnedFeedback,
    /// Top salience project (brief §3.1).
    TopSalienceProject,
    /// Active playbook (brief §3.1).
    ActivePlaybook,
    /// Recent user signal (brief §3.1).
    RecentUserSignal,
}

/// Condition that gates an extractor entry in the chain (§5.2.a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExtractTrigger {
    /// Run this extractor only when the previous one produced confidence < 0.6.
    ConfidenceBelow,
}

/// Which LLM provider backend is active (§4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum LlmProvider {
    /// Any `OpenAI`-compatible endpoint (Ollama, LM Studio, `OpenAI`, Azure).
    OpenaiCompatible,
}

/// Macro: implement string-backed serde for enums with an implicit
/// `Custom(String)` variant. Reduces boilerplate for `StoreKind`,
/// `OrchestratorKind`, and `ExtractorWorkerKind`.
///
/// The `Custom` variant is always generated implicitly — do not list it in the
/// invocation. This avoids a Rust macro local-ambiguity when the parser sees
/// the terminal `Custom,` ident before `=>`.
macro_rules! string_enum {
    (
        $(#[$attr:meta])*
        pub enum $name:ident {
            $( $(#[$vattr:meta])* $variant:ident => $wire:literal , )*
        }
        unknown_msg: $msg:literal $(,)?
    ) => {
        $(#[$attr])*
        pub enum $name {
            $( $(#[$vattr])* $variant, )*
            /// A third-party plugin registered under this contract.
            /// The string after `"custom:"` is the raw plugin name.
            Custom(String),
        }

        impl Default for $name {
            fn default() -> Self {
                // First variant is the default.
                $name::first_variant()
            }
        }

        impl $name {
            #[allow(unreachable_code)]
            fn first_variant() -> Self {
                $( return Self::$variant; )*
                unreachable!()
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                match self {
                    $( Self::$variant => s.serialize_str($wire), )*
                    Self::Custom(name) => s.serialize_str(&format!("custom:{name}")),
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(d)?;
                match raw.as_str() {
                    $( $wire => Ok(Self::$variant), )*
                    s if s.starts_with("custom:") => {
                        Ok(Self::Custom(s["custom:".len()..].to_owned()))
                    }
                    _ => Err(serde::de::Error::custom(format!(
                        "unknown {}: {:?} ({})",
                        stringify!($name), raw, $msg
                    ))),
                }
            }
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                match (self, other) {
                    $( (Self::$variant, Self::$variant) => true, )*
                    (Self::Custom(a), Self::Custom(b)) => a == b,
                    _ => false,
                }
            }
        }
        impl Eq for $name {}

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $( Self::$variant => write!(f, "{}", $wire), )*
                    Self::Custom(n) => write!(f, "custom:{n}"),
                }
            }
        }

        impl Clone for $name {
            fn clone(&self) -> Self {
                match self {
                    $( Self::$variant => Self::$variant, )*
                    Self::Custom(n) => Self::Custom(n.clone()),
                }
            }
        }
    };
}

string_enum! {
    /// Which memory store adapter is active (§4.1 plugin config).
    #[non_exhaustive]
    pub enum StoreKind {
        /// `SQLite` + FTS5 + sqlite-vec. P0 default.
        Sqlite => "sqlite",
        /// Nexus sidecar (P1).
        NexusSandbox => "nexus-sandbox",
        /// Federated Nexus hub (P2).
        NexusFull => "nexus-full",
    }
    unknown_msg: "expected sqlite | nexus-sandbox | nexus-full | custom:<name>",
}

string_enum! {
    /// Which workflow orchestrator is active (§4.1, §4.0 row 3).
    #[non_exhaustive]
    pub enum OrchestratorKind {
        /// In-process tokio + `SQLite` job table. P0 default.
        Local => "local",
        /// Temporal workflow engine (P1 opt-in).
        Temporal => "temporal",
    }
    unknown_msg: "expected local | temporal | custom:<name>",
}

string_enum! {
    /// Which extractor worker mode is used in a chain entry (§5.2.a).
    #[non_exhaustive]
    pub enum ExtractorWorkerKind {
        /// Regex pattern-matching, <2 ms, P0 always-on.
        Regex => "regex",
        /// Single LLM call with structured output schema. P0 default for turn capture.
        Llm => "llm",
        /// Full Cairn agent with read-only tools. P2 opt-in.
        Agent => "agent",
    }
    unknown_msg: "expected regex | llm | agent | custom:<name>",
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

    #[test]
    fn vault_tier_round_trips() {
        let json = serde_json::to_string(&VaultTier::Local).unwrap();
        assert_eq!(json, r#""local""#);
        let back: VaultTier = serde_json::from_str(&json).unwrap();
        assert_eq!(back, VaultTier::Local);
    }

    #[test]
    fn hot_memory_recipe_step_round_trips() {
        let json = serde_json::to_string(&HotMemoryRecipeStep::PinnedFeedback).unwrap();
        assert_eq!(json, r#""pinned_feedback""#);
    }

    #[test]
    fn extract_trigger_round_trips() {
        let json = serde_json::to_string(&ExtractTrigger::ConfidenceBelow).unwrap();
        assert_eq!(json, r#""confidence_below""#);
    }

    #[test]
    fn store_kind_sqlite_round_trips() {
        let json = serde_json::to_string(&StoreKind::Sqlite).unwrap();
        assert_eq!(json, r#""sqlite""#);
        let back: StoreKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, StoreKind::Sqlite);
    }

    #[test]
    fn store_kind_custom_round_trips() {
        let json =
            serde_json::to_string(&StoreKind::Custom("cairn-store-qdrant".into())).unwrap();
        assert_eq!(json, r#""custom:cairn-store-qdrant""#);
        let back: StoreKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, StoreKind::Custom("cairn-store-qdrant".into()));
    }

    #[test]
    fn store_kind_unknown_rejected() {
        let result: Result<StoreKind, _> = serde_json::from_str(r#""bogus""#);
        assert!(result.is_err());
    }

    #[test]
    fn orchestrator_kind_round_trips() {
        let json = serde_json::to_string(&OrchestratorKind::Local).unwrap();
        assert_eq!(json, r#""local""#);
        let back: OrchestratorKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, OrchestratorKind::Local);
    }

    #[test]
    fn extractor_worker_kind_round_trips() {
        let json = serde_json::to_string(&ExtractorWorkerKind::Llm).unwrap();
        assert_eq!(json, r#""llm""#);
        let back: ExtractorWorkerKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ExtractorWorkerKind::Llm);
    }
}
