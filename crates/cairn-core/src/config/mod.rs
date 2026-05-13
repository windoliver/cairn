//! Typed config structs for `.cairn/config.yaml` (brief §3.1, §4.1, §5.2.a).

pub mod mcp;
pub use mcp::{McpConfig, McpStdioConfig};

/// Validate `[mcp.*]` invariants beyond what serde alone enforces.
///
/// # Errors
/// - [`ConfigError::McpStdioMissingPrincipal`] when
///   `[mcp.stdio] single_tenant = true` is set without a `principal`.
/// - [`ConfigError::McpStdioInvalidPrincipal`] when the configured principal
///   fails [`crate::domain::ScopeTuple::validate`] — empty components,
///   reserved characters, or the unsupported `project` dimension. The
///   graph-tools matcher binds only the six IDL-addressable dimensions,
///   so a `project`-bearing principal would be silently broadened at
///   read time; we fail closed at config-load instead.
pub fn validate_mcp_config(cfg: &McpConfig) -> Result<(), ConfigError> {
    if cfg.stdio.single_tenant && cfg.stdio.principal.is_none() {
        return Err(ConfigError::McpStdioMissingPrincipal);
    }
    if let Some(principal) = cfg.stdio.principal.as_ref() {
        principal
            .validate()
            .map_err(|err| ConfigError::McpStdioInvalidPrincipal {
                message: err.to_string(),
            })?;
    }
    Ok(())
}

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
    /// A ratio field fell outside its accepted range.
    #[error("invalid ratio for {field}: value {value} must be > 0 and <= 1")]
    InvalidRatio {
        /// The config field containing the invalid ratio.
        field: &'static str,
        /// The invalid ratio value.
        value: f64,
    },
    /// `vault.hot_memory.default_recipe` named a recipe that does not exist.
    #[error("vault.hot_memory.default_recipe {name:?} does not exist in vault.hot_memory.recipes")]
    MissingHotMemoryDefaultRecipe {
        /// The missing recipe name.
        name: String,
    },
    /// A recipe-related identifier (default name or table key) failed a
    /// non-emptiness / shape check before reaching the wire boundary.
    #[error("invalid recipe name for {field}: {reason}")]
    InvalidRecipeName {
        /// Config path that contains the offending name.
        field: &'static str,
        /// Reason the name was rejected.
        reason: &'static str,
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
    /// `[mcp.stdio] single_tenant = true` was set but no `principal` was
    /// provided.
    #[error("[mcp.stdio] single_tenant = true requires a `principal` scope tuple")]
    McpStdioMissingPrincipal,
    /// `[mcp.stdio].principal` failed `ScopeTuple::validate` (malformed
    /// components or unsupported dimension). The error text from the
    /// underlying domain check is carried in `message`.
    #[error("[mcp.stdio].principal is malformed: {message}")]
    McpStdioInvalidPrincipal {
        /// Stringified `DomainError::MalformedScope` body.
        message: String,
    },
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
    #[serde(alias = "top_salience")]
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
    /// Minimum entity-edge confidence score (`entity_edges.confidence_score`)
    /// for the hybrid graph leg to admit an edge. Edges below this floor are
    /// excluded from graph expansion entirely so weak/ambiguous evidence does
    /// not dominate hybrid recall. Default `0.3` — matches `EdgeConfidence`'s
    /// `Extracted` floor while excluding clearly unreliable links.
    /// Range `[0.0, 1.0]`; values outside that range still parse but the
    /// store clamps before use.
    #[serde(default = "default_graph_confidence_min")]
    pub graph_confidence_min: f32,
}

fn default_graph_confidence_min() -> f32 {
    0.3
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
            graph_confidence_min: default_graph_confidence_min(),
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
    /// Source-file forget/redaction policy.
    pub source: SourceConfig,
    /// Sensor enablement.
    pub sensors: SensorsConfig,
    /// Workflow orchestrator selection.
    pub workflows: WorkflowsConfig,
    /// Pipeline stage configuration.
    pub pipeline: PipelineConfig,
    /// MCP transport configuration (issue #190).
    pub mcp: McpConfig,
}

// ── Source ────────────────────────────────────────────────────────────────

/// Source-link policy controlling how `forget` and lint treat sources
/// under the vault (issue #257; brief §3, §5.6).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourceConfig {
    /// When true, `forget` MUST redact the raw bytes of any forgotten
    /// source file in-place (overwriting body, keeping only hash +
    /// metadata) at the same time it writes the consent-journal row.
    ///
    /// The lint rule `source_redact_on_forget_honored` asserts the
    /// invariant after the fact: every `consent_journal` `SourceForget`
    /// row has a content-redacted source file in `<vault>/sources/`.
    /// Mismatch is `source_redact_skipped`.
    ///
    /// Default `false`: P0 operators can ship without the policy and
    /// lint stays quiet. Turning it on is a deliberate policy bump.
    pub redact_on_forget: bool,
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
#[derive(Debug, Clone, PartialEq)]
pub struct HotMemoryConfig {
    /// Ordered steps in the assembly recipe.
    pub recipe: Vec<HotMemoryRecipeStep>,
    /// Maximum bytes in the assembled hot prefix. Must be > 0.
    pub max_bytes: u32,
    /// Recipe label to use for `PreCompact` reinjection.
    pub pre_compact_recipe: String,
    /// Fraction of `compaction_target` reserved for `PreCompact` reinjection.
    pub pre_compact_safety_ratio: f64,
    /// Name of the recipe to use when the caller does not pass `--recipe`.
    pub default_recipe: String,
    /// Named recipe presets keyed by user-facing recipe name.
    pub recipes: BTreeMap<String, HotMemoryRecipePreset>,
}

/// One named hot-memory recipe preset.
///
/// YAML entries must supply BOTH `steps` and `max_bytes` — partial
/// presets are rejected at deserialize time. This makes the override
/// semantics explicit: overriding a recipe in YAML is a full
/// replacement, not a deep-merge against the built-in entry. Eliminates
/// the "missing field silently widens budget to a generic default"
/// footgun for users overriding e.g. `recipes.wake-up.steps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HotMemoryRecipePreset {
    /// Ordered steps in the recipe preset.
    pub steps: Vec<HotMemoryRecipeStep>,
    /// Maximum bytes for this recipe preset.
    pub max_bytes: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HotMemoryRecipePresetWire {
    steps: Option<Vec<HotMemoryRecipeStep>>,
    max_bytes: Option<u32>,
}

impl<'de> Deserialize<'de> for HotMemoryRecipePreset {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let wire = HotMemoryRecipePresetWire::deserialize(deserializer)?;
        let steps = wire.steps.ok_or_else(|| {
            D::Error::custom("recipe preset is missing required field `steps` (partial overrides are not allowed)")
        })?;
        let max_bytes = wire.max_bytes.ok_or_else(|| {
            D::Error::custom("recipe preset is missing required field `max_bytes` (partial overrides are not allowed)")
        })?;
        Ok(Self { steps, max_bytes })
    }
}

/// Resolved hot-memory recipe selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHotMemoryRecipe<'a> {
    /// User-visible recipe name.
    pub name: String,
    /// Recipe steps to execute.
    pub steps: &'a [HotMemoryRecipeStep],
    /// Effective byte budget for the recipe.
    pub max_bytes: u32,
}

impl Default for HotMemoryConfig {
    fn default() -> Self {
        let recipes = hot_memory_builtin_recipes();
        let chat = recipes
            .get("chat")
            .unwrap_or_else(|| unreachable!("invariant: built-in chat recipe exists"));
        Self {
            recipe: chat.steps.clone(),
            max_bytes: chat.max_bytes,
            pre_compact_recipe: default_pre_compact_recipe(),
            pre_compact_safety_ratio: default_pre_compact_safety_ratio(),
            default_recipe: "chat".into(),
            recipes,
        }
    }
}

fn default_pre_compact_recipe() -> String {
    "handoff".to_owned()
}

fn default_pre_compact_safety_ratio() -> f64 {
    0.30
}

impl HotMemoryConfig {
    /// Resolve the effective named recipe for an `assemble_hot` request.
    #[must_use]
    pub fn resolve_recipe(&self, requested: Option<&str>) -> Option<ResolvedHotMemoryRecipe<'_>> {
        let name = requested.unwrap_or(&self.default_recipe);
        // Named-recipe table is the single source of truth. Flat
        // `recipe`/`max_bytes` are kept only for pre-recipe configs
        // that loaded legacy scalars; the deserializer mirrors those
        // into the default-recipe table entry so this lookup wins in
        // both cases.
        if let Some(recipe) = self.recipes.get(name) {
            return Some(ResolvedHotMemoryRecipe {
                name: name.to_owned(),
                steps: &recipe.steps,
                max_bytes: recipe.max_bytes,
            });
        }
        // Legacy fallback: only fires when the recipes table is
        // completely empty (a pre-recipe config carrying only the flat
        // `recipe`/`max_bytes` scalars). A populated table that simply
        // does not contain `name` returns `None` so callers fail closed
        // — silently substituting stale flat fields for a missing entry
        // (e.g. `default_recipe = "ghost"`) would turn a config typo
        // into misexecution under a misleading recipe name.
        if self.recipes.is_empty() && name == self.default_recipe {
            return Some(ResolvedHotMemoryRecipe {
                name: name.to_owned(),
                steps: &self.recipe,
                max_bytes: self.max_bytes,
            });
        }
        None
    }

    /// Return all known named recipe keys in stable order.
    #[must_use]
    pub fn recipe_names(&self) -> Vec<&str> {
        self.recipes.keys().map(String::as_str).collect()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HotMemoryConfigWire {
    recipe: Option<Vec<HotMemoryRecipeStep>>,
    max_bytes: Option<u32>,
    pre_compact_recipe: Option<String>,
    pre_compact_safety_ratio: Option<f64>,
    default_recipe: Option<String>,
    recipes: BTreeMap<String, HotMemoryRecipePreset>,
}

impl<'de> Deserialize<'de> for HotMemoryConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = HotMemoryConfigWire::deserialize(deserializer)?;
        let mut cfg = HotMemoryConfig::default();
        // Two input modes — figment layering of a serialized default
        // under a user YAML overlay can produce either, never both:
        //   - "new shape": `recipes` and/or `default_recipe` present.
        //     The user opts into named-recipe semantics. The table is
        //     authoritative; legacy scalars in the same input are
        //     treated as historic noise from a default that was
        //     serialized in legacy form and silently dropped — they
        //     never overwrite the resolved table entry, so the
        //     `resolve_recipe` lookup is unambiguous.
        //   - "legacy shape": only `recipe` and/or `max_bytes`. This
        //     is the form a pre-recipe binary writes (and what
        //     Serialize itself emits for the canonical default, so
        //     rollback to such a binary can read this branch's
        //     bootstrap output). Mirror the scalars into
        //     `recipes[default_recipe]` so resolve_recipe still sees
        //     them.
        if let Some(pre_compact_recipe) = raw.pre_compact_recipe {
            cfg.pre_compact_recipe = pre_compact_recipe;
        }
        if let Some(pre_compact_safety_ratio) = raw.pre_compact_safety_ratio {
            cfg.pre_compact_safety_ratio = pre_compact_safety_ratio;
        }
        let raw_has_new_shape = !raw.recipes.is_empty() || raw.default_recipe.is_some();
        if raw_has_new_shape {
            cfg.recipes.extend(raw.recipes);
            if let Some(default_recipe) = raw.default_recipe {
                cfg.default_recipe = default_recipe;
            }
            if let Some(recipe) = cfg.recipes.get(&cfg.default_recipe) {
                cfg.recipe.clone_from(&recipe.steps);
                cfg.max_bytes = recipe.max_bytes;
            }
        } else {
            let legacy_scalars_present = raw.recipe.is_some() || raw.max_bytes.is_some();
            if let Some(recipe) = raw.recipe {
                cfg.recipe = recipe;
            }
            if let Some(max_bytes) = raw.max_bytes {
                cfg.max_bytes = max_bytes;
            }
            if legacy_scalars_present {
                cfg.recipes.insert(
                    cfg.default_recipe.clone(),
                    HotMemoryRecipePreset {
                        steps: cfg.recipe.clone(),
                        max_bytes: cfg.max_bytes,
                    },
                );
            }
        }

        Ok(cfg)
    }
}

impl Serialize for HotMemoryConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Rollback-safety strategy:
        //   - When the config matches the canonical built-in defaults
        //     (no custom recipes, default_recipe == "chat"), emit only
        //     the legacy `recipe`/`max_bytes` scalars. A pre-recipe
        //     binary can read these losslessly. Bootstrap writes the
        //     default config to disk, so the on-disk file produced by
        //     this branch is rollback-safe in the common path.
        //   - Once the user customizes (`default_recipe` override or
        //     custom `recipes` entries), emit the new named-recipe
        //     shape. This is a forward-only step: a pre-recipe binary
        //     will fail closed on the unknown fields. Real rollback
        //     across the customization boundary needs
        //     `deny_unknown_fields` relaxed in an older release first.
        #[derive(Serialize)]
        #[serde(deny_unknown_fields)]
        struct LegacyOut<'a> {
            recipe: &'a [HotMemoryRecipeStep],
            max_bytes: u32,
            pre_compact_recipe: &'a str,
            pre_compact_safety_ratio: f64,
        }
        #[derive(Serialize)]
        #[serde(deny_unknown_fields)]
        struct HotMemoryConfigOut<'a> {
            pre_compact_recipe: &'a str,
            pre_compact_safety_ratio: f64,
            default_recipe: &'a str,
            recipes: BTreeMap<String, HotMemoryRecipePreset>,
        }

        let builtins = hot_memory_builtin_recipes();
        let is_canonical_default = self.default_recipe == "chat" && self.recipes == builtins;
        if is_canonical_default {
            return LegacyOut {
                recipe: &self.recipe,
                max_bytes: self.max_bytes,
                pre_compact_recipe: &self.pre_compact_recipe,
                pre_compact_safety_ratio: self.pre_compact_safety_ratio,
            }
            .serialize(serializer);
        }

        // The recipes table is the single source of truth — emit it
        // verbatim. The flat `recipe`/`max_bytes` fields are kept only
        // for legacy back-compat at deserialize time; reconstructing
        // the default-recipe entry from them here would silently
        // overwrite a programmatic edit to `recipes[default_recipe]`.
        HotMemoryConfigOut {
            pre_compact_recipe: &self.pre_compact_recipe,
            pre_compact_safety_ratio: self.pre_compact_safety_ratio,
            default_recipe: &self.default_recipe,
            recipes: self.recipes.clone(),
        }
        .serialize(serializer)
    }
}

fn hot_memory_builtin_recipes() -> BTreeMap<String, HotMemoryRecipePreset> {
    BTreeMap::from([
        (
            "chat".into(),
            HotMemoryRecipePreset {
                steps: vec![
                    HotMemoryRecipeStep::Purpose,
                    HotMemoryRecipeStep::Index,
                    HotMemoryRecipeStep::PinnedFeedback,
                    HotMemoryRecipeStep::TopSalienceProject,
                    HotMemoryRecipeStep::ActivePlaybook,
                    HotMemoryRecipeStep::RecentUserSignal,
                ],
                max_bytes: 25_600,
            },
        ),
        // wake-up / debug / handoff are built from cairn.mcp.v1
        // steps only. The richer steps the brief sketches
        // (`last_session_digest`, `recent_failures`, `contradictions`,
        // `last_session_summary`) require a v2 contract bump; until
        // then the presets approximate intent using v1 steps. Operators
        // wanting custom shapes can declare recipes in
        // `.cairn/config.yaml` (exercised by the
        // `cairn_assemble_hot_custom_config_recipe_requires_no_code_change`
        // smoke test).
        (
            "wake-up".into(),
            HotMemoryRecipePreset {
                steps: vec![
                    HotMemoryRecipeStep::Purpose,
                    HotMemoryRecipeStep::RecentUserSignal,
                ],
                max_bytes: 8_192,
            },
        ),
        (
            "debug".into(),
            HotMemoryRecipePreset {
                steps: vec![
                    HotMemoryRecipeStep::Purpose,
                    HotMemoryRecipeStep::PinnedFeedback,
                    HotMemoryRecipeStep::TopSalienceProject,
                    HotMemoryRecipeStep::RecentUserSignal,
                ],
                max_bytes: 16_384,
            },
        ),
        (
            "handoff".into(),
            HotMemoryRecipePreset {
                steps: vec![
                    HotMemoryRecipeStep::Purpose,
                    HotMemoryRecipeStep::Index,
                    HotMemoryRecipeStep::ActivePlaybook,
                    HotMemoryRecipeStep::RecentUserSignal,
                ],
                max_bytes: 16_384,
            },
        ),
    ])
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

/// Check whether `config.search.default_provider` and
/// `config.search.embedding_model` are mutually consistent.
///
/// This is a pure, no-I/O predicate used by the SDK and MCP surfaces (which
/// have no access to env vars like `OPENAI_API_KEY`) to gate semantic/hybrid
/// capability advertisement when the model selection contradicts the provider.
///
/// The classic misconfiguration the check catches:
/// `default_provider = openai` but `embedding_model = bge-small-en-v1.5`
/// — the `OpenAI` HTTP endpoint cannot serve a local candle model, so the ANN
/// dispatcher would silently return zero results for every semantic query
/// (rows indexed with BGE vectors have a `vec_model` label that the `OpenAI`
/// dispatcher filter never matches).
///
/// Alignment rules:
/// - `Local` provider → model must be a locally-runnable candle variant
///   (`BgeSmallEnV1_5` or `AllMiniLmL6V2`).
/// - `OpenAi` provider → model must be a native `OpenAI` variant
///   (`OpenAiTextEmbedding3Small` or `OpenAiTextEmbedding3Large`).
/// - Any future provider not yet listed here → `false` (fail-closed per
///   CLAUDE.md §4.6). Update this function when a new provider lands.
///
/// The CLI's `embedding_provider_ready` additionally checks
/// `OPENAI_API_KEY` presence and the `openai` Cargo feature flag. This
/// function intentionally does not — those checks require I/O or feature
/// gating that cannot be performed inside `cairn-core`.
#[must_use]
pub fn provider_model_aligned(config: &CairnConfig) -> bool {
    match config.search.default_provider {
        EmbeddingProvider::Local => matches!(
            config.search.embedding_model,
            EmbeddingModelKind::BgeSmallEnV1_5 | EmbeddingModelKind::AllMiniLmL6V2
        ),
        EmbeddingProvider::OpenAi => matches!(
            config.search.embedding_model,
            EmbeddingModelKind::OpenAiTextEmbedding3Small
                | EmbeddingModelKind::OpenAiTextEmbedding3Large
        ),
        // Future providers: gate-closed by default until this function is updated.
        #[allow(unreachable_patterns)]
        _ => false,
    }
}

/// Derived capability set, computed from `CairnConfig` (no I/O).
///
/// The verb layer calls `config.capabilities(embedding_provider_ready)` before dispatching to
/// gate features that require capabilities that may not be present.
// Six orthogonal capability flags; a bitflags type would obscure the intent.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySet {
    /// Always true at P0 (`FTS5` always present).
    pub keyword_search: bool,
    /// True iff `search.local_embeddings` is true and the embedding provider is ready
    /// (local: model files on disk; cloud: feature compiled in + API key set).
    pub semantic_search: bool,
    /// True iff `search.local_embeddings` is true and the embedding provider is ready
    /// (hybrid uses keyword + semantic legs; both require an active embedder).
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
    /// True iff `cairn.mcp.v1.replay.sequence` capability is advertised
    /// — sequence-mode envelopes (`signed_intent.sequence`) admit
    /// against the per-issuer CAS in `issuer_seq` (brief §4.2). Always
    /// true at P0 once the vault is bound; the schema ships
    /// unconditionally.
    pub replay_sequence: bool,
    /// True iff `cairn.mcp.v1.replay.challenge` capability is
    /// advertised — challenge-mode envelopes
    /// (`signed_intent.server_challenge`) admit by consuming an
    /// outstanding row in `outstanding_challenges` minted via
    /// `cairn handshake` (issue #52, brief §4.2). Always true at P0
    /// once the vault is bound; the schema ships unconditionally.
    pub replay_challenge: bool,
}

impl CairnConfig {
    /// Validate semantic invariants that serde cannot enforce.
    ///
    /// # Errors
    /// See [`ConfigError`] variants for the full list.
    #[allow(
        clippy::too_many_lines,
        reason = "linear validation table; splitting hurts readability"
    )]
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

        // 3. hot_memory.max_bytes must be > 0. Check the legacy scalar
        //    only when there is no named-recipe table to resolve from;
        //    otherwise the per-recipe loop below validates each entry's
        //    max_bytes and the table is the authoritative source.
        if self.vault.hot_memory.recipes.is_empty() && self.vault.hot_memory.max_bytes == 0 {
            return Err(ConfigError::InvalidBudget {
                field: "vault.hot_memory.max_bytes",
                value: 0_u64,
            });
        }
        if self.vault.hot_memory.pre_compact_safety_ratio.is_nan()
            || self.vault.hot_memory.pre_compact_safety_ratio <= 0.0
            || self.vault.hot_memory.pre_compact_safety_ratio > 1.0
        {
            return Err(ConfigError::InvalidRatio {
                field: "vault.hot_memory.pre_compact_safety_ratio",
                value: self.vault.hot_memory.pre_compact_safety_ratio,
            });
        }

        if self.vault.hot_memory.default_recipe.is_empty() {
            return Err(ConfigError::InvalidRecipeName {
                field: "vault.hot_memory.default_recipe",
                reason: "must not be empty",
            });
        }
        if !self
            .vault
            .hot_memory
            .recipes
            .contains_key(&self.vault.hot_memory.default_recipe)
        {
            return Err(ConfigError::MissingHotMemoryDefaultRecipe {
                name: self.vault.hot_memory.default_recipe.clone(),
            });
        }
        for (name, recipe) in &self.vault.hot_memory.recipes {
            if name.is_empty() {
                return Err(ConfigError::InvalidRecipeName {
                    field: "vault.hot_memory.recipes",
                    reason: "recipe key must not be empty",
                });
            }
            if recipe.max_bytes == 0 {
                return Err(ConfigError::InvalidBudget {
                    field: "vault.hot_memory.recipes[].max_bytes",
                    value: 0_u64,
                });
            }
            // Empty `steps` is intentionally allowed — operators can
            // declare a recipe with no steps to disable hot-memory
            // assembly for that preset. The assembler honors this and
            // emits `segments: []` with an empty prefix (covered by
            // `assemble_hot_empty_recipe`).
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
    /// `embedding_provider_ready` should be `true` when the configured embedding
    /// provider can produce vectors end-to-end:
    /// - For `default_provider = local`: the model files exist on disk
    ///   (stat-checked via `ModelCache::is_present`).
    /// - For `default_provider = openai`: the `openai` Cargo feature is compiled
    ///   in AND `OPENAI_API_KEY` is set in the environment.
    ///
    /// The verb layer uses this to gate features before dispatch.
    #[must_use]
    pub fn capabilities(&self, embedding_provider_ready: bool) -> CapabilitySet {
        let llm_on = self.llm.provider.is_some();
        let semantic = self.search.local_embeddings && embedding_provider_ready;
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
            // Both replay modes have substrate support (migration 0046,
            // `replay::prepare_wal_with_replay`, `mint_challenge`) shipped
            // by issue #52, but the signed-verb dispatch path does not
            // yet route through them. Advertising the capability before
            // the dispatch is honest end-to-end would over-advertise per
            // brief §15. These flags flip to `true` in the follow-up that
            // wires verb dispatch — see `cairn-cli/src/verbs/status.rs`.
            replay_sequence: false,
            replay_challenge: false,
        }
    }

    /// Convenience: equivalent to `capabilities(false)`.
    /// Use when no embedding provider is ready (e.g., pure config tests, no
    /// model on disk, no API key in environment).
    #[must_use]
    pub fn capabilities_no_model(&self) -> CapabilitySet {
        self.capabilities(false)
    }

    /// Run cross-section invariants that serde alone cannot express.
    ///
    /// Currently checks:
    /// - `[mcp.stdio] single_tenant + principal` consistency
    ///   ([`validate_mcp_config`]).
    ///
    /// Existing validators (pipeline, retention, etc.) keep their own
    /// entry points; this method composes the new MCP check without
    /// disturbing them.
    ///
    /// # Errors
    /// Returns the first [`ConfigError`] encountered.
    pub fn validate_mcp(&self) -> Result<(), ConfigError> {
        validate_mcp_config(&self.mcp)
    }

    /// Single shared predicate that gates `cairn status` MCP-graph reporting
    /// and the MCP `tools/list` / `tools/call` graph-tool advertisement.
    /// Both surfaces read the same function so they cannot drift.
    ///
    /// The deliberate fall-through order (most-specific reason wins) is:
    ///
    /// 1. `single_tenant == false` → `UnavailableSingleTenantOff`
    /// 2. else if `scope.is_none()` → `UnavailableNoScopeResolver`
    /// 3. else if `!store_caps.graph_edges` → `UnavailableNoStoreCapability`
    /// 4. else → `Available { tool_count: 5 }` (Plan C: graph tools landed)
    ///
    /// Note: the predicate does **not** call `scope.allowed_scopes`. That
    /// single resolver call lives in `Handler::materialize_graph_request`,
    /// which calls this predicate first and then resolves scopes exactly once.
    ///
    /// The `transport` argument is currently always `Stdio` and the body
    /// branches only on `Stdio`. Future SSE / HTTP transports add their
    /// own branches with their own per-transport preconditions.
    #[must_use]
    pub fn mcp_graph_tools_available(
        &self,
        scope: Option<&dyn crate::mcp_auth::McpSessionScope>,
        transport: crate::mcp_auth::McpTransport,
        store_caps: &crate::contract::memory_store::MemoryStoreCapabilities,
    ) -> crate::mcp_auth::McpGraphAvailability {
        use crate::mcp_auth::{McpGraphAvailability, McpTransport};

        match transport {
            McpTransport::Stdio => {
                if !self.mcp.stdio.single_tenant {
                    return McpGraphAvailability::UnavailableSingleTenantOff;
                }
                if scope.is_none() {
                    return McpGraphAvailability::UnavailableNoScopeResolver;
                }
                if !store_caps.graph_edges {
                    return McpGraphAvailability::UnavailableNoStoreCapability;
                }
                // Plan C: graph tools have landed; advertise them.
                McpGraphAvailability::Available { tool_count: 5 }
            }
        }
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
    fn config_error_ratio_display() {
        let e = ConfigError::InvalidRatio {
            field: "vault.hot_memory.pre_compact_safety_ratio",
            value: 1.25,
        };
        assert_eq!(
            e.to_string(),
            "invalid ratio for vault.hot_memory.pre_compact_safety_ratio: value 1.25 must be > 0 and <= 1"
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
        // The named-recipe table is the authoritative source of truth.
        // A zero budget on the resolved default recipe must be rejected
        // regardless of what the legacy flat scalar says.
        let mut config = CairnConfig::default();
        if let Some(entry) = config.vault.hot_memory.recipes.get_mut("chat") {
            entry.max_bytes = 0;
        }
        let err = config.validate().unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidBudget {
                field: "vault.hot_memory.recipes[].max_bytes",
                ..
            }
        ));
    }

    #[test]
    fn validate_rejects_default_recipe_not_in_recipes() {
        let mut config = CairnConfig::default();
        config.vault.hot_memory.default_recipe = "ghost".into();
        let err = config.validate().unwrap_err();
        match err {
            ConfigError::MissingHotMemoryDefaultRecipe { name } => {
                assert_eq!(name, "ghost");
            }
            other => panic!("expected MissingHotMemoryDefaultRecipe, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_rejects_partial_recipe_preset_missing_max_bytes() {
        // A YAML override like `recipes.wake-up.steps: [...]` previously
        // silently widened the budget to a generic default by relying on
        // `HotMemoryRecipePreset::default`. Now an incomplete preset is
        // rejected at deserialize so users opt into a full replacement
        // explicitly (or override one of the built-in entries verbatim).
        let json = r#"{
            "recipes": {
                "wake-up": { "steps": ["purpose"] }
            }
        }"#;
        let err = serde_json::from_str::<HotMemoryConfig>(json)
            .expect_err("partial recipe preset must be rejected");
        assert!(
            err.to_string().contains("max_bytes"),
            "error should mention the missing field: {err}"
        );
    }

    #[test]
    fn deserialize_rejects_partial_recipe_preset_missing_steps() {
        let json = r#"{
            "recipes": {
                "wake-up": { "max_bytes": 4096 }
            }
        }"#;
        let err = serde_json::from_str::<HotMemoryConfig>(json)
            .expect_err("partial recipe preset must be rejected");
        assert!(
            err.to_string().contains("steps"),
            "error should mention the missing field: {err}"
        );
    }

    #[test]
    fn validate_accepts_in_process_recipes_mutation_with_stale_legacy_max_bytes() {
        // Regression: a programmatic editor that mutates
        // `recipes[default_recipe].max_bytes` without mirroring the
        // legacy flat scalar must not be vetoed by validate(). The
        // named-recipe table is the authoritative source.
        let mut config = CairnConfig::default();
        config.vault.hot_memory.max_bytes = 0; // stale legacy
        if let Some(entry) = config.vault.hot_memory.recipes.get_mut("chat") {
            entry.max_bytes = 16_384;
        }
        config
            .validate()
            .expect("named-recipe budget is authoritative");
    }

    #[test]
    fn validate_accepts_recipe_with_empty_steps() {
        // Compat: operators can declare a recipe with `steps: []` to
        // disable hot-memory assembly for that preset. Validation must
        // not reject this — the assembler emits `segments: []` for it.
        let mut config = CairnConfig::default();
        config.vault.hot_memory.recipes.insert(
            "disabled".into(),
            HotMemoryRecipePreset {
                steps: vec![],
                max_bytes: 1024,
            },
        );
        config.validate().expect("empty steps must be accepted");
    }

    #[test]
    fn validate_rejects_empty_default_recipe() {
        let mut config = CairnConfig::default();
        config.vault.hot_memory.default_recipe = String::new();
        let err = config.validate().unwrap_err();
        match err {
            ConfigError::InvalidRecipeName { field, .. } => {
                assert_eq!(field, "vault.hot_memory.default_recipe");
            }
            other => panic!("expected InvalidRecipeName, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_empty_recipe_table_key() {
        let mut config = CairnConfig::default();
        config.vault.hot_memory.recipes.insert(
            String::new(),
            HotMemoryRecipePreset {
                steps: vec![HotMemoryRecipeStep::Purpose],
                max_bytes: 1024,
            },
        );
        let err = config.validate().unwrap_err();
        match err {
            ConfigError::InvalidRecipeName { field, .. } => {
                assert_eq!(field, "vault.hot_memory.recipes");
            }
            other => panic!("expected InvalidRecipeName, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_recipe_with_zero_max_bytes() {
        let mut config = CairnConfig::default();
        config.vault.hot_memory.recipes.insert(
            "broken".into(),
            HotMemoryRecipePreset {
                steps: vec![HotMemoryRecipeStep::Purpose],
                max_bytes: 0,
            },
        );
        let err = config.validate().unwrap_err();
        assert!(matches!(err, ConfigError::InvalidBudget { .. }));
    }

    #[test]
    fn resolve_recipe_chat_matches_default() {
        let cfg = HotMemoryConfig::default();
        let with_flag = cfg.resolve_recipe(Some("chat")).expect("chat resolves");
        let default = cfg.resolve_recipe(None).expect("default resolves");
        assert_eq!(with_flag.steps, default.steps);
        assert_eq!(with_flag.max_bytes, default.max_bytes);
        assert_eq!(with_flag.name, default.name);
    }

    #[test]
    fn resolve_recipe_user_override_of_built_in_wins() {
        // A user redefining `chat` in config must shadow the built-in.
        let mut cfg = HotMemoryConfig::default();
        cfg.recipes.insert(
            "chat".into(),
            HotMemoryRecipePreset {
                steps: vec![HotMemoryRecipeStep::Purpose],
                max_bytes: 999,
            },
        );
        let r = cfg.resolve_recipe(Some("chat")).expect("chat resolves");
        // resolve_recipe short-circuits on default_recipe and reads cfg.recipe;
        // the user-supplied recipe block only takes effect after deserialize
        // mirrors it into cfg.recipe. Direct in-process mutation does not, so
        // assert against the recipes table to capture the user override
        // intent at the source.
        assert_eq!(cfg.recipes["chat"].max_bytes, 999);
        // ResolvedHotMemoryRecipe always reports a `chat` name when chat is
        // requested or default.
        assert_eq!(r.name, "chat");
    }

    #[test]
    fn resolve_recipe_unknown_returns_none() {
        let cfg = HotMemoryConfig::default();
        assert!(cfg.resolve_recipe(Some("nope")).is_none());
    }

    #[test]
    fn resolve_recipe_unknown_default_with_populated_table_returns_none() {
        // Regression: an in-process mutation like
        // `default_recipe = "ghost"` against a populated recipes table
        // must fail closed (None), not silently fall back to the flat
        // `recipe`/`max_bytes` scalars under a misleading recipe name.
        let cfg = HotMemoryConfig {
            default_recipe: "ghost".into(),
            ..HotMemoryConfig::default()
        };
        assert!(
            cfg.resolve_recipe(None).is_none(),
            "missing default in a populated table must not fall back to legacy flat fields"
        );
    }

    #[test]
    fn recipe_step_unknown_variant_rejected_at_parse() {
        // The HotMemoryRecipeStep enum is closed — adding a config recipe
        // step the binary does not know about must fail at deserialize
        // time, not silently fall through to an unrecognized step.
        let json = r#"["purpose","not_a_real_step"]"#;
        let result: Result<Vec<HotMemoryRecipeStep>, _> = serde_json::from_str(json);
        assert!(result.is_err(), "unknown step must be rejected");
    }

    #[test]
    fn deserialize_new_shape_wins_over_legacy_scalars() {
        // Figment layering can produce an input that carries both the
        // user-supplied new shape and the historic legacy scalars from
        // a serialized default. The new shape is authoritative and the
        // legacy fields are dropped, so `resolve_recipe` sees a single
        // unambiguous source of truth.
        let json = r#"{
            "default_recipe": "chat",
            "recipes": {
                "chat": { "steps": ["purpose","index"], "max_bytes": 999 }
            },
            "max_bytes": 42
        }"#;
        let cfg: HotMemoryConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.max_bytes, 999);
        assert_eq!(cfg.recipes["chat"].max_bytes, 999);
    }

    #[test]
    fn deserialize_legacy_only_mirrors_into_recipes_table() {
        // A pure-legacy config (pre-recipe binary's output, or what
        // Serialize emits for the canonical default) must populate the
        // recipes table so resolve_recipe sees the user's overrides.
        let json = r#"{
            "recipe": ["purpose","index"],
            "max_bytes": 42
        }"#;
        let cfg: HotMemoryConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.max_bytes, 42);
        assert_eq!(cfg.recipes["chat"].max_bytes, 42);
        assert_eq!(cfg.recipes["chat"].steps.len(), 2);
        let r = cfg.resolve_recipe(None).expect("chat resolves");
        assert_eq!(r.max_bytes, 42);
        assert_eq!(r.steps.len(), 2);
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
    fn hot_memory_defaults_round_trip() {
        let hot_memory = HotMemoryConfig::default();
        let json = serde_json::to_string(&hot_memory).expect("hot_memory serializes");
        let round_trip: HotMemoryConfig =
            serde_json::from_str(&json).expect("hot_memory deserializes");
        assert_eq!(round_trip, hot_memory);
        assert_eq!(round_trip.pre_compact_recipe, "handoff");
        assert!((round_trip.pre_compact_safety_ratio - 0.30).abs() < f64::EPSILON);
    }

    #[test]
    fn validate_rejects_pre_compact_safety_ratio_above_one() {
        let mut config = CairnConfig::default();
        config.vault.hot_memory.pre_compact_safety_ratio = 1.01;
        let err = config.validate().unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidRatio {
                field: "vault.hot_memory.pre_compact_safety_ratio",
                ..
            }
        ));
    }

    #[test]
    fn validate_rejects_pre_compact_safety_ratio_nan() {
        let mut config = CairnConfig::default();
        config.vault.hot_memory.pre_compact_safety_ratio = f64::NAN;
        let err = config.validate().unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidRatio {
                field: "vault.hot_memory.pre_compact_safety_ratio",
                ..
            }
        ));
    }

    #[test]
    fn capabilities_llm_off_by_default() {
        let caps = CairnConfig::default().capabilities(false);
        assert!(caps.keyword_search, "keyword_search always true");
        assert!(!caps.semantic_search, "provider not ready → no semantic");
        assert!(
            !caps.hybrid_search,
            "provider not ready → no hybrid: runtime resolves an embedder for hybrid \
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
            "provider not ready → no semantic even with LLM"
        );
        assert!(
            !caps.hybrid_search,
            "provider not ready → no hybrid: hybrid requires the embedder the runtime \
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
    fn canonical_default_serializes_to_legacy_form_for_rollback() {
        // Rollback-safety pin: when the user has not customized the
        // recipe table, Serialize must emit the legacy `recipe` /
        // `max_bytes` scalar shape only. A pre-recipe binary's
        // `deny_unknown_fields` deserializer can read this losslessly.
        let cfg = HotMemoryConfig::default();
        let value = serde_json::to_value(&cfg).expect("serialize");
        let obj = value.as_object().expect("object");
        assert!(
            obj.contains_key("recipe"),
            "legacy `recipe` must be present"
        );
        assert!(
            obj.contains_key("max_bytes"),
            "legacy `max_bytes` must be present"
        );
        assert!(
            !obj.contains_key("recipes"),
            "canonical default must not emit `recipes` (rollback hazard)"
        );
        assert!(
            !obj.contains_key("default_recipe"),
            "canonical default must not emit `default_recipe` (rollback hazard)"
        );
    }

    #[test]
    fn customized_config_serializes_new_shape() {
        // Once the user customizes, Serialize must switch to the new
        // shape so resolve_recipe sees the user's table on next load.
        // (Forward-only — documented limitation.)
        let mut cfg = HotMemoryConfig::default();
        cfg.recipes.insert(
            "tiny".into(),
            HotMemoryRecipePreset {
                steps: vec![HotMemoryRecipeStep::Purpose],
                max_bytes: 1024,
            },
        );
        let value = serde_json::to_value(&cfg).expect("serialize");
        let obj = value.as_object().expect("object");
        assert!(obj.contains_key("recipes"));
        assert!(obj.contains_key("default_recipe"));
        assert!(
            !obj.contains_key("recipe"),
            "new shape must not include legacy scalar"
        );
    }

    #[test]
    fn serialize_then_deserialize_default_roundtrips_resolution() {
        // Roundtrip: serialize the canonical default (legacy form),
        // deserialize, and verify resolve_recipe(None) returns a result
        // byte-identical to the original.
        let original = HotMemoryConfig::default();
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: HotMemoryConfig = serde_json::from_str(&json).expect("deserialize");
        let orig_r = original.resolve_recipe(None).expect("original resolves");
        let parsed_r = parsed.resolve_recipe(None).expect("parsed resolves");
        assert_eq!(orig_r.name, parsed_r.name);
        assert_eq!(orig_r.max_bytes, parsed_r.max_bytes);
        assert_eq!(orig_r.steps, parsed_r.steps);
    }

    #[test]
    fn semantic_on_when_local_embeddings_and_provider_ready() {
        let config = CairnConfig::default();
        let caps = config.capabilities(true);
        assert!(caps.semantic_search);
        assert!(caps.hybrid_search, "hybrid on when local_embeddings: true");
    }

    #[test]
    fn semantic_off_when_local_embeddings_false() {
        let mut config = CairnConfig::default();
        config.search.local_embeddings = false;
        let caps = config.capabilities(true); // provider ready but opt-out
        assert!(!caps.semantic_search);
    }

    #[test]
    fn semantic_off_when_provider_not_ready() {
        let config = CairnConfig::default(); // local_embeddings: true
        let caps = config.capabilities(false); // provider not ready
        assert!(!caps.semantic_search);
    }

    #[test]
    fn semantic_not_tied_to_llm_provider() {
        let mut config = CairnConfig::default();
        // LLM present but embedding provider not ready → semantic still false.
        config.llm.provider = Some(LlmProvider::OpenaiCompatible);
        let caps = config.capabilities(false);
        assert!(!caps.semantic_search);
        // Embedding provider ready → semantic true regardless of LLM.
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

    #[test]
    fn validate_mcp_rejects_single_tenant_without_principal() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = None;
        let err = cfg.validate_mcp().unwrap_err();
        assert!(
            matches!(err, ConfigError::McpStdioMissingPrincipal),
            "got: {err:?}",
        );
    }

    #[test]
    fn validate_mcp_accepts_single_tenant_with_principal() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(crate::domain::ScopeTuple {
            tenant: Some("acme".into()),
            ..crate::domain::ScopeTuple::default()
        });
        cfg.validate_mcp().expect("valid config");
    }

    #[test]
    fn validate_mcp_accepts_default_config() {
        // Default: single_tenant = false, principal = None — cleanly valid.
        let cfg = CairnConfig::default();
        cfg.validate_mcp().expect("default config is valid");
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

    use crate::contract::memory_store::MemoryStoreCapabilities;
    use crate::mcp_auth::{ConfigBackedScope, McpGraphAvailability, McpSessionScope, McpTransport};

    fn store_caps_with_graph(graph: bool) -> MemoryStoreCapabilities {
        MemoryStoreCapabilities {
            fts: true,
            vector: false,
            graph_edges: graph,
            transactions: true,
            per_record_consent_model: true,
            graph_search: graph,
        }
    }

    fn principal_acme() -> crate::domain::ScopeTuple {
        crate::domain::ScopeTuple {
            tenant: Some("acme".into()),
            ..crate::domain::ScopeTuple::default()
        }
    }

    #[test]
    fn graph_tools_unavailable_when_single_tenant_off() {
        let cfg = CairnConfig::default(); // single_tenant defaults to false
        let scope = ConfigBackedScope::new(principal_acme());
        let caps = store_caps_with_graph(true);
        let s: &dyn McpSessionScope = &scope;
        let avail = cfg.mcp_graph_tools_available(Some(s), McpTransport::Stdio, &caps);
        assert_eq!(avail, McpGraphAvailability::UnavailableSingleTenantOff);
    }

    #[test]
    fn graph_tools_unavailable_when_no_scope_resolver() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal_acme());
        let caps = store_caps_with_graph(true);
        let avail = cfg.mcp_graph_tools_available(None, McpTransport::Stdio, &caps);
        assert_eq!(avail, McpGraphAvailability::UnavailableNoScopeResolver);
    }

    #[test]
    fn graph_tools_unavailable_when_store_lacks_graph_capability() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal_acme());
        let scope = ConfigBackedScope::new(principal_acme());
        let caps = store_caps_with_graph(false);
        let s: &dyn McpSessionScope = &scope;
        let avail = cfg.mcp_graph_tools_available(Some(s), McpTransport::Stdio, &caps);
        assert_eq!(avail, McpGraphAvailability::UnavailableNoStoreCapability);
    }

    /// Minimal static scope for tests — returns a fixed allowed-scope set.
    struct StaticScope {
        allowed: Vec<crate::domain::ScopeTuple>,
    }

    impl StaticScope {
        fn new(allowed: Vec<crate::domain::ScopeTuple>) -> Self {
            Self { allowed }
        }
    }

    impl McpSessionScope for StaticScope {
        fn allowed_scopes(
            &self,
            _ctx: &crate::mcp_auth::McpAuthContext<'_>,
        ) -> Result<Vec<crate::domain::ScopeTuple>, crate::mcp_auth::ScopeResolutionError> {
            Ok(self.allowed.clone())
        }
    }

    fn config_with_single_tenant_stdio() -> CairnConfig {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal_acme());
        cfg
    }

    /// Plan C: with all conditions met, the predicate must return
    /// `Available { tool_count: 5 }`.
    #[test]
    fn mcp_graph_tools_available_returns_five_when_all_conditions_hold() {
        let cfg = config_with_single_tenant_stdio();
        let store_caps = MemoryStoreCapabilities {
            graph_edges: true,
            ..Default::default()
        };
        let scope = StaticScope::new(vec![crate::domain::ScopeTuple::default()]);
        let av = cfg.mcp_graph_tools_available(Some(&scope), McpTransport::Stdio, &store_caps);
        assert!(
            matches!(av, McpGraphAvailability::Available { tool_count: 5 }),
            "Plan C: all conditions met must return Available{{5}}; got {av:?}",
        );
    }

    /// Previously the Plan A test. Now that Plan C has landed, the same
    /// all-conditions-hold scenario must return `Available { tool_count: 5 }`.
    #[test]
    fn graph_tools_available_when_all_conditions_hold() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal_acme());
        let scope = ConfigBackedScope::new(principal_acme());
        let caps = store_caps_with_graph(true);
        let s: &dyn McpSessionScope = &scope;
        let avail = cfg.mcp_graph_tools_available(Some(s), McpTransport::Stdio, &caps);
        assert!(
            matches!(avail, McpGraphAvailability::Available { tool_count: 5 }),
            "Plan C must emit Available{{5}} when all conditions hold; got {avail:?}",
        );
    }

    // ── provider_model_aligned tests (round-4 review Finding A) ──────────────

    #[test]
    fn provider_model_aligned_local_with_local_model_is_true() {
        let mut cfg = CairnConfig::default();
        cfg.search.default_provider = EmbeddingProvider::Local;
        cfg.search.embedding_model = EmbeddingModelKind::BgeSmallEnV1_5;
        assert!(super::provider_model_aligned(&cfg));

        cfg.search.embedding_model = EmbeddingModelKind::AllMiniLmL6V2;
        assert!(super::provider_model_aligned(&cfg));
    }

    #[test]
    fn provider_model_aligned_local_with_openai_model_is_false() {
        let mut cfg = CairnConfig::default();
        cfg.search.default_provider = EmbeddingProvider::Local;
        cfg.search.embedding_model = EmbeddingModelKind::OpenAiTextEmbedding3Small;
        assert!(!super::provider_model_aligned(&cfg));

        cfg.search.embedding_model = EmbeddingModelKind::OpenAiTextEmbedding3Large;
        assert!(!super::provider_model_aligned(&cfg));
    }

    #[test]
    fn provider_model_aligned_openai_with_openai_model_is_true() {
        let mut cfg = CairnConfig::default();
        cfg.search.default_provider = EmbeddingProvider::OpenAi;
        cfg.search.embedding_model = EmbeddingModelKind::OpenAiTextEmbedding3Small;
        assert!(super::provider_model_aligned(&cfg));

        cfg.search.embedding_model = EmbeddingModelKind::OpenAiTextEmbedding3Large;
        assert!(super::provider_model_aligned(&cfg));
    }

    #[test]
    fn provider_model_aligned_openai_with_local_model_is_false() {
        // This is the classic misconfiguration the round-4 review caught:
        // `default_provider = openai` with a candle model. The gate must
        // return false so SDK/MCP don't advertise semantic/hybrid.
        let mut cfg = CairnConfig::default();
        cfg.search.default_provider = EmbeddingProvider::OpenAi;
        cfg.search.embedding_model = EmbeddingModelKind::BgeSmallEnV1_5;
        assert!(!super::provider_model_aligned(&cfg));

        cfg.search.embedding_model = EmbeddingModelKind::AllMiniLmL6V2;
        assert!(!super::provider_model_aligned(&cfg));
    }
}
