//! Typed config structs for `.cairn/config.yaml` (brief §3.1, §4.1, §5.2.a).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::taxonomy::MemoryKind;

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

// ── Top-level ─────────────────────────────────────────────────────────────

/// Root config type. Deserialized from `.cairn/config.yaml` (brief §3.1).
///
/// All fields default to the P0 offline-local deployment:
/// `SQLite` store, no LLM, hook + IDE sensors, local tokio orchestrator,
/// regex-only extractor chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CairnConfig {
    /// Vault-level configuration.
    pub vault: VaultConfig,
    /// Store adapter selection.
    pub store: StoreConfig,
    /// LLM provider configuration.
    pub llm: LlmConfig,
    /// Sensor enablement.
    pub sensors: SensorsConfig,
    /// Workflow orchestrator selection.
    pub workflows: WorkflowsConfig,
    /// Pipeline stage configuration.
    pub pipeline: PipelineConfig,
}

impl Default for CairnConfig {
    fn default() -> Self {
        Self {
            vault:     VaultConfig::default(),
            store:     StoreConfig::default(),
            llm:       LlmConfig::default(),
            sensors:   SensorsConfig::default(),
            workflows: WorkflowsConfig::default(),
            pipeline:  PipelineConfig::default(),
        }
    }
}

// ── Vault ─────────────────────────────────────────────────────────────────

/// Vault-level configuration (§3.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    /// Human-readable vault name.
    pub name: String,
    /// Storage tier.
    pub tier: VaultTier,
    /// Folder layout and enabled kinds.
    pub layout: LayoutConfig,
    /// Hot-memory assembly recipe and budget.
    pub hot_memory: HotMemoryConfig,
    /// Glob-keyed retention policies. Value: `"forever"` or `"<N>d"`.
    pub retention: BTreeMap<String, String>,
    /// Schema files to include in the vault.
    pub schema_files: Vec<String>,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            name:         "my-vault".into(),
            tier:         VaultTier::Local,
            layout:       LayoutConfig::default(),
            hot_memory:   HotMemoryConfig::default(),
            retention:    BTreeMap::new(),
            schema_files: vec![
                "CLAUDE.md".into(),
                "AGENTS.md".into(),
                "GEMINI.md".into(),
            ],
        }
    }
}

/// Folder names and enabled kinds (§3.1 layout block).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    /// Directory name for source files.
    pub sources: String,
    /// Directory name for raw records.
    pub records: String,
    /// Directory name for wiki files.
    pub wiki: String,
    /// Directory name for skills.
    pub skills: String,
    /// Subset of the 19 `MemoryKind`s active for extraction + storage.
    /// Empty means all 19 kinds are enabled (semantics: absence = unrestricted).
    pub enabled_kinds: Vec<MemoryKind>,
    /// File naming template.
    pub file_naming: String,
    /// Index file caps.
    pub index: IndexConfig,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            sources:       "sources".into(),
            records:       "raw".into(),
            wiki:          "wiki".into(),
            skills:        "skills".into(),
            enabled_kinds: vec![],
            file_naming:   "{kind}_{slug}.md".into(),
            index:         IndexConfig::default(),
        }
    }
}

/// Index file caps (§3.1 layout.index).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    /// Maximum number of lines in the index.
    pub max_lines: u32,
    /// Maximum number of bytes in the index.
    pub max_bytes: u32,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self { max_lines: 200, max_bytes: 25_600 }
    }
}

/// Hot-memory assembly recipe and budget (§3.1 hot_memory).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotMemoryConfig {
    /// Ordered steps in the assembly recipe.
    pub recipe: Vec<HotMemoryRecipeStep>,
    /// Maximum bytes in the assembled hot prefix. Must be > 0.
    pub max_bytes: u32,
}

impl Default for HotMemoryConfig {
    fn default() -> Self {
        Self {
            recipe: vec![
                HotMemoryRecipeStep::Purpose,
                HotMemoryRecipeStep::Index,
                HotMemoryRecipeStep::PinnedFeedback,
                HotMemoryRecipeStep::TopSalienceProject,
                HotMemoryRecipeStep::ActivePlaybook,
                HotMemoryRecipeStep::RecentUserSignal,
            ],
            max_bytes: 25_600,
        }
    }
}

// ── Store ─────────────────────────────────────────────────────────────────

/// Store adapter selection (§4.1 plugin config).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreConfig {
    /// Which memory store adapter is active.
    pub kind: StoreKind,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self { kind: StoreKind::Sqlite }
    }
}

// ── LLM ──────────────────────────────────────────────────────────────────

/// LLM provider configuration (§4.1, ADR 0001).
///
/// P0 default: all `None`. LLM-dependent features fail closed with
/// `CapabilityUnavailable { code: "llm.not_configured" }`.
/// Fields `model` and `api_key` support `${VAR}` interpolation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LlmConfig {
    /// Which LLM provider backend is active.
    pub provider: Option<LlmProvider>,
    /// Base URL for the LLM provider endpoint.
    pub base_url: Option<String>,
    /// Model name to use. Supports `${VAR}` interpolation.
    pub model: Option<String>,
    /// API key. Supports `${VAR}` interpolation.
    pub api_key: Option<String>,
}

// ── Sensors ───────────────────────────────────────────────────────────────

/// Sensor enablement (§3.1 sensors block).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SensorsConfig {
    /// Hook sensor configuration.
    pub hooks: SensorToggle,
    /// IDE sensor configuration.
    pub ide: SensorToggle,
    /// Screen sensor configuration.
    pub screen: SensorToggle,
    /// Slack sensor configuration.
    pub slack: SlackSensorConfig,
}

impl Default for SensorsConfig {
    fn default() -> Self {
        Self {
            hooks:  SensorToggle { enabled: true },
            ide:    SensorToggle { enabled: true },
            screen: SensorToggle { enabled: false },
            slack:  SlackSensorConfig::default(),
        }
    }
}

/// Simple on/off toggle for a sensor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensorToggle {
    /// Whether this sensor is enabled.
    pub enabled: bool,
}

/// Slack sensor configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SlackSensorConfig {
    /// Whether the Slack sensor is enabled.
    pub enabled: bool,
    /// Slack channels or workspaces in scope.
    pub scope: Vec<String>,
}

// ── Workflows ─────────────────────────────────────────────────────────────

/// Workflow orchestrator selection (§4.1, §4.0 row 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowsConfig {
    /// Which workflow orchestrator is active.
    pub orchestrator: OrchestratorKind,
}

impl Default for WorkflowsConfig {
    fn default() -> Self {
        Self { orchestrator: OrchestratorKind::Local }
    }
}

// ── Pipeline ─────────────────────────────────────────────────────────────

/// Pipeline stage configuration (§5.2.a).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineConfig {
    /// Extractor chain configuration.
    pub extract: ExtractConfig,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self { extract: ExtractConfig::default() }
    }
}

/// Extractor chain configuration (§5.2.a).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtractConfig {
    /// Ordered list of extractor entries.
    pub chain: Vec<ExtractorEntry>,
}

impl Default for ExtractConfig {
    fn default() -> Self {
        Self {
            chain: vec![ExtractorEntry {
                worker:  ExtractorWorkerKind::Regex,
                kinds:   vec![],
                trigger: None,
                budget:  ExtractBudget::default(),
            }],
        }
    }
}

/// One entry in the extractor chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtractorEntry {
    /// Which extractor worker mode is used.
    pub worker: ExtractorWorkerKind,
    /// Kinds this extractor handles. Empty means all kinds.
    pub kinds: Vec<MemoryKind>,
    /// Condition that gates this extractor entry.
    pub trigger: Option<ExtractTrigger>,
    /// Resource limits for this extractor worker.
    pub budget: ExtractBudget,
}

impl Default for ExtractorEntry {
    fn default() -> Self {
        Self {
            worker:  ExtractorWorkerKind::Regex,
            kinds:   vec![],
            trigger: None,
            budget:  ExtractBudget::default(),
        }
    }
}

/// Resource limits for one extractor worker. `None` means unlimited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ExtractBudget {
    /// Maximum tokens this extractor may consume.
    pub max_tokens: Option<u32>,
    /// Maximum wall-clock time in milliseconds.
    pub max_wall_ms: Option<u32>,
    /// Maximum number of LLM turns.
    pub max_turns: Option<u32>,
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

    #[test]
    fn default_config_deserializes_from_empty_json() {
        let config: CairnConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config, CairnConfig::default());
    }

    #[test]
    fn default_store_kind_is_sqlite() {
        assert_eq!(CairnConfig::default().store.kind, StoreKind::Sqlite);
    }

    #[test]
    fn default_llm_provider_is_none() {
        assert!(CairnConfig::default().llm.provider.is_none());
    }

    #[test]
    fn default_hooks_sensor_is_enabled() {
        assert!(CairnConfig::default().sensors.hooks.enabled);
    }

    #[test]
    fn default_screen_sensor_is_disabled() {
        assert!(!CairnConfig::default().sensors.screen.enabled);
    }

    #[test]
    fn default_orchestrator_is_local() {
        assert_eq!(CairnConfig::default().workflows.orchestrator, OrchestratorKind::Local);
    }

    #[test]
    fn default_extract_chain_has_regex_only() {
        let chain = &CairnConfig::default().pipeline.extract.chain;
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].worker, ExtractorWorkerKind::Regex);
    }

    #[test]
    fn full_config_json_round_trips() {
        let json = r#"{
          "vault": {
            "name": "my-vault",
            "tier": "local",
            "layout": {
              "sources": "inbox", "records": "memories", "wiki": "notes",
              "skills": "skills", "enabled_kinds": ["user","feedback"],
              "file_naming": "{kind}_{slug}.md",
              "index": { "max_lines": 200, "max_bytes": 25600 }
            },
            "hot_memory": { "max_bytes": 25600, "recipe": ["purpose","index"] },
            "retention": {}, "schema_files": ["CLAUDE.md"]
          },
          "store": { "kind": "sqlite" },
          "llm": {},
          "sensors": {
            "hooks": { "enabled": true },
            "ide": { "enabled": false },
            "screen": { "enabled": false },
            "slack": { "enabled": false, "scope": [] }
          },
          "workflows": { "orchestrator": "local" },
          "pipeline": { "extract": { "chain": [{ "worker": "regex", "kinds": [] }] } }
        }"#;
        let config: CairnConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.vault.name, "my-vault");
        assert_eq!(config.vault.layout.sources, "inbox");
        assert_eq!(config.vault.layout.enabled_kinds.len(), 2);
        assert!(!config.sensors.ide.enabled);
    }
}
