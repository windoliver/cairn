//! Typed config structs for `.cairn/config.yaml` (brief §3.1, §4.1, §5.2.a).

pub mod vault_registry;
pub use vault_registry::{VaultEntry, VaultRegistry};

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
    #[serde(alias = "ollama")]
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

// ── Search ────────────────────────────────────────────────────────────────

/// Embedding model selection for local semantic search (brief §3.0).
///
/// Variant strings are kebab-case to match the brief's model identifiers.
/// Lives in `cairn-core` (not in `cairn-embeddings-local`) so `CairnConfig`
/// can reference it without a workspace-dep direction violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[non_exhaustive]
pub enum EmbeddingModelKind {
    /// BGE-small-en-v1.5, 384-dim, MIT license. Default.
    /// Applies asymmetric query prefix for retrieval.
    #[default]
    #[serde(rename = "bge-small-en-v1.5")]
    BgeSmallEnV1_5,
    /// all-MiniLM-L6-v2, 384-dim, Apache 2.0.
    #[serde(rename = "all-MiniLM-L6-v2")]
    AllMiniLmL6V2,
    /// `OpenAI` `text-embedding-3-large` (1536 dim). Requires the `openai`
    /// embedding provider; cannot be loaded by `ModelCache`.
    #[serde(rename = "openai-text-embedding-3-large")]
    OpenAiTextEmbedding3Large,
    /// `OpenAI` `text-embedding-3-small` (1536 dim).
    #[serde(rename = "openai-text-embedding-3-small")]
    OpenAiTextEmbedding3Small,
}

impl EmbeddingModelKind {
    /// Stable kebab-case label used in file-system paths and DB rows.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BgeSmallEnV1_5 => "bge-small-en-v1.5",
            Self::AllMiniLmL6V2 => "all-MiniLM-L6-v2",
            Self::OpenAiTextEmbedding3Large => "openai-text-embedding-3-large",
            Self::OpenAiTextEmbedding3Small => "openai-text-embedding-3-small",
        }
    }

    /// `HuggingFace` repo id for fetchable models. `None` for cloud providers.
    #[must_use]
    pub fn hf_repo(self) -> Option<&'static str> {
        match self {
            Self::BgeSmallEnV1_5 => Some("BAAI/bge-small-en-v1.5"),
            Self::AllMiniLmL6V2 => Some("sentence-transformers/all-MiniLM-L6-v2"),
            Self::OpenAiTextEmbedding3Large | Self::OpenAiTextEmbedding3Small => None,
        }
    }

    /// Expected output dimension of the model.
    ///
    /// Uses an explicit `match` so the compiler forces this to be updated
    /// whenever a new variant is added.
    #[must_use]
    #[allow(clippy::match_same_arms)] // intentional: exhaustive match forces updates on new variants
    pub fn dim(self) -> usize {
        match self {
            Self::BgeSmallEnV1_5 | Self::AllMiniLmL6V2 => 384,
            Self::OpenAiTextEmbedding3Large | Self::OpenAiTextEmbedding3Small => 1536,
        }
    }
}

/// Retrieval mode selected at search time (CLI flag, config default, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SearchMode {
    /// Keyword-only retrieval via FTS5 BM25.
    Bm25,
    /// Vector-only retrieval via sqlite-vec ANN.
    Vector,
    /// FTS5 + vector + RRF fusion + cosine re-rank.
    #[default]
    Hybrid,
}

/// Source of embedding vectors at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum EmbeddingProvider {
    /// Local candle inference (BGE / `MiniLM`).
    #[default]
    Local,
    /// `OpenAI` HTTP embedding endpoint. Requires the `openai` Cargo feature
    /// in `cairn-cli` and an `OPENAI_API_KEY` resolvable at runtime.
    #[serde(rename = "openai")]
    OpenAi,
}

/// Local semantic search configuration (brief §3.0).
///
/// `local_embeddings: false` drops `cairn.mcp.v1.search.semantic` and
/// `cairn.mcp.v1.search.hybrid` from `status.capabilities`. Those modes
/// return `CapabilityUnavailable` — no silent fallback (brief §3.0 fail-closed).
//
// Note: `Eq` was intentionally dropped from the derive list when `f32`/`f64`
// retrieval-tuning fields landed in Task 3 of the hybrid-retrieval branch.
// Floats can't be `Eq`. Pre-1.0 codebase, no external SDK consumers yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Enable local embedding runtime. Default `true`.
    pub local_embeddings: bool,
    /// Which embedding model to use. Default `bge-small-en-v1.5`.
    pub embedding_model: EmbeddingModelKind,
    /// Default retrieval mode when no `--mode` flag is supplied. Default `hybrid`.
    pub default_mode: SearchMode,
    /// Default embedding provider for query-time vectorization. Default `local`.
    //
    // TODO(task 8): when `cairn search` flag dispatch lands, validate at
    // verb-time that `default_provider == Local` is consistent with
    // `local_embeddings == true`, and `default_provider == OpenAi` requires
    // the `openai` Cargo feature + `OPENAI_API_KEY`. Fail-closed per
    // CLAUDE.md §4 invariant 6.
    pub default_provider: EmbeddingProvider,
    /// Blend coefficient α for cosine re-rank: final = α * rrf + (1-α) * cos.
    /// Range `[0.0, 1.0]`. Default `0.7`.
    pub rerank_blend: f32,
    /// Weights passed to FTS5 `bm25(records_fts, w0, w1, w2, w3)` over the
    /// four indexed columns: `[kind, class, scope, body]`. Default
    /// `[10.0, 10.0, 5.0, 1.0]`.
    pub fts_column_weights: [f64; 4],
    /// RRF constant `k`. Default `60`.
    pub rrf_k: usize,
    /// Number of top RRF candidates to second-pass cosine re-rank. Default `20`.
    pub rerank_topk: usize,
    /// Maximum total snippet characters per search page. Trimming happens
    /// after candidate ranking + dedup. Char-count proxy for token budget;
    /// token-accurate trimming is P1 (see issue #49). Default `8000`.
    pub max_snippet_chars_per_page: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            local_embeddings: true,
            embedding_model: EmbeddingModelKind::default(),
            default_mode: SearchMode::default(),
            default_provider: EmbeddingProvider::default(),
            rerank_blend: 0.7,
            fts_column_weights: [10.0, 10.0, 5.0, 1.0],
            rrf_k: 60,
            rerank_topk: 20,
            max_snippet_chars_per_page: 8000,
        }
    }
}

// ── Top-level ─────────────────────────────────────────────────────────────

/// Root config type. Deserialized from `.cairn/config.yaml` (brief §3.1).
///
/// All fields default to the P0 offline-local deployment:
/// `SQLite` store, no LLM, hook + IDE sensors, local tokio orchestrator,
/// regex-only extractor chain.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CairnConfig {
    /// Vault-level configuration.
    pub vault: VaultConfig,
    /// Store adapter selection.
    pub store: StoreConfig,
    /// LLM provider configuration.
    pub llm: LlmConfig,
    /// Search and embedding availability.
    pub search: SearchConfig,
    /// Sensor enablement.
    pub sensors: SensorsConfig,
    /// Workflow orchestrator selection.
    pub workflows: WorkflowsConfig,
    /// Pipeline stage configuration.
    pub pipeline: PipelineConfig,
}

// ── Vault ─────────────────────────────────────────────────────────────────

/// Vault-level configuration (§3.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
            name: "my-vault".into(),
            tier: VaultTier::Local,
            layout: LayoutConfig::default(),
            hot_memory: HotMemoryConfig::default(),
            retention: BTreeMap::new(),
            schema_files: vec!["CLAUDE.md".into(), "AGENTS.md".into(), "GEMINI.md".into()],
        }
    }
}

/// Folder names and enabled kinds (§3.1 layout block).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
            sources: "sources".into(),
            records: "raw".into(),
            wiki: "wiki".into(),
            skills: "skills".into(),
            enabled_kinds: vec![],
            file_naming: "{kind}_{slug}.md".into(),
            index: IndexConfig::default(),
        }
    }
}

/// Index file caps (§3.1 layout.index).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexConfig {
    /// Maximum number of lines in the index.
    pub max_lines: u32,
    /// Maximum number of bytes in the index.
    pub max_bytes: u32,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            max_lines: 200,
            max_bytes: 25_600,
        }
    }
}

/// Hot-memory assembly recipe and budget (§3.1 `hot_memory`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct StoreConfig {
    /// Which memory store adapter is active.
    pub kind: StoreKind,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            kind: StoreKind::Sqlite,
        }
    }
}

// ── LLM ──────────────────────────────────────────────────────────────────

/// LLM provider configuration (§4.1, ADR 0001).
///
/// P0 default: all `None`. LLM-dependent features fail closed with
/// `CapabilityUnavailable { code: "llm.not_configured" }`.
/// Fields `model` and `api_key` support `${VAR}` interpolation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
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
            hooks: SensorToggle { enabled: true },
            ide: SensorToggle { enabled: true },
            screen: SensorToggle { enabled: false },
            slack: SlackSensorConfig::default(),
        }
    }
}

/// Simple on/off toggle for a sensor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensorToggle {
    /// Whether this sensor is enabled.
    pub enabled: bool,
}

/// Slack sensor configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SlackSensorConfig {
    /// Whether the Slack sensor is enabled.
    pub enabled: bool,
    /// Slack channels or workspaces in scope.
    pub scope: Vec<String>,
}

// ── Workflows ─────────────────────────────────────────────────────────────

/// Workflow orchestrator selection (§4.1, §4.0 row 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkflowsConfig {
    /// Which workflow orchestrator is active.
    pub orchestrator: OrchestratorKind,
}

impl Default for WorkflowsConfig {
    fn default() -> Self {
        Self {
            orchestrator: OrchestratorKind::Local,
        }
    }
}

// ── Pipeline ─────────────────────────────────────────────────────────────

/// Pipeline stage configuration (§5.2.a).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PipelineConfig {
    /// Extractor chain configuration.
    pub extract: ExtractConfig,
}

/// Extractor chain configuration (§5.2.a).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExtractConfig {
    /// Ordered list of extractor entries.
    pub chain: Vec<ExtractorEntry>,
}

impl Default for ExtractConfig {
    fn default() -> Self {
        Self {
            chain: vec![ExtractorEntry {
                worker: ExtractorWorkerKind::Regex,
                kinds: vec![],
                trigger: None,
                budget: ExtractBudget::default(),
            }],
        }
    }
}

/// One entry in the extractor chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
            worker: ExtractorWorkerKind::Regex,
            kinds: vec![],
            trigger: None,
            budget: ExtractBudget::default(),
        }
    }
}

/// Resource limits for one extractor worker. `None` means unlimited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ExtractBudget {
    /// Maximum tokens this extractor may consume.
    pub max_tokens: Option<u32>,
    /// Maximum wall-clock time in milliseconds.
    pub max_wall_ms: Option<u32>,
    /// Maximum number of LLM turns.
    pub max_turns: Option<u32>,
}

/// Derived capability set, computed from `CairnConfig` (no I/O).
///
/// The verb layer calls `config.capabilities(model_present)` before dispatching to
/// gate features that require capabilities that may not be present.
// Six orthogonal capability flags; a bitflags type would obscure the intent.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySet {
    /// Always true at P0 (`FTS5` always present).
    pub keyword_search: bool,
    /// True iff `search.local_embeddings` is true and embedding model files exist on disk.
    pub semantic_search: bool,
    /// True iff `search.local_embeddings` is true (hybrid uses keyword+semantic legs).
    pub hybrid_search: bool,
    /// True iff `llm.provider` is `Some`.
    pub llm_extract: bool,
    /// True iff the pipeline chain contains an `agent` worker.
    pub agent_extract: bool,
    /// False for `sqlite` (P0). P1+ stores may advertise this.
    pub graph_edges: bool,
    /// True iff `cairn.mcp.v1.policy_trace` capability is advertised.
    /// Gates `--explain` on search and other Tier-2 inspection paths.
    pub policy_trace: bool,
}

impl CairnConfig {
    /// Validate semantic invariants that serde cannot enforce.
    ///
    /// # Errors
    /// See [`ConfigError`] variants for the full list.
    pub fn validate(&self) -> Result<(), ConfigError> {
        use crate::contract::registry::PluginName;

        // 1. Custom store plugin name grammar
        if let StoreKind::Custom(name) = &self.store.kind {
            PluginName::new(name.clone()).map_err(|source| ConfigError::InvalidPluginName {
                field: "store.kind",
                source,
            })?;
        }

        // 2. Custom orchestrator plugin name grammar
        if let OrchestratorKind::Custom(name) = &self.workflows.orchestrator {
            PluginName::new(name.clone()).map_err(|source| ConfigError::InvalidPluginName {
                field: "workflows.orchestrator",
                source,
            })?;
        }

        // 3. hot_memory.max_bytes must be > 0
        if self.vault.hot_memory.max_bytes == 0 {
            return Err(ConfigError::InvalidBudget {
                field: "vault.hot_memory.max_bytes",
                value: 0_u64,
            });
        }

        // 4. Extractor budget fields must be > 0 when set
        for entry in &self.pipeline.extract.chain {
            let b = &entry.budget;
            if b.max_tokens == Some(0) {
                return Err(ConfigError::InvalidBudget {
                    field: "pipeline.extract.chain[].budget.max_tokens",
                    value: 0_u64,
                });
            }
            if b.max_wall_ms == Some(0) {
                return Err(ConfigError::InvalidBudget {
                    field: "pipeline.extract.chain[].budget.max_wall_ms",
                    value: 0_u64,
                });
            }
            if b.max_turns == Some(0) {
                return Err(ConfigError::InvalidBudget {
                    field: "pipeline.extract.chain[].budget.max_turns",
                    value: 0_u64,
                });
            }
        }

        // 5. LLM extractor in chain requires an LLM provider
        let has_llm_worker = self
            .pipeline
            .extract
            .chain
            .iter()
            .any(|e| e.worker == ExtractorWorkerKind::Llm);
        if has_llm_worker && self.llm.provider.is_none() {
            return Err(ConfigError::LlmExtractorWithoutProvider);
        }

        // 6. Retention key glob patterns: `*` only in the filename position
        for key in self.vault.retention.keys() {
            if key.contains('\0') {
                return Err(ConfigError::InvalidRetentionKey(key.clone()));
            }
            let parts: Vec<&str> = key.split('/').collect();
            // Every component except the last (filename) must be free of `*`
            let dir_parts = parts.len().saturating_sub(1);
            for part in &parts[..dir_parts] {
                if part.contains('*') {
                    return Err(ConfigError::InvalidRetentionKey(key.clone()));
                }
            }
        }

        Ok(())
    }

    /// Derive the active capability set from this config (pure, no I/O).
    ///
    /// `model_present` should be `true` when the configured embedding model
    /// files exist on disk (stat-checked at startup).
    ///
    /// The verb layer uses this to gate features before dispatch.
    #[must_use]
    pub fn capabilities(&self, model_present: bool) -> CapabilitySet {
        let llm_on = self.llm.provider.is_some();
        let semantic = self.search.local_embeddings && model_present;
        let agent_extract = self
            .pipeline
            .extract
            .chain
            .iter()
            .any(|e| matches!(e.worker, ExtractorWorkerKind::Agent));

        CapabilitySet {
            keyword_search: true,
            // Semantic and hybrid both require an embedding model on disk:
            // the runtime resolves an embedder for both modes (see
            // `cairn-cli/src/verbs/search.rs`) and fails with `Internal`
            // if `ModelCache::ensure` returns `ModelNotFetched`. Advertising
            // hybrid without a model would therefore violate fail-closed
            // capability semantics. Keyword-only graceful degradation for
            // hybrid is a separate runtime change (track in #9-ish);
            // until then the gate matches what the runtime can honor.
            semantic_search: semantic,
            hybrid_search: semantic,
            llm_extract: llm_on,
            agent_extract,
            graph_edges: !matches!(self.store.kind, StoreKind::Sqlite), // P0: sqlite always false; P1+ gates on store capability
            // P0 always advertises policy_trace; a future config knob
            // (`search.disable_explain: true`) can opt out for environments
            // that prohibit trace-level output.
            policy_trace: true,
        }
    }

    /// Convenience: equivalent to `capabilities(false)`.
    /// Use when filesystem access is unavailable (e.g., pure config tests).
    #[must_use]
    pub fn capabilities_no_model(&self) -> CapabilitySet {
        self.capabilities(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta;
    use proptest::prelude::*;

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
        assert_eq!(
            e.to_string(),
            "unresolved env var in config: ${OPENAI_API_KEY}"
        );
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
        let json = serde_json::to_string(&StoreKind::Custom("cairn-store-qdrant".into())).unwrap();
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
    fn default_local_embeddings_enabled() {
        assert!(CairnConfig::default().search.local_embeddings);
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
        assert_eq!(
            CairnConfig::default().workflows.orchestrator,
            OrchestratorKind::Local
        );
    }

    #[test]
    fn default_extract_chain_has_regex_only() {
        let chain = &CairnConfig::default().pipeline.extract.chain;
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].worker, ExtractorWorkerKind::Regex);
    }

    #[test]
    fn validate_default_config_ok() {
        CairnConfig::default().validate().unwrap();
    }

    #[test]
    fn validate_rejects_zero_hot_memory_budget() {
        let mut config = CairnConfig::default();
        config.vault.hot_memory.max_bytes = 0;
        let err = config.validate().unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidBudget {
                field: "vault.hot_memory.max_bytes",
                ..
            }
        ));
    }

    #[test]
    fn validate_rejects_zero_extractor_budget_tokens() {
        let mut config = CairnConfig::default();
        config.pipeline.extract.chain[0].budget.max_tokens = Some(0);
        let err = config.validate().unwrap_err();
        assert!(matches!(err, ConfigError::InvalidBudget { .. }));
    }

    #[test]
    fn validate_rejects_llm_worker_without_provider() {
        let mut config = CairnConfig::default();
        config.pipeline.extract.chain.push(ExtractorEntry {
            worker: ExtractorWorkerKind::Llm,
            kinds: vec![],
            trigger: None,
            budget: ExtractBudget::default(),
        });
        // llm.provider is None by default
        let err = config.validate().unwrap_err();
        assert!(matches!(err, ConfigError::LlmExtractorWithoutProvider));
    }

    #[test]
    fn validate_accepts_llm_worker_with_provider() {
        let mut config = CairnConfig::default();
        config.llm.provider = Some(LlmProvider::OpenaiCompatible);
        config.pipeline.extract.chain.push(ExtractorEntry {
            worker: ExtractorWorkerKind::Llm,
            kinds: vec![],
            trigger: None,
            budget: ExtractBudget::default(),
        });
        config.validate().unwrap();
    }

    #[test]
    fn validate_rejects_invalid_custom_store_name() {
        let mut config = CairnConfig::default();
        config.store.kind = StoreKind::Custom("BAD NAME WITH SPACES".into());
        let err = config.validate().unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidPluginName {
                field: "store.kind",
                ..
            }
        ));
    }

    #[test]
    fn validate_accepts_valid_custom_store_name() {
        let mut config = CairnConfig::default();
        config.store.kind = StoreKind::Custom("cairn-store-qdrant".into());
        config.validate().unwrap();
    }

    #[test]
    fn validate_rejects_retention_key_with_star_in_dir() {
        let mut config = CairnConfig::default();
        config
            .vault
            .retention
            .insert("*/trace.md".into(), "30d".into());
        let err = config.validate().unwrap_err();
        assert!(matches!(err, ConfigError::InvalidRetentionKey(_)));
    }

    #[test]
    fn validate_accepts_retention_key_star_in_filename() {
        let mut config = CairnConfig::default();
        config
            .vault
            .retention
            .insert("raw/trace_*.md".into(), "30d".into());
        config.validate().unwrap();
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
          "search": { "local_embeddings": true },
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

    #[test]
    fn capabilities_llm_off_by_default() {
        let caps = CairnConfig::default().capabilities(false);
        assert!(caps.keyword_search, "keyword_search always true");
        assert!(!caps.semantic_search, "model absent → no semantic");
        assert!(
            !caps.hybrid_search,
            "model absent → no hybrid: runtime resolves an embedder for hybrid \
             mode and fails closed without one (see crates/cairn-cli/src/verbs/search.rs)"
        );
        assert!(!caps.llm_extract, "no LLM → no llm_extract");
        assert!(!caps.agent_extract, "default chain has no agent worker");
        assert!(!caps.graph_edges, "sqlite → no graph edges");
        assert!(caps.policy_trace, "policy_trace always true at P0");
    }

    #[test]
    fn capabilities_local_embeddings_off() {
        let mut config = CairnConfig::default();
        config.search.local_embeddings = false;
        let caps = config.capabilities(false);
        assert!(caps.keyword_search);
        assert!(!caps.semantic_search);
        assert!(!caps.hybrid_search);
        assert!(!caps.llm_extract);
    }

    #[test]
    fn capabilities_llm_on() {
        let mut config = CairnConfig::default();
        config.llm.provider = Some(LlmProvider::OpenaiCompatible);
        let caps = config.capabilities(false);
        assert!(caps.keyword_search);
        assert!(
            !caps.semantic_search,
            "model absent → no semantic even with LLM"
        );
        assert!(
            !caps.hybrid_search,
            "model absent → no hybrid: hybrid requires the embedder the runtime \
             resolves for both legs (see crates/cairn-cli/src/verbs/search.rs)"
        );
        assert!(caps.llm_extract);
        assert!(!caps.agent_extract);
    }

    #[test]
    fn capabilities_agent_extract_when_chain_has_agent() {
        let mut config = CairnConfig::default();
        config.pipeline.extract.chain.push(ExtractorEntry {
            worker: ExtractorWorkerKind::Agent,
            kinds: vec![],
            trigger: None,
            budget: ExtractBudget::default(),
        });
        let caps = config.capabilities(false);
        assert!(caps.agent_extract);
    }

    #[test]
    fn default_config_snapshot() {
        let json = serde_json::to_string_pretty(&CairnConfig::default())
            .expect("CairnConfig::default() must be serializable");
        insta::assert_snapshot!(json);
    }

    #[test]
    fn semantic_on_when_local_embeddings_and_model_present() {
        let config = CairnConfig::default();
        let caps = config.capabilities(true);
        assert!(caps.semantic_search);
        assert!(caps.hybrid_search, "hybrid on when local_embeddings: true");
    }

    #[test]
    fn semantic_off_when_local_embeddings_false() {
        let mut config = CairnConfig::default();
        config.search.local_embeddings = false;
        let caps = config.capabilities(true); // model present but opt-out
        assert!(!caps.semantic_search);
    }

    #[test]
    fn semantic_off_when_model_absent() {
        let config = CairnConfig::default(); // local_embeddings: true
        let caps = config.capabilities(false); // model not on disk
        assert!(!caps.semantic_search);
    }

    #[test]
    fn semantic_not_tied_to_llm_provider() {
        let mut config = CairnConfig::default();
        // LLM present but model absent → semantic still false.
        config.llm.provider = Some(LlmProvider::OpenaiCompatible);
        let caps = config.capabilities(false);
        assert!(!caps.semantic_search);
        // Model present → semantic true regardless of LLM.
        let caps2 = config.capabilities(true);
        assert!(caps2.semantic_search);
    }

    #[test]
    fn embedding_model_kind_as_str() {
        assert_eq!(
            EmbeddingModelKind::BgeSmallEnV1_5.as_str(),
            "bge-small-en-v1.5"
        );
        assert_eq!(
            EmbeddingModelKind::AllMiniLmL6V2.as_str(),
            "all-MiniLM-L6-v2"
        );
    }

    #[test]
    fn search_config_default() {
        let c = SearchConfig::default();
        assert!(c.local_embeddings);
        assert_eq!(c.embedding_model, EmbeddingModelKind::BgeSmallEnV1_5);
    }

    #[test]
    fn embedding_model_kind_serde_round_trip() {
        let json = serde_json::to_string(&EmbeddingModelKind::AllMiniLmL6V2).unwrap();
        assert_eq!(json, r#""all-MiniLM-L6-v2""#);
        let back: EmbeddingModelKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, EmbeddingModelKind::AllMiniLmL6V2);
        // Also verify BgeSmallEnV1_5
        let json2 = serde_json::to_string(&EmbeddingModelKind::BgeSmallEnV1_5).unwrap();
        assert_eq!(json2, r#""bge-small-en-v1.5""#);
    }

    #[test]
    fn search_mode_default_is_hybrid() {
        assert_eq!(SearchMode::default(), SearchMode::Hybrid);
    }

    #[test]
    fn search_mode_serde_kebab() {
        let modes = [SearchMode::Bm25, SearchMode::Vector, SearchMode::Hybrid];
        let strs = ["bm25", "vector", "hybrid"];
        for (m, s) in modes.iter().zip(strs.iter()) {
            let yaml = yaml_serde::to_string(m).unwrap();
            assert!(yaml.trim() == *s, "mode {m:?} serialized to {yaml:?}");
            let back: SearchMode = yaml_serde::from_str(s).unwrap();
            assert_eq!(*m, back);
        }
    }

    #[test]
    fn embedding_provider_default_is_local() {
        assert_eq!(EmbeddingProvider::default(), EmbeddingProvider::Local);
    }

    #[test]
    fn embedding_provider_serde_kebab() {
        let yaml = yaml_serde::to_string(&EmbeddingProvider::OpenAi).unwrap();
        assert_eq!(yaml.trim(), "openai");
        let back: EmbeddingProvider = yaml_serde::from_str("openai").unwrap();
        assert_eq!(back, EmbeddingProvider::OpenAi);
    }

    #[test]
    fn openai_embedding_model_kinds_have_dim_1536() {
        assert_eq!(EmbeddingModelKind::OpenAiTextEmbedding3Large.dim(), 1536);
        assert_eq!(EmbeddingModelKind::OpenAiTextEmbedding3Small.dim(), 1536);
    }

    #[test]
    fn openai_embedding_model_kinds_have_no_hf_repo() {
        assert_eq!(
            EmbeddingModelKind::OpenAiTextEmbedding3Large.hf_repo(),
            None
        );
        assert_eq!(
            EmbeddingModelKind::OpenAiTextEmbedding3Small.hf_repo(),
            None
        );
        assert_eq!(
            EmbeddingModelKind::BgeSmallEnV1_5.hf_repo(),
            Some("BAAI/bge-small-en-v1.5"),
        );
    }

    #[test]
    fn search_config_default_includes_new_fields() {
        let c = SearchConfig::default();
        assert_eq!(c.default_mode, SearchMode::Hybrid);
        assert_eq!(c.default_provider, EmbeddingProvider::Local);
        assert!((c.rerank_blend - 0.7).abs() < 1e-6);
        // Compare element-wise with epsilon to avoid clippy::float_cmp on arrays.
        let expected = [10.0_f64, 10.0, 5.0, 1.0];
        for (got, want) in c.fts_column_weights.iter().zip(expected.iter()) {
            assert!((got - want).abs() < 1e-9, "got {got}, want {want}");
        }
        assert_eq!(c.rrf_k, 60);
        assert_eq!(c.rerank_topk, 20);
    }

    #[test]
    fn search_config_yaml_round_trip() {
        let yaml = "
local_embeddings: true
embedding_model: bge-small-en-v1.5
default_mode: hybrid
default_provider: local
rerank_blend: 0.7
fts_column_weights: [10.0, 10.0, 5.0, 1.0]
rrf_k: 60
rerank_topk: 20
";
        let c: SearchConfig = yaml_serde::from_str(yaml).unwrap();
        let back = yaml_serde::to_string(&c).unwrap();
        let again: SearchConfig = yaml_serde::from_str(&back).unwrap();
        assert_eq!(c, again);
    }

    proptest! {
        #[test]
        fn default_config_json_round_trip(_seed in 0u8..1) {
            // Tests serde symmetry on the default config; a full property test
            // would require Arbitrary impls for all types (out of scope for P0).
            let original = CairnConfig::default();
            let json = serde_json::to_string(&original).unwrap();
            let restored: CairnConfig = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(original, restored);
        }
    }
}
