# Config Schema Design — Issue #39

**Date:** 2026-04-26  
**Issue:** [#39 — Define config schema for vault, store, pipeline, sensors, and budgets](https://github.com/windoliver/cairn/issues/39)  
**Brief sections:** §3.1 Config template · §4.1 Config selects implementation · §5.2.a Extractor config  
**Status:** Approved

---

## 1. Scope

Define typed config structs and validation for vault paths, active store, local search settings,
extractor chain, hot-memory budget, sensor enablement, and workflow cadence. Produce a working
`cairn bootstrap` command that writes a valid default config. Out of scope: plugin implementation
loading and runtime command handling.

---

## 2. Architecture

Two crates each own a distinct layer.

| Layer | Crate | Responsibility |
|---|---|---|
| Types + validation | `cairn-core::config` | `CairnConfig` + section structs + `ConfigError` + `validate()` + `capabilities()`. Pure `serde` data, no I/O. |
| Loading | `cairn-cli::config` | figment stack (defaults → YAML file → `CAIRN_*` env → CLI overrides). `${VAR}` pre-processor. Returns `anyhow::Result<CairnConfig>`. |

`cairn-core::config` is exported to the workspace. `cairn-cli::config` is CLI-only. This preserves
the brief invariant: core has zero I/O deps.

**New workspace dependencies:**
- `figment` (with `yaml` feature) — added to `cairn-cli` only
- No new dep in `cairn-core` (serde already present)

---

## 3. Config types (`cairn-core::config`)

### 3.1 Top-level

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CairnConfig {
    pub vault:     VaultConfig,
    pub store:     StoreConfig,
    pub llm:       LlmConfig,
    pub sensors:   SensorsConfig,
    pub workflows: WorkflowsConfig,
    pub pipeline:  PipelineConfig,
}
```

All fields carry `#[serde(default)]` so an empty YAML file produces the P0 offline-local default.

### 3.2 Vault section

```rust
pub struct VaultConfig {
    pub name:         String,           // default "my-vault"
    pub tier:         VaultTier,        // Local | Embedded | Cloud
    pub layout:       LayoutConfig,
    pub hot_memory:   HotMemoryConfig,
    pub retention:    BTreeMap<String, RetentionPolicy>,
    pub schema_files: Vec<String>,      // default ["CLAUDE.md","AGENTS.md","GEMINI.md"]
}

pub struct LayoutConfig {
    pub sources:       String,          // default "sources"
    pub records:       String,          // default "raw"
    pub wiki:          String,          // default "wiki"
    pub skills:        String,          // default "skills"
    pub enabled_kinds: Vec<MemoryKind>, // empty = all 19 kinds (semantics: absence means unrestricted)
    pub file_naming:   String,          // default "{kind}_{slug}.md"
    pub index:         IndexConfig,
}

pub struct IndexConfig {
    pub max_lines: u32,   // default 200
    pub max_bytes: u32,   // default 25600
}

pub struct HotMemoryConfig {
    pub recipe:    Vec<HotMemoryRecipeStep>,
    pub max_bytes: u32,   // default 25600; validate > 0
}
```

`VaultTier` and `HotMemoryRecipeStep` are closed-set enums derived from §3.1.

### 3.3 Store section

```rust
pub struct StoreConfig {
    pub kind: StoreKind,
}

pub enum StoreKind {
    Sqlite,
    NexusSandbox,
    NexusFull,
    Custom(PluginName),   // reuses existing PluginName newtype for grammar validation
}
```

Default: `StoreKind::Sqlite`.

### 3.4 LLM section

```rust
pub struct LlmConfig {
    pub provider: Option<LlmProvider>,  // None = fail closed (CapabilityUnavailable)
    pub base_url: Option<String>,
    pub model:    Option<String>,       // supports ${VAR} interpolation
    pub api_key:  Option<String>,       // supports ${VAR} interpolation
}
```

P0 compiled default: `provider: None`. LLM-dependent verbs fail closed with
`CapabilityUnavailable { code: "llm.not_configured" }` until an operator configures a provider
(ADR 0001).

### 3.5 Sensors section

```rust
pub struct SensorsConfig {
    pub hooks:  SensorToggle,       // { enabled: bool }  default: true
    pub ide:    SensorToggle,       // default: true
    pub screen: SensorToggle,       // default: false
    pub slack:  SlackSensorConfig,  // { enabled: bool, scope: Vec<String> }  default: false
}
```

### 3.6 Workflows section

```rust
pub struct WorkflowsConfig {
    pub orchestrator: OrchestratorKind,
}

pub enum OrchestratorKind {
    Local,
    Temporal,
    Custom(PluginName),
}
```

Default: `OrchestratorKind::Local` (in-process tokio + SQLite job table, §4.0).

### 3.7 Pipeline section (§5.2.a)

```rust
pub struct PipelineConfig {
    pub extract: ExtractConfig,
}

pub struct ExtractConfig {
    pub chain: Vec<ExtractorEntry>,
}

pub struct ExtractorEntry {
    pub worker:  ExtractorWorkerKind,   // Regex | Llm | Agent | Custom(PluginName)
    pub kinds:   Vec<MemoryKind>,       // empty = all kinds
    pub trigger: Option<ExtractTrigger>,
    pub budget:  ExtractBudget,
}

pub struct ExtractBudget {
    pub max_tokens:  Option<u32>,
    pub max_wall_ms: Option<u32>,
    pub max_turns:   Option<u32>,
}
```

Default chain: `[regex(all kinds)]` — no `llm` entry in the compiled default because the P0
default has no `llm.provider`. If an operator adds an `llm` worker to the chain without
configuring `llm.provider`, `validate()` returns `ConfigError::LlmExtractorWithoutProvider`.

---

## 4. Validation (`CairnConfig::validate`)

Pure method, returns `Result<(), ConfigError>`. Called by the loader before returning to any caller.

| Check | Error variant |
|---|---|
| `StoreKind::Custom` or `OrchestratorKind::Custom` name grammar | `InvalidPluginName { field, source }` |
| `hot_memory.max_bytes == 0` or extractor budget field `== 0` | `InvalidBudget { field, value }` |
| Retention key contains null byte or `*` in non-filename position | `InvalidRetentionKey(String)` |
| Pipeline chain has `llm` worker but `llm.provider` is None | `LlmExtractorWithoutProvider` |
| `${VAR}` in string value but env var unset | `UnresolvedEnvVar(String)` |

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("invalid plugin name for {field}: {source}")]
    InvalidPluginName { field: &'static str, #[source] source: PluginError },
    #[error("invalid budget for {field}: value {value} must be > 0")]
    InvalidBudget { field: &'static str, value: u64 },
    #[error("invalid retention key pattern: {0}")]
    InvalidRetentionKey(String),
    #[error("pipeline chain has llm worker but llm.provider is not configured")]
    LlmExtractorWithoutProvider,
    #[error("unresolved env var in config: ${{{0}}}")]
    UnresolvedEnvVar(String),
}
```

---

## 5. Capability derivation (`CairnConfig::capabilities`)

Pure, no I/O. Used by the verb layer to gate features before dispatch.

```rust
pub struct CapabilitySet {
    pub keyword_search:  bool,  // always true (FTS5 always present at P0)
    pub semantic_search: bool,  // true iff llm.provider is Some
    pub hybrid_search:   bool,  // true iff semantic_search
    pub llm_extract:     bool,  // true iff llm.provider is Some
    pub agent_extract:   bool,  // true iff pipeline chain has agent worker
    pub graph_edges:     bool,  // false for Sqlite (P0)
}
```

Disabling local embeddings (`llm.provider: ~`) → `semantic_search: false`, `hybrid_search: false`,
`keyword_search: true`. This satisfies the acceptance criterion.

---

## 6. Loading (`cairn-cli::config`)

### 6.1 Public API

```rust
pub fn load(vault_path: &Path, cli: &CliOverrides) -> anyhow::Result<CairnConfig>
```

### 6.2 Five-step figment stack

1. **Defaults** — `Figment::new().merge(Serialized::defaults(CairnConfig::default()))`
2. **YAML file** — read `<vault_path>/.cairn/config.yaml`; if absent, skip (default config is valid)
3. **Env-var interpolation** — regex pass on raw YAML bytes before figment parsing:
   - Pattern: `\$\{([A-Z_][A-Z0-9_]*)\}`
   - Replace with `std::env::var(name)` → error `ConfigError::UnresolvedEnvVar` if unset
   - Only applies to string-typed values; numeric/boolean YAML values are not interpolated
4. **`CAIRN_*` env overrides** — `Env::prefixed("CAIRN_").split("__")` (double-underscore for nesting, e.g. `CAIRN_STORE__KIND=sqlite`)
5. **CLI overrides** — `Serialized::globals(cli)` on top

Then call `config.validate()` and return.

### 6.3 `cairn bootstrap`

New subcommand in `cairn-cli`. Calls `load()` with empty `CliOverrides`. Serializes the resulting
`CairnConfig` back to YAML and writes to `<vault_path>/.cairn/config.yaml` if the file does not
yet exist (fails with an informative message if it already exists). Exits `EX_CONFIG (78)` on any
`ConfigError`.

---

## 7. Testing

### 7.1 Unit tests in `cairn-core` (`config/mod.rs`)

| Test | What it checks |
|---|---|
| `valid_minimal_config` | Empty YAML deserializes to valid default |
| `valid_full_config` | §3.1 sketch YAML deserializes to correct field values |
| `invalid_plugin_name` | `store.kind: custom:BAD NAME` → `InvalidPluginName` |
| `invalid_budget_zero` | `hot_memory.max_bytes: 0` → `InvalidBudget` |
| `llm_extractor_without_provider` | Chain has `llm` worker, no provider → `LlmExtractorWithoutProvider` |
| `capabilities_llm_off` | No provider → `semantic_search: false`, `keyword_search: true` |
| `capabilities_llm_on` | Provider set → `semantic_search: true`, `hybrid_search: true` |

### 7.2 Snapshot tests in `cairn-core` (`insta`)

- `default_config_snapshot` — `serde_json::to_string_pretty(&CairnConfig::default())` committed as `.snap`. Catches silent default drift.

### 7.3 Integration tests in `cairn-cli` (`tests/config.rs`)

| Test | What it checks |
|---|---|
| `load_from_file` | Minimal YAML in tempdir, `load()` returns correct fields |
| `env_override_wins` | `CAIRN_STORE__KIND=sqlite` in env beats file value |
| `env_var_interpolation` | YAML `api_key: ${TEST_KEY}`, env var set → substituted value |
| `missing_env_var_fails` | YAML `${MISSING}` → `UnresolvedEnvVar` error |
| `bootstrap_writes_default` | Bootstrap on tempdir → `.cairn/config.yaml` created, round-trips to `default()` |

### 7.4 Property test (`proptest`)

- Round-trip: `CairnConfig → serde_json → CairnConfig` on generated configs — validates serde symmetry.

---

## 8. Acceptance criteria mapping

| Criterion | Design element |
|---|---|
| `cairn bootstrap` produces valid default config | §6.3 bootstrap command + `load()` with empty overrides |
| Invalid plugin names fail with typed errors | `validate()` → `ConfigError::InvalidPluginName` |
| Unsupported capabilities fail with typed errors | `validate()` → `ConfigError::LlmExtractorWithoutProvider` |
| Impossible budgets fail with typed errors | `validate()` → `ConfigError::InvalidBudget` |
| Disabling embeddings removes semantic/hybrid, leaves keyword | `capabilities()` → `CapabilitySet` |

---

## 9. Out of scope

- Plugin implementation loading (runtime `dlopen` / registry wiring)
- Runtime command handling beyond `cairn bootstrap`
- P1+ store kinds (NexusSandbox, NexusFull) — config types are defined but validation that the store is actually reachable is deferred
