# Config Schema Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Define typed config structs, validation, and capability derivation in `cairn-core`; add a figment-based loader and `cairn bootstrap` command in `cairn-cli`.

**Architecture:** Pure serde types + validation live in `cairn-core::config` (no I/O). The figment loading stack (defaults → YAML file → `CAIRN_*` env → CLI overrides) lives in `cairn-cli::config`. The `${VAR}` interpolation pre-processor runs on raw YAML bytes before figment sees them.

**Tech Stack:** `serde` (already in workspace), `figment` + `yaml` feature, `serde_yaml 0.9`, `regex`, `insta` (snapshot), `proptest` (round-trip), `tempfile` (integration tests).

**Design spec:** `docs/superpowers/specs/2026-04-26-config-schema-design.md`  
**Brief sections:** §3.1, §4.1, §5.2.a  

---

## File Map

| Action | Path | Responsibility |
|---|---|---|
| Modify | `Cargo.toml` | Add `figment`, `serde_yaml`, `regex` to workspace deps |
| Modify | `crates/cairn-core/Cargo.toml` | Add `insta` to dev-deps |
| Modify | `crates/cairn-core/src/lib.rs` | `pub mod config;` |
| **Create** | `crates/cairn-core/src/config/mod.rs` | All config types, errors, `validate()`, `capabilities()` |
| Modify | `crates/cairn-cli/Cargo.toml` | Add `figment`, `serde_yaml`, `regex`; add `tempfile`, `proptest` to dev-deps |
| Modify | `crates/cairn-cli/src/lib.rs` | `pub mod config;` |
| **Create** | `crates/cairn-cli/src/config.rs` | `interpolate_env`, `load`, `write_default`, `CliOverrides` |
| Modify | `crates/cairn-cli/src/main.rs` | Add `bootstrap` subcommand wired to `config::write_default` |
| **Create** | `crates/cairn-cli/tests/config.rs` | Integration tests for loader + bootstrap |

---

## Task 1: Workspace deps + cairn-core module scaffold

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/cairn-core/Cargo.toml`
- Modify: `crates/cairn-core/src/lib.rs`
- Create: `crates/cairn-core/src/config/mod.rs`

- [ ] **Step 1: Add figment, serde_yaml, regex to workspace Cargo.toml**

In `Cargo.toml`, inside `[workspace.dependencies]`, add after the `toml` line:

```toml
figment = { version = "0.10", features = ["yaml"] }
regex = { version = "1", default-features = false, features = ["std"] }
serde_yaml = "0.9"
```

- [ ] **Step 2: Add insta to cairn-core dev-deps**

In `crates/cairn-core/Cargo.toml`, add to `[dev-dependencies]`:

```toml
insta = { workspace = true }
```

- [ ] **Step 3: Expose the config module in cairn-core**

In `crates/cairn-core/src/lib.rs`, add after the existing `pub mod` lines:

```rust
pub mod config;
```

- [ ] **Step 4: Write the failing test for ConfigError display**

Create `crates/cairn-core/src/config/mod.rs` with just the error type and one failing test:

```rust
//! Typed config structs for `.cairn/config.yaml` (brief §3.1, §4.1, §5.2.a).

use crate::contract::registry::PluginError;

/// Errors produced during config validation or env-var interpolation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// A `custom:<name>` plugin name failed the `PluginName` grammar check.
    #[error("invalid plugin name for {field}: {source}")]
    InvalidPluginName {
        field: &'static str,
        #[source]
        source: PluginError,
    },
    /// A numeric budget field was set to zero.
    #[error("invalid budget for {field}: value {value} must be > 0")]
    InvalidBudget { field: &'static str, value: u64 },
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
```

- [ ] **Step 5: Verify tests fail (can't compile yet — that's fine; cargo check reports the error)**

```bash
cargo check -p cairn-core --locked 2>&1 | head -20
```

Expected: compiles cleanly (ConfigError has no other deps yet). If errors appear about missing imports, fix them before proceeding.

- [ ] **Step 6: Run the tests**

```bash
cargo nextest run -p cairn-core config:: --locked 2>&1 | tail -10
```

Expected: both tests PASS.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/cairn-core/Cargo.toml crates/cairn-core/src/lib.rs crates/cairn-core/src/config/mod.rs
git commit -m "feat(config): module scaffold + ConfigError (§3.1 #39)"
```

---

## Task 2: Closed-set enums

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`

- [ ] **Step 1: Write failing tests for enum serde**

Add to the `#[cfg(test)] mod tests` block in `config/mod.rs`:

```rust
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
```

- [ ] **Step 2: Run — expect compile error (types undefined)**

```bash
cargo check -p cairn-core --locked 2>&1 | grep "^error" | head -10
```

Expected: `cannot find type VaultTier` (and others). Good — that's the red phase.

- [ ] **Step 3: Add the closed-set enums before the tests block in `config/mod.rs`**

```rust
use serde::{Deserialize, Serialize};

/// Vault storage tier (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultTier {
    /// Single-user, on-disk SQLite vault. P0 default.
    #[default]
    Local,
    /// Embedded in another process (library mode). P1.
    Embedded,
    /// Federated cloud vault. P2.
    Cloud,
}

/// Ordered steps in the hot-memory assembly recipe (§3.1 hot_memory.recipe).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotMemoryRecipeStep {
    Purpose,
    Index,
    PinnedFeedback,
    TopSalienceProject,
    ActivePlaybook,
    RecentUserSignal,
}

/// Condition that gates an extractor entry in the chain (§5.2.a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractTrigger {
    /// Run this extractor only when the previous one produced confidence < 0.6.
    ConfidenceBelow,
}

/// Which LLM provider backend is active (§4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmProvider {
    /// Any OpenAI-compatible endpoint (Ollama, LM Studio, OpenAI, Azure).
    OpenaiCompatible,
}
```

- [ ] **Step 4: Run tests — expect PASS**

```bash
cargo nextest run -p cairn-core config:: --locked 2>&1 | tail -10
```

Expected: 5 tests PASS (3 new + 2 from Task 1).

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/config/mod.rs
git commit -m "feat(config): closed-set enums with serde (§3.1 #39)"
```

---

## Task 3: String-backed enums (StoreKind, OrchestratorKind, ExtractorWorkerKind)

These three enums serialize as human-readable strings like `"sqlite"` or `"custom:cairn-store-qdrant"`. They use hand-written `Serialize`/`Deserialize` impls.

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`

- [ ] **Step 1: Write failing tests**

Add to the tests block:

```rust
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
```

- [ ] **Step 2: Run — expect compile error**

```bash
cargo check -p cairn-core --locked 2>&1 | grep "^error" | head -5
```

Expected: `cannot find type StoreKind`.

- [ ] **Step 3: Add the string-backed enums to `config/mod.rs`**

Add after the closed-set enums:

```rust
/// Macro: implement string-backed serde for enums with an optional
/// `custom:<payload>` variant. Reduces boilerplate for StoreKind,
/// OrchestratorKind, and ExtractorWorkerKind.
macro_rules! string_enum {
    (
        $(#[$attr:meta])*
        pub enum $name:ident {
            $( $variant:ident => $wire:literal , )*
            Custom,
        }
        unknown_msg: $msg:literal $(,)?
    ) => {
        $(#[$attr])*
        pub enum $name {
            $( $variant, )*
            /// A third-party plugin registered under this contract.
            /// The string after `"custom:"` is the raw plugin name.
            Custom(String),
        }

        impl Default for $name {
            fn default() -> Self {
                // First variant is the default.
                // SAFETY: we always provide at least one non-Custom variant.
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
    pub enum StoreKind {
        Sqlite => "sqlite",
        NexusSandbox => "nexus-sandbox",
        NexusFull => "nexus-full",
        Custom,
    }
    unknown_msg: "expected sqlite | nexus-sandbox | nexus-full | custom:<name>",
}

string_enum! {
    pub enum OrchestratorKind {
        Local => "local",
        Temporal => "temporal",
        Custom,
    }
    unknown_msg: "expected local | temporal | custom:<name>",
}

string_enum! {
    pub enum ExtractorWorkerKind {
        Regex => "regex",
        Llm => "llm",
        Agent => "agent",
        Custom,
    }
    unknown_msg: "expected regex | llm | agent | custom:<name>",
}
```

- [ ] **Step 4: Run tests — expect PASS**

```bash
cargo nextest run -p cairn-core config:: --locked 2>&1 | tail -10
```

Expected: 10 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/config/mod.rs
git commit -m "feat(config): string-backed enums for store/orchestrator/extractor (§4.1 #39)"
```

---

## Task 4: All config structs + Default impls

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`

- [ ] **Step 1: Write failing tests**

Add to the tests block:

```rust
    #[test]
    fn default_config_deserializes_from_empty_yaml() {
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
    fn full_config_yaml_round_trips() {
        // §3.1 config sketch — abbreviated. Tests that every struct accepts its fields.
        let yaml = r#"
vault:
  name: my-vault
  tier: local
  layout:
    sources: inbox
    records: memories
    wiki: notes
    skills: skills
    enabled_kinds:
      - user
      - feedback
    file_naming: "{kind}_{slug}.md"
    index:
      max_lines: 200
      max_bytes: 25600
  hot_memory:
    max_bytes: 25600
    recipe:
      - purpose
      - index
  schema_files:
    - CLAUDE.md
store:
  kind: sqlite
sensors:
  hooks:
    enabled: true
  ide:
    enabled: false
  screen:
    enabled: false
  slack:
    enabled: false
    scope: []
workflows:
  orchestrator: local
pipeline:
  extract:
    chain:
      - worker: regex
        kinds: []
"#;
        let config: CairnConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.vault.name, "my-vault");
        assert_eq!(config.vault.layout.sources, "inbox");
        assert_eq!(config.vault.layout.enabled_kinds.len(), 2);
        assert!(!config.sensors.ide.enabled);
    }
```

- [ ] **Step 2: Run — expect compile error**

```bash
cargo check -p cairn-core --locked 2>&1 | grep "^error" | head -5
```

Expected: `cannot find type CairnConfig`.

- [ ] **Step 3: Add all struct definitions and Default impls to `config/mod.rs`**

Add after the enums and before `#[cfg(test)]`:

```rust
use std::collections::BTreeMap;
use crate::domain::taxonomy::MemoryKind;

// ── Top-level ─────────────────────────────────────────────────────────────

/// Root config type. Deserialized from `.cairn/config.yaml` (brief §3.1).
///
/// All fields default to the P0 offline-local deployment:
/// SQLite store, no LLM, hook + IDE sensors, local tokio orchestrator,
/// regex-only extractor chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CairnConfig {
    pub vault:     VaultConfig,
    pub store:     StoreConfig,
    pub llm:       LlmConfig,
    pub sensors:   SensorsConfig,
    pub workflows: WorkflowsConfig,
    pub pipeline:  PipelineConfig,
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
    pub name:         String,
    pub tier:         VaultTier,
    pub layout:       LayoutConfig,
    pub hot_memory:   HotMemoryConfig,
    /// Glob-keyed retention policies. Value: `"forever"` or `"<N>d"`.
    pub retention:    BTreeMap<String, String>,
    pub schema_files: Vec<String>,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            name:       "my-vault".into(),
            tier:       VaultTier::Local,
            layout:     LayoutConfig::default(),
            hot_memory: HotMemoryConfig::default(),
            retention:  BTreeMap::new(),
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
    pub sources:       String,
    pub records:       String,
    pub wiki:          String,
    pub skills:        String,
    /// Subset of the 19 `MemoryKind`s active for extraction + storage.
    /// Empty means all 19 kinds are enabled (absence = unrestricted).
    pub enabled_kinds: Vec<MemoryKind>,
    pub file_naming:   String,
    pub index:         IndexConfig,
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
    pub max_lines: u32,
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
    pub recipe:    Vec<HotMemoryRecipeStep>,
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
    pub provider: Option<LlmProvider>,
    pub base_url: Option<String>,
    pub model:    Option<String>,
    pub api_key:  Option<String>,
}

// ── Sensors ───────────────────────────────────────────────────────────────

/// Sensor enablement (§3.1 sensors block).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SensorsConfig {
    pub hooks:  SensorToggle,
    pub ide:    SensorToggle,
    pub screen: SensorToggle,
    pub slack:  SlackSensorConfig,
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
#[serde(default)]
pub struct SensorToggle {
    pub enabled: bool,
}

/// Slack sensor configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SlackSensorConfig {
    pub enabled: bool,
    pub scope:   Vec<String>,
}

// ── Workflows ─────────────────────────────────────────────────────────────

/// Workflow orchestrator selection (§4.1, §4.0 row 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowsConfig {
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
    pub worker:  ExtractorWorkerKind,
    /// Kinds this extractor handles. Empty means all kinds.
    pub kinds:   Vec<MemoryKind>,
    pub trigger: Option<ExtractTrigger>,
    pub budget:  ExtractBudget,
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
    pub max_tokens:  Option<u32>,
    pub max_wall_ms: Option<u32>,
    pub max_turns:   Option<u32>,
}

/// Derived capability set, computed from `CairnConfig` (no I/O).
///
/// The verb layer calls `config.capabilities()` before dispatching to
/// gate features that require capabilities that may not be present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySet {
    /// Always true at P0 (FTS5 always present).
    pub keyword_search:  bool,
    /// True iff `llm.provider` is `Some`.
    pub semantic_search: bool,
    /// True iff `semantic_search` (requires vector embeddings).
    pub hybrid_search:   bool,
    /// True iff `llm.provider` is `Some`.
    pub llm_extract:     bool,
    /// True iff the pipeline chain contains an `agent` worker.
    pub agent_extract:   bool,
    /// False for `sqlite` (P0). P1+ stores may advertise this.
    pub graph_edges:     bool,
}
```

Also add `serde_yaml` to the test imports so the `full_config_yaml_round_trips` test compiles. The `serde_yaml` crate is not yet a dep of `cairn-core`. For that test, use `serde_json` instead (re-write the YAML as JSON in the test). Update the test:

```rust
    #[test]
    fn full_config_json_round_trips() {
        // All struct fields populated — verifies field names and types.
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
```

Remove the `full_config_yaml_round_trips` test (replace it with `full_config_json_round_trips`).

- [ ] **Step 4: Run tests — expect PASS**

```bash
cargo nextest run -p cairn-core config:: --locked 2>&1 | tail -15
```

Expected: all tests PASS. If any fail, check serde attribute typos.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/config/mod.rs
git commit -m "feat(config): all config structs + Default impls (§3.1 #39)"
```

---

## Task 5: `validate()` method

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`

- [ ] **Step 1: Write failing tests**

Add to the tests block:

```rust
    #[test]
    fn validate_default_config_ok() {
        CairnConfig::default().validate().unwrap();
    }

    #[test]
    fn validate_rejects_zero_hot_memory_budget() {
        let mut config = CairnConfig::default();
        config.vault.hot_memory.max_bytes = 0;
        let err = config.validate().unwrap_err();
        assert!(matches!(err, ConfigError::InvalidBudget { field: "vault.hot_memory.max_bytes", .. }));
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
        assert!(matches!(err, ConfigError::InvalidPluginName { field: "store.kind", .. }));
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
        config.vault.retention.insert("*/trace.md".into(), "30d".into());
        let err = config.validate().unwrap_err();
        assert!(matches!(err, ConfigError::InvalidRetentionKey(_)));
    }

    #[test]
    fn validate_accepts_retention_key_star_in_filename() {
        let mut config = CairnConfig::default();
        config.vault.retention.insert("raw/trace_*.md".into(), "30d".into());
        config.validate().unwrap();
    }
```

- [ ] **Step 2: Run — expect compile errors (validate undefined)**

```bash
cargo check -p cairn-core --locked 2>&1 | grep "^error" | head -5
```

- [ ] **Step 3: Add `validate()` to `CairnConfig` impl**

Add before `#[cfg(test)]`:

```rust
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
                value: 0,
            });
        }

        // 4. Extractor budget fields must be > 0 when set
        for entry in &self.pipeline.extract.chain {
            let b = &entry.budget;
            if b.max_tokens == Some(0) {
                return Err(ConfigError::InvalidBudget {
                    field: "pipeline.extract.chain[].budget.max_tokens",
                    value: 0,
                });
            }
            if b.max_wall_ms == Some(0) {
                return Err(ConfigError::InvalidBudget {
                    field: "pipeline.extract.chain[].budget.max_wall_ms",
                    value: 0,
                });
            }
            if b.max_turns == Some(0) {
                return Err(ConfigError::InvalidBudget {
                    field: "pipeline.extract.chain[].budget.max_turns",
                    value: 0,
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
}
```

- [ ] **Step 4: Run tests — expect PASS**

```bash
cargo nextest run -p cairn-core config:: --locked 2>&1 | tail -15
```

Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/config/mod.rs
git commit -m "feat(config): validate() typed error checks (§3.1 #39)"
```

---

## Task 6: `capabilities()` method + CapabilitySet

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`

- [ ] **Step 1: Write failing tests**

Add to the tests block:

```rust
    #[test]
    fn capabilities_llm_off_by_default() {
        let caps = CairnConfig::default().capabilities();
        assert!(caps.keyword_search,  "keyword_search always true");
        assert!(!caps.semantic_search, "no LLM → no semantic");
        assert!(!caps.hybrid_search,   "no LLM → no hybrid");
        assert!(!caps.llm_extract,     "no LLM → no llm_extract");
        assert!(!caps.agent_extract,   "default chain has no agent worker");
        assert!(!caps.graph_edges,     "sqlite → no graph edges");
    }

    #[test]
    fn capabilities_llm_on() {
        let mut config = CairnConfig::default();
        config.llm.provider = Some(LlmProvider::OpenaiCompatible);
        let caps = config.capabilities();
        assert!(caps.keyword_search);
        assert!(caps.semantic_search);
        assert!(caps.hybrid_search);
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
        let caps = config.capabilities();
        assert!(caps.agent_extract);
    }
```

- [ ] **Step 2: Run — expect compile error (capabilities undefined)**

```bash
cargo check -p cairn-core --locked 2>&1 | grep "^error" | head -5
```

- [ ] **Step 3: Add `capabilities()` to `CairnConfig` impl (inside the existing `impl CairnConfig` block)**

```rust
    /// Derive the active capability set from this config (pure, no I/O).
    ///
    /// The verb layer uses this to gate features before dispatch.
    #[must_use]
    pub fn capabilities(&self) -> CapabilitySet {
        let llm_on = self.llm.provider.is_some();
        let agent_extract = self
            .pipeline
            .extract
            .chain
            .iter()
            .any(|e| e.worker == ExtractorWorkerKind::Agent);

        CapabilitySet {
            keyword_search:  true,
            semantic_search: llm_on,
            hybrid_search:   llm_on,
            llm_extract:     llm_on,
            agent_extract,
            graph_edges:     false, // P0: sqlite always false; P1+ gates on store capability
        }
    }
```

- [ ] **Step 4: Run tests — expect PASS**

```bash
cargo nextest run -p cairn-core config:: --locked 2>&1 | tail -15
```

Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/config/mod.rs
git commit -m "feat(config): capabilities() + CapabilitySet (§3.1 #39)"
```

---

## Task 7: Snapshot test + proptest round-trip

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`

- [ ] **Step 1: Add the snapshot test**

Add to the tests block:

```rust
    #[test]
    fn default_config_snapshot() {
        let json = serde_json::to_string_pretty(&CairnConfig::default())
            .expect("CairnConfig::default() must be serializable");
        insta::assert_snapshot!(json);
    }
```

Also add to the top of `config/mod.rs` (outside `#[cfg(test)]`):

```rust
// Allow the insta macro in tests — it is a dev-dep only.
#[cfg(test)]
use insta;
```

Wait — `insta` macros don't need an explicit `use`. Remove that line and just use `insta::assert_snapshot!` directly.

- [ ] **Step 2: Add the proptest round-trip**

Add to the tests block (also add `use proptest::prelude::*;` at top of the tests module):

```rust
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn default_config_json_round_trip(_seed in 0u8..1) {
            // Seed is unused; we just test the single default value.
            // A full property test would require Arbitrary impls for all types,
            // which is out of scope for P0. This guards the serde round-trip.
            let original = CairnConfig::default();
            let json = serde_json::to_string(&original).unwrap();
            let restored: CairnConfig = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(original, restored);
        }
    }
```

- [ ] **Step 3: Run snapshot test once to create the snapshot file**

```bash
cargo nextest run -p cairn-core config::tests::default_config_snapshot --locked 2>&1
```

Expected: test fails with "snapshot not found" — that's correct. Then accept it:

```bash
cargo insta review
```

Press `a` to accept the snapshot. A `.snap` file is created in `crates/cairn-core/src/config/snapshots/`.

- [ ] **Step 4: Run all config tests — expect PASS**

```bash
cargo nextest run -p cairn-core config:: --locked 2>&1 | tail -15
```

Expected: all PASS (snapshot now accepted).

- [ ] **Step 5: Commit snapshot + test**

```bash
git add crates/cairn-core/src/config/
git commit -m "test(config): snapshot + proptest round-trip (§3.1 #39)"
```

---

## Task 8: cairn-cli config module + env-var interpolation

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/cairn-cli/Cargo.toml`
- Modify: `crates/cairn-cli/src/lib.rs`
- Create: `crates/cairn-cli/src/config.rs`

- [ ] **Step 1: Add figment, serde_yaml, regex to cairn-cli Cargo.toml**

In `crates/cairn-cli/Cargo.toml`, add to `[dependencies]`:

```toml
figment    = { workspace = true }
regex      = { workspace = true }
serde_yaml = { workspace = true }
```

Add to `[dev-dependencies]`:

```toml
tempfile = { workspace = true }
proptest = { workspace = true }
```

Also remove `figment`, `regex`, `serde_yaml` from the `[package.metadata.cargo-machete] ignored` list if they appear there (they shouldn't, but check).

- [ ] **Step 2: Expose `config` module in cairn-cli lib.rs**

In `crates/cairn-cli/src/lib.rs`, add after `pub mod plugins;`:

```rust
pub mod config;
```

- [ ] **Step 3: Write the failing test for interpolate_env**

Create `crates/cairn-cli/src/config.rs`:

```rust
//! Config loading for the `cairn` binary (brief §3.1, §6.5).
//!
//! The loading stack: compiled defaults → `.cairn/config.yaml` →
//! `CAIRN_*` environment variables → CLI flag overrides.
//!
//! `${VAR}` placeholders in string YAML values are substituted from the
//! process environment before handing bytes to figment. Only
//! `[A-Z_][A-Z0-9_]*` variable names are recognized (matching the design
//! brief's explicit examples: `${OPENAI_API_KEY}`, `${CAIRN_LLM_MODEL}`).

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use cairn_core::config::{CairnConfig, ConfigError};

/// CLI-layer overrides. Sparse at P0 — extended as verbs land.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CliOverrides {
    // Future: store_kind, log_format, vault_path, …
}

fn env_var_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\$\{([A-Z_][A-Z0-9_]*)\}").expect("valid regex")
    })
}

/// Replace every `${VAR}` in `src` with its environment variable value.
///
/// # Errors
/// [`ConfigError::UnresolvedEnvVar`] for the first unset variable found.
pub fn interpolate_env(src: &str) -> Result<String, ConfigError> {
    let re = env_var_re();
    let mut unresolved: Option<String> = None;
    let result = re.replace_all(src, |caps: &regex::Captures<'_>| {
        let name = &caps[1];
        match std::env::var(name) {
            Ok(val) => val,
            Err(_) => {
                if unresolved.is_none() {
                    unresolved = Some(name.to_owned());
                }
                caps[0].to_owned() // leave placeholder in place
            }
        }
    });
    if let Some(name) = unresolved {
        return Err(ConfigError::UnresolvedEnvVar(name));
    }
    Ok(result.into_owned())
}

/// Load and validate config for a vault rooted at `vault_path`.
///
/// Layering order (later layers win):
/// 1. Compiled defaults (`CairnConfig::default()`)
/// 2. `.cairn/config.yaml` (after `${VAR}` interpolation)
/// 3. `CAIRN_*` environment variables (double-underscore for nesting)
/// 4. `cli` overrides
///
/// # Errors
/// Returns an error if the YAML is invalid, a `${VAR}` is unset, or
/// `CairnConfig::validate()` fails.
pub fn load(vault_path: &Path, cli: &CliOverrides) -> Result<CairnConfig> {
    use figment::providers::{Env, Format, Serialized, Yaml};
    use figment::Figment;

    let config_path = vault_path.join(".cairn/config.yaml");

    let yaml_content: String = if config_path.exists() {
        let raw = std::fs::read_to_string(&config_path)
            .with_context(|| format!("reading {}", config_path.display()))?;
        interpolate_env(&raw)
            .map_err(anyhow::Error::from)
            .with_context(|| "resolving ${VAR} placeholders in config")?
    } else {
        String::new()
    };

    let config: CairnConfig = Figment::new()
        .merge(Serialized::defaults(CairnConfig::default()))
        .merge(Yaml::string(&yaml_content))
        .merge(Env::prefixed("CAIRN_").split("__"))
        .merge(Serialized::globals(cli))
        .extract()
        .context("parsing config")?;

    config
        .validate()
        .map_err(anyhow::Error::from)
        .context("validating config")?;

    Ok(config)
}

/// Write the default config to `<vault_path>/.cairn/config.yaml`.
///
/// Fails if the file already exists.
///
/// # Errors
/// Returns an error if the directory cannot be created, the file already
/// exists, or serialization fails.
pub fn write_default(vault_path: &Path) -> Result<()> {
    let config_dir = vault_path.join(".cairn");
    let config_path = config_dir.join("config.yaml");

    anyhow::ensure!(
        !config_path.exists(),
        "{} already exists; delete it first to re-bootstrap",
        config_path.display()
    );

    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("creating {}", config_dir.display()))?;

    let yaml = serde_yaml::to_string(&CairnConfig::default())
        .context("serializing default config to YAML")?;

    std::fs::write(&config_path, yaml)
        .with_context(|| format!("writing {}", config_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_no_vars_unchanged() {
        let input = "vault:\n  name: my-vault\n";
        assert_eq!(interpolate_env(input).unwrap(), input);
    }

    #[test]
    fn interpolate_substitutes_set_var() {
        std::env::set_var("CAIRN_TEST_INTERPOLATE_ABC", "sk-hello");
        let result = interpolate_env("api_key: ${CAIRN_TEST_INTERPOLATE_ABC}").unwrap();
        std::env::remove_var("CAIRN_TEST_INTERPOLATE_ABC");
        assert_eq!(result, "api_key: sk-hello");
    }

    #[test]
    fn interpolate_errors_on_unset_var() {
        std::env::remove_var("CAIRN_TEST_MISSING_XYZ");
        let err = interpolate_env("key: ${CAIRN_TEST_MISSING_XYZ}").unwrap_err();
        assert!(matches!(err, ConfigError::UnresolvedEnvVar(ref v) if v == "CAIRN_TEST_MISSING_XYZ"));
    }

    #[test]
    fn interpolate_ignores_lowercase_placeholder() {
        // Only uppercase+underscore names are recognized; lowercase passes through.
        let input = "note: ${not_a_var}";
        assert_eq!(interpolate_env(input).unwrap(), input);
    }
}
```

- [ ] **Step 4: Run — expect compile errors (figment/regex not yet in deps)**

```bash
cargo check -p cairn-cli --locked 2>&1 | grep "^error" | head -10
```

Expected: missing crate errors. After adding deps (Step 1 already did this), run again:

```bash
cargo check -p cairn-cli --locked 2>&1 | grep "^error" | head -10
```

Expected: clean.

- [ ] **Step 5: Run unit tests in config module**

```bash
cargo nextest run -p cairn-cli cairn_cli::config:: --locked 2>&1 | tail -15
```

Expected: all 4 unit tests PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/cairn-cli/Cargo.toml crates/cairn-cli/src/lib.rs crates/cairn-cli/src/config.rs
git commit -m "feat(config): cairn-cli loader + interpolate_env (§3.1 §6.5 #39)"
```

---

## Task 9: `cairn bootstrap` subcommand

**Files:**
- Modify: `crates/cairn-cli/src/main.rs`

- [ ] **Step 1: Add the bootstrap subcommand builder and handler**

In `crates/cairn-cli/src/main.rs`, add the import at the top (after existing imports):

```rust
use cairn_cli::config as cli_config;
```

Add the subcommand builder function (alongside `plugins_subcommand()`):

```rust
fn bootstrap_subcommand() -> clap::Command {
    clap::Command::new("bootstrap")
        .about("Write a default .cairn/config.yaml to a vault directory")
        .arg(
            clap::Arg::new("vault-path")
                .long("vault-path")
                .default_value(".")
                .value_name("PATH")
                .help("Vault root directory (default: current directory)"),
        )
}
```

Register it in `build_command()`:

```rust
fn build_command() -> clap::Command {
    generated::command()
        .version(env!("CARGO_PKG_VERSION"))
        .about("Cairn — agent memory framework")
        .subcommand(plugins_subcommand())
        .subcommand(bootstrap_subcommand())   // ← add this line
}
```

Add the handler in `main()` inside the `match matches.subcommand()` block, before the catch-all `Some((verb, _))` arm:

```rust
        Some(("bootstrap", sub)) => run_bootstrap(sub),
```

Add the handler function (alongside `run_plugins()`):

```rust
fn run_bootstrap(matches: &ArgMatches) -> ExitCode {
    let vault_path = std::path::PathBuf::from(
        matches
            .get_one::<String>("vault-path")
            .expect("vault-path has a default value"),
    );

    match cli_config::write_default(&vault_path) {
        Ok(()) => {
            println!(
                "cairn bootstrap: wrote default config to {}",
                vault_path.join(".cairn/config.yaml").display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            // EX_CONFIG (78) — bad config or file already exists
            eprintln!("cairn bootstrap: {e:#}");
            ExitCode::from(78)
        }
    }
}
```

- [ ] **Step 2: Build and smoke-test the binary**

```bash
cargo build -p cairn-cli --locked 2>&1 | tail -5
```

Expected: compiles cleanly.

```bash
./target/debug/cairn bootstrap --help
```

Expected: shows `--vault-path` flag description.

```bash
tmp=$(mktemp -d) && ./target/debug/cairn bootstrap --vault-path "$tmp" && cat "$tmp/.cairn/config.yaml" | head -10
```

Expected: prints "cairn bootstrap: wrote default config to …" and the file contains `vault:` YAML.

- [ ] **Step 3: Verify double-bootstrap fails cleanly**

```bash
./target/debug/cairn bootstrap --vault-path "$tmp" ; echo "exit: $?"
```

Expected: prints "cairn bootstrap: … already exists …" and exits with code 78.

- [ ] **Step 4: Run full check**

```bash
cargo check --workspace --all-targets --locked 2>&1 | grep "^error" | head -10
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/main.rs
git commit -m "feat(cli): cairn bootstrap subcommand (§3.1 acceptance criterion #39)"
```

---

## Task 10: Integration tests

**Files:**
- Create: `crates/cairn-cli/tests/config.rs`

- [ ] **Step 1: Create the integration test file**

```rust
//! Integration tests for the cairn-cli config loader (brief §3.1, §6.5).

use cairn_cli::config::{load, write_default, CliOverrides};
use cairn_core::config::{CairnConfig, StoreKind};

fn write_yaml(vault: &std::path::Path, content: &str) {
    let dir = vault.join(".cairn");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.yaml"), content).unwrap();
}

// ── Loader ────────────────────────────────────────────────────────────────

#[test]
fn absent_config_file_gives_default() {
    let dir = tempfile::tempdir().unwrap();
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    assert_eq!(config, CairnConfig::default());
}

#[test]
fn load_from_file_overrides_name() {
    let dir = tempfile::tempdir().unwrap();
    write_yaml(dir.path(), "vault:\n  name: test-vault\n");
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    assert_eq!(config.vault.name, "test-vault");
    // Unset fields stay at default
    assert_eq!(config.store.kind, StoreKind::Sqlite);
}

#[test]
fn env_var_interpolation_sets_api_key() {
    let dir = tempfile::tempdir().unwrap();
    write_yaml(
        dir.path(),
        "llm:\n  provider: openai-compatible\n  api_key: ${CAIRN_IT_API_KEY_TEST}\n",
    );
    std::env::set_var("CAIRN_IT_API_KEY_TEST", "sk-integration-test");
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    std::env::remove_var("CAIRN_IT_API_KEY_TEST");
    assert_eq!(config.llm.api_key, Some("sk-integration-test".into()));
}

#[test]
fn missing_env_var_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    std::env::remove_var("CAIRN_IT_MISSING_VAR_TEST");
    write_yaml(dir.path(), "llm:\n  api_key: ${CAIRN_IT_MISSING_VAR_TEST}\n");
    let err = load(dir.path(), &CliOverrides::default()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("CAIRN_IT_MISSING_VAR_TEST"),
        "error should name the unresolved var: {msg}"
    );
}

#[test]
fn cairn_env_override_wins_over_file() {
    let dir = tempfile::tempdir().unwrap();
    write_yaml(dir.path(), "store:\n  kind: nexus-sandbox\n");
    // Env var overrides the file value
    std::env::set_var("CAIRN_STORE__KIND", "sqlite");
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    std::env::remove_var("CAIRN_STORE__KIND");
    assert_eq!(config.store.kind, StoreKind::Sqlite);
}

#[test]
fn invalid_config_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    // zero budget is invalid
    write_yaml(
        dir.path(),
        "vault:\n  hot_memory:\n    max_bytes: 0\n",
    );
    let err = load(dir.path(), &CliOverrides::default()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("max_bytes"), "error should mention the bad field: {msg}");
}

// ── Bootstrap ─────────────────────────────────────────────────────────────

#[test]
fn bootstrap_writes_config_file() {
    let dir = tempfile::tempdir().unwrap();
    write_default(dir.path()).unwrap();
    assert!(dir.path().join(".cairn/config.yaml").exists());
}

#[test]
fn bootstrap_round_trips_to_default() {
    let dir = tempfile::tempdir().unwrap();
    write_default(dir.path()).unwrap();
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    assert_eq!(config, CairnConfig::default());
}

#[test]
fn bootstrap_fails_if_file_already_exists() {
    let dir = tempfile::tempdir().unwrap();
    write_yaml(dir.path(), "vault:\n  name: existing\n");
    let err = write_default(dir.path()).unwrap_err();
    assert!(
        format!("{err}").contains("already exists"),
        "should describe the conflict: {err}"
    );
}
```

- [ ] **Step 2: Run the integration tests**

```bash
cargo nextest run -p cairn-cli --test config --locked 2>&1 | tail -20
```

Expected: all 9 tests PASS. If `env_override_wins` or interpolation tests fail intermittently (env var pollution from parallel tests), double check that each test uses a unique env var name — they do in the code above.

- [ ] **Step 3: Run the full verification checklist**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
```

Expected: all pass. Fix any clippy lints before committing (common ones: `clippy::must_use_candidate` on `CapabilitySet`, `clippy::module_name_repetitions`).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/tests/config.rs
git commit -m "test(config): integration tests for loader + bootstrap (#39)"
```

---

## Task 11: Close out

- [ ] **Step 1: Run supply-chain checks**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Fix any issues found. Common: `cargo machete` may flag `serde_yaml` as unused if it only appears in `write_default`. Suppress with `[package.metadata.cargo-machete] ignored = ["serde_yaml"]` in `cairn-cli/Cargo.toml` if needed — but only after confirming it IS used.

- [ ] **Step 2: Run docs check**

```bash
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked 2>&1 | grep "^error" | head -10
```

Fix any doc link errors.

- [ ] **Step 3: Update issue checklist**

The following acceptance criteria are now satisfied:
- ✅ `cairn bootstrap` or config loader can produce a valid default config
- ✅ Invalid plugin names, unsupported capabilities, and impossible budgets fail with typed errors
- ✅ Disabling local embeddings correctly removes semantic/hybrid capabilities and leaves keyword search available

- [ ] **Step 4: Final commit if any fixes were needed**

```bash
git add -p  # stage only the relevant fixes
git commit -m "chore(config): supply-chain + doc fixes (#39)"
```

---

## Appendix: Quick reference

**Run all config tests:**
```bash
cargo nextest run --workspace -E 'test(config)' --locked
```

**Accept a new insta snapshot:**
```bash
cargo insta review
```

**Check core boundary (must pass before every PR):**
```bash
./scripts/check-core-boundary.sh
```

**Verify capabilities derive correctly from a custom config:**
```rust
let mut config = CairnConfig::default();
config.llm.provider = Some(LlmProvider::OpenaiCompatible);
let caps = config.capabilities();
// caps.semantic_search == true, caps.hybrid_search == true
```
