# Hot Memory Assembler Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement issue #80 as a full vertical slice: hot-memory assembly, source ordering, byte budgeting, metadata, SQLite cache/invalidation, centrality ranking, and CLI response wiring.

**Architecture:** `cairn-core` owns pure hot-memory types and assembly. `MemoryStore` exposes the exact read/cache surface the verb needs. `cairn-store-sqlite` owns SQLite schema, vault-file reads, record/edge queries, centrality, and cache rows. `cairn-cli` remains thin: load config from the current vault, open the store, call the shared path, and render the generated response envelope.

**Tech Stack:** Rust 1.95, serde/serde_json, thiserror, async-trait, rusqlite bundled, sha2, clap-generated CLI, cairn-codegen, insta/nextest integration tests.

---

## File Structure

- Create `crates/cairn-core/src/hot_memory.rs`: pure input/output structs, ranking, budget enforcement, truncation metadata, source fingerprints, and cache key helpers.
- Modify `crates/cairn-core/src/lib.rs`: export `hot_memory`.
- Modify `crates/cairn-core/src/config/mod.rs`: add `vault.hot_memory.god_node_weight` with validation.
- Modify `crates/cairn-core/src/contract/memory_store.rs`: add hot-memory request/cache methods and typed store errors.
- Modify `crates/cairn-idl/schema/verbs/assemble_hot.json`: extend response data metadata.
- Regenerate `crates/cairn-core/src/generated/**`, `crates/cairn-cli/src/generated/**`, `crates/cairn-mcp/src/generated/**`, and `skills/cairn/**`.
- Modify `crates/cairn-store-sqlite/Cargo.toml`: enable rusqlite/serde_json/sha2 dependencies.
- Modify `crates/cairn-store-sqlite/src/lib.rs`: implement `SqliteMemoryStore` connection, schema, source reads, centrality, cache, invalidation, and test helpers.
- Create `crates/cairn-store-sqlite/tests/hot_memory.rs`: SQLite integration tests.
- Modify `crates/cairn-cli/Cargo.toml`: ensure `cairn-store-sqlite` is a normal dependency if it is not already present.
- Modify `crates/cairn-cli/src/verbs/assemble_hot.rs`: wire the real verb path.
- Modify `crates/cairn-cli/tests/envelope_tests.rs`: stop expecting `assemble_hot` to abort.
- Create `crates/cairn-cli/tests/assemble_hot.rs`: CLI JSON/human/budget tests.
- Update snapshots under `crates/cairn-core/src/config/snapshots/`, `crates/cairn-idl/tests/snapshots/`, `crates/cairn-cli/tests/snapshots/`, and `crates/cairn-test-fixtures/tests/snapshots/` when intentional.

---

### Task 1: Config And IDL Shape

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`
- Modify: `crates/cairn-idl/schema/verbs/assemble_hot.json`
- Modify after codegen: generated files under `crates/cairn-core/src/generated/`, `crates/cairn-cli/src/generated/`, `crates/cairn-mcp/src/generated/`, `skills/cairn/`
- Test: `crates/cairn-core/src/config/mod.rs`
- Test: `crates/cairn-idl/tests/codegen_snapshot.rs`

- [ ] **Step 1: Write the failing config tests**

Add these tests in the existing `#[cfg(test)] mod tests` in `crates/cairn-core/src/config/mod.rs`:

```rust
#[test]
fn default_hot_memory_god_node_weight_is_point_three() {
    assert_eq!(CairnConfig::default().vault.hot_memory.god_node_weight, 0.3);
}

#[test]
fn validate_rejects_hot_memory_god_node_weight_above_one() {
    let mut config = CairnConfig::default();
    config.vault.hot_memory.god_node_weight = 1.01;
    let err = config.validate().unwrap_err();
    assert!(matches!(
        err,
        ConfigError::InvalidWeight {
            field: "vault.hot_memory.god_node_weight",
            ..
        }
    ));
}

#[test]
fn validate_rejects_hot_memory_god_node_weight_below_zero() {
    let mut config = CairnConfig::default();
    config.vault.hot_memory.god_node_weight = -0.01;
    let err = config.validate().unwrap_err();
    assert!(matches!(
        err,
        ConfigError::InvalidWeight {
            field: "vault.hot_memory.god_node_weight",
            ..
        }
    ));
}
```

- [ ] **Step 2: Run config tests to verify they fail**

Run:

```bash
cargo test -p cairn-core config::tests::default_hot_memory_god_node_weight_is_point_three config::tests::validate_rejects_hot_memory_god_node_weight_above_one config::tests::validate_rejects_hot_memory_god_node_weight_below_zero --locked
```

Expected: compile failure mentioning missing field `god_node_weight` and missing variant `InvalidWeight`.

- [ ] **Step 3: Implement the config field and validation**

Add the error variant near `InvalidBudget`:

```rust
/// A floating-point weight was outside its closed range.
#[error("invalid weight for {field}: value {value} must be in [{min}, {max}]")]
InvalidWeight {
    /// The config field name containing the invalid weight.
    field: &'static str,
    /// The invalid weight value.
    value: f32,
    /// Inclusive minimum.
    min: f32,
    /// Inclusive maximum.
    max: f32,
},
```

Add the field to `HotMemoryConfig`:

```rust
/// Blend weight for entity graph degree centrality in hot-memory ranking.
pub god_node_weight: f32,
```

Set the default:

```rust
god_node_weight: 0.3,
```

Add validation after the `max_bytes` check:

```rust
if !(0.0..=1.0).contains(&self.vault.hot_memory.god_node_weight)
    || self.vault.hot_memory.god_node_weight.is_nan()
{
    return Err(ConfigError::InvalidWeight {
        field: "vault.hot_memory.god_node_weight",
        value: self.vault.hot_memory.god_node_weight,
        min: 0.0,
        max: 1.0,
    });
}
```

- [ ] **Step 4: Extend the assemble_hot response schema**

Replace `$defs.Data` in `crates/cairn-idl/schema/verbs/assemble_hot.json` with this object and sibling defs:

```json
"HotSourceKind": {
  "type": "string",
  "enum": [
    "purpose",
    "profile",
    "pinned",
    "high_salience",
    "project_state",
    "rolling_summary",
    "playbook",
    "recent_user_signal"
  ]
},
"HotCacheStatus": {
  "type": "string",
  "enum": ["hit", "miss", "refreshed"]
},
"SourceSummary": {
  "type": "object",
  "additionalProperties": false,
  "required": ["kind", "attempted", "included", "omitted", "bytes"],
  "properties": {
    "kind": { "$ref": "#/$defs/HotSourceKind" },
    "attempted": { "type": "integer", "minimum": 0 },
    "included": { "type": "integer", "minimum": 0 },
    "omitted": { "type": "integer", "minimum": 0 },
    "bytes": { "type": "integer", "minimum": 0, "maximum": 4194304 }
  }
},
"TruncationDecision": {
  "type": "object",
  "additionalProperties": false,
  "required": ["kind", "reason", "attempted_bytes", "included_bytes"],
  "properties": {
    "kind": { "$ref": "#/$defs/HotSourceKind" },
    "record_id": { "$ref": "../common/primitives.json#/$defs/Ulid" },
    "reason": { "type": "string", "enum": ["budget_exhausted", "section_truncated", "record_omitted"] },
    "attempted_bytes": { "type": "integer", "minimum": 0, "maximum": 4194304 },
    "included_bytes": { "type": "integer", "minimum": 0, "maximum": 4194304 }
  }
},
"CacheInfo": {
  "type": "object",
  "additionalProperties": false,
  "required": ["status", "key"],
  "properties": {
    "status": { "$ref": "#/$defs/HotCacheStatus" },
    "key": { "type": "string", "minLength": 1 }
  }
},
"Data": {
  "type": "object",
  "additionalProperties": false,
  "required": ["prefix", "bytes", "sources", "truncation", "cache"],
  "properties": {
    "prefix": { "type": "string", "description": "Assembled hot-memory text ready to inject into the agent prompt. May be empty when no hot-memory is available." },
    "bytes": { "type": "integer", "minimum": 0, "maximum": 4194304 },
    "sources": {
      "type": "array",
      "items": { "$ref": "#/$defs/SourceSummary" }
    },
    "truncation": {
      "type": "array",
      "items": { "$ref": "#/$defs/TruncationDecision" }
    },
    "cache": { "$ref": "#/$defs/CacheInfo" }
  }
}
```

- [ ] **Step 5: Run tests to verify schema/codegen drift fails before regeneration**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: FAIL listing generated files that differ.

- [ ] **Step 6: Regenerate codegen artifacts**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

Expected: generated files are rewritten.

- [ ] **Step 7: Verify config and codegen tests pass**

Run:

```bash
cargo test -p cairn-core config::tests --locked
cargo nextest run -p cairn-idl --locked
```

Expected: both commands pass, except intentional insta snapshot updates may require review.

- [ ] **Step 8: Review and accept intentional snapshots**

Run when insta reports pending snapshots:

```bash
cargo insta review
```

Expected: accept only snapshots showing `god_node_weight` and assemble_hot metadata fields.

- [ ] **Step 9: Commit Task 1**

Run:

```bash
git add crates/cairn-core/src/config/mod.rs crates/cairn-idl/schema/verbs/assemble_hot.json crates/cairn-core/src/generated crates/cairn-cli/src/generated crates/cairn-mcp/src/generated skills/cairn crates/cairn-core/src/config/snapshots crates/cairn-idl/tests/snapshots
git commit -m "feat(config): add hot memory centrality weight"
```

---

### Task 2: Pure Hot Memory Assembly In Core

**Files:**
- Create: `crates/cairn-core/src/hot_memory.rs`
- Modify: `crates/cairn-core/src/lib.rs`
- Test: `crates/cairn-core/src/hot_memory.rs`

- [ ] **Step 1: Write failing core tests**

Create `crates/cairn-core/src/hot_memory.rs` with only the tests module and enough imports for compile errors to point at missing types:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn source(kind: HotMemorySourceKind, id: &str, body: &str, rank: f32) -> HotMemorySource {
        HotMemorySource {
            kind,
            record_id: Some(id.to_owned()),
            title: Some(id.to_owned()),
            body: body.to_owned(),
            salience: rank,
            evidence_score: rank,
            centrality_score: 0.0,
            updated_at: "2026-05-12T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn assembles_sources_in_design_order() {
        let input = HotMemoryInput {
            sources: vec![
                source(HotMemorySourceKind::Playbook, "01J0000000000000000000004", "playbook", 0.7),
                source(HotMemorySourceKind::Purpose, "01J0000000000000000000001", "purpose", 0.1),
                source(HotMemorySourceKind::Profile, "01J0000000000000000000002", "profile", 0.1),
                source(HotMemorySourceKind::Pinned, "01J0000000000000000000003", "pinned", 0.9),
            ],
            source_revision: "rev-a".to_owned(),
        };
        let out = assemble_hot_memory(&input, HotMemoryOptions { budget_bytes: 4096, god_node_weight: 0.3, cache: HotMemoryCacheInfo::miss("key-a") });
        assert!(out.prefix.find("purpose").unwrap() < out.prefix.find("profile").unwrap());
        assert!(out.prefix.find("profile").unwrap() < out.prefix.find("pinned").unwrap());
        assert!(out.prefix.find("pinned").unwrap() < out.prefix.find("playbook").unwrap());
        assert_eq!(out.bytes as usize, out.prefix.len());
    }

    #[test]
    fn truncates_on_utf8_boundary_and_reports_decision() {
        let input = HotMemoryInput {
            sources: vec![source(HotMemorySourceKind::Purpose, "01J0000000000000000000001", "alpha em dash beta", 1.0)],
            source_revision: "rev-b".to_owned(),
        };
        let out = assemble_hot_memory(&input, HotMemoryOptions { budget_bytes: 12, god_node_weight: 0.0, cache: HotMemoryCacheInfo::miss("key-b") });
        assert!(out.prefix.is_char_boundary(out.prefix.len()));
        assert!(out.bytes <= 12);
        assert_eq!(out.truncation[0].kind, HotMemorySourceKind::Purpose);
        assert_eq!(out.truncation[0].reason, HotMemoryTruncationReason::SectionTruncated);
    }

    #[test]
    fn centrality_weight_changes_order_within_section() {
        let input = HotMemoryInput {
            sources: vec![
                HotMemorySource { centrality_score: 1.0, ..source(HotMemorySourceKind::HighSalience, "01J0000000000000000000001", "central", 0.1) },
                HotMemorySource { centrality_score: 0.0, ..source(HotMemorySourceKind::HighSalience, "01J0000000000000000000002", "evidence", 0.9) },
            ],
            source_revision: "rev-c".to_owned(),
        };
        let out = assemble_hot_memory(&input, HotMemoryOptions { budget_bytes: 4096, god_node_weight: 0.7, cache: HotMemoryCacheInfo::miss("key-c") });
        assert!(out.prefix.find("central").unwrap() < out.prefix.find("evidence").unwrap());
    }
}
```

Add `pub mod hot_memory;` to `crates/cairn-core/src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p cairn-core hot_memory --locked
```

Expected: compile failure for missing `HotMemorySourceKind`, `HotMemorySource`, `HotMemoryInput`, `HotMemoryOptions`, `HotMemoryCacheInfo`, `HotMemoryTruncationReason`, and `assemble_hot_memory`.

- [ ] **Step 3: Implement the pure core module**

Replace `crates/cairn-core/src/hot_memory.rs` with production code before the test module. Include these public types:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotMemorySourceKind {
    Purpose,
    Profile,
    Pinned,
    HighSalience,
    ProjectState,
    RollingSummary,
    Playbook,
    RecentUserSignal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotMemoryTruncationReason {
    BudgetExhausted,
    SectionTruncated,
    RecordOmitted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotMemorySource {
    pub kind: HotMemorySourceKind,
    pub record_id: Option<String>,
    pub title: Option<String>,
    pub body: String,
    pub salience: f32,
    pub evidence_score: f32,
    pub centrality_score: f32,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotMemoryInput {
    pub sources: Vec<HotMemorySource>,
    pub source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotMemoryOptions {
    pub budget_bytes: u32,
    pub god_node_weight: f32,
    pub cache: HotMemoryCacheInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HotMemoryCacheStatus {
    Hit,
    Miss,
    Refreshed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotMemoryCacheInfo {
    pub status: HotMemoryCacheStatus,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotMemorySourceSummary {
    pub kind: HotMemorySourceKind,
    pub attempted: u32,
    pub included: u32,
    pub omitted: u32,
    pub bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotMemoryTruncation {
    pub kind: HotMemorySourceKind,
    pub record_id: Option<String>,
    pub reason: HotMemoryTruncationReason,
    pub attempted_bytes: u32,
    pub included_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotMemoryOutput {
    pub prefix: String,
    pub bytes: u32,
    pub sources: Vec<HotMemorySourceSummary>,
    pub truncation: Vec<HotMemoryTruncation>,
    pub cache: HotMemoryCacheInfo,
}
```

Implement helpers with these signatures:

```rust
impl HotMemoryCacheInfo {
    #[must_use]
    pub fn hit(key: impl Into<String>) -> Self { Self { status: HotMemoryCacheStatus::Hit, key: key.into() } }
    #[must_use]
    pub fn miss(key: impl Into<String>) -> Self { Self { status: HotMemoryCacheStatus::Miss, key: key.into() } }
    #[must_use]
    pub fn refreshed(key: impl Into<String>) -> Self { Self { status: HotMemoryCacheStatus::Refreshed, key: key.into() } }
}

#[must_use]
pub fn assemble_hot_memory(input: &HotMemoryInput, options: HotMemoryOptions) -> HotMemoryOutput {
    // Implementation details:
    // - group by HotMemorySourceKind using the fixed kind_order() list
    // - sort each group by rank descending, salience descending, evidence_score descending, updated_at descending, record_id ascending
    // - render each source as "## <kind>\n<title line when present>\n<body>\n\n"
    // - append only when the next text fits remaining budget
    // - truncate only Purpose/Profile text blocks when needed and record SectionTruncated
    // - mark lower-priority omitted records as RecordOmitted or BudgetExhausted
    // - populate a HotMemorySourceSummary for every kind in kind_order()
}
```

The implementation must use a `truncate_utf8(input: &str, max_bytes: usize) -> &str` helper that moves backward until `input.is_char_boundary(idx)` is true.

- [ ] **Step 4: Run core tests to verify green**

Run:

```bash
cargo test -p cairn-core hot_memory --locked
```

Expected: PASS.

- [ ] **Step 5: Commit Task 2**

Run:

```bash
git add crates/cairn-core/src/lib.rs crates/cairn-core/src/hot_memory.rs
git commit -m "feat(core): add pure hot memory assembler"
```

---

### Task 3: MemoryStore Hot-Memory Contract

**Files:**
- Modify: `crates/cairn-core/src/contract/memory_store.rs`
- Test: `crates/cairn-core/src/contract/memory_store.rs`

- [ ] **Step 1: Write failing contract test**

Add to the existing tests in `memory_store.rs`:

```rust
#[tokio::test]
async fn dyn_store_supports_hot_memory_methods() {
    let s: Box<dyn MemoryStore> = Box::new(StubStore);
    let request = HotMemoryRequest {
        session_id: Some("session-a".to_owned()),
        agent_id: Some("agent-a".to_owned()),
        budget_bytes: 1024,
        config_fingerprint: "config-a".to_owned(),
        god_node_weight: 0.3,
    };
    let input = s.hot_memory_input(&request).await.expect("hot memory input");
    assert_eq!(input.source_revision, "stub-revision");
    let key = s.hot_memory_cache_key(&request, &input).expect("cache key");
    assert!(!key.is_empty());
}
```

Update `StubStore` with method bodies in the test after the trait methods exist:

```rust
async fn hot_memory_input(
    &self,
    _request: &HotMemoryRequest,
) -> Result<HotMemoryInput, MemoryStoreError> {
    Ok(HotMemoryInput {
        sources: Vec::new(),
        source_revision: "stub-revision".to_owned(),
    })
}

fn hot_memory_cache_key(
    &self,
    request: &HotMemoryRequest,
    input: &HotMemoryInput,
) -> Result<String, MemoryStoreError> {
    Ok(format!("{}:{}", request.config_fingerprint, input.source_revision))
}

async fn load_hot_memory_cache(
    &self,
    _key: &str,
) -> Result<Option<HotMemoryOutput>, MemoryStoreError> {
    Ok(None)
}

async fn store_hot_memory_cache(
    &self,
    _key: &str,
    _output: &HotMemoryOutput,
) -> Result<(), MemoryStoreError> {
    Ok(())
}

async fn invalidate_hot_memory_cache(
    &self,
    _scope: HotMemoryInvalidationScope,
) -> Result<u64, MemoryStoreError> {
    Ok(0)
}
```

- [ ] **Step 2: Run contract test to verify it fails**

Run:

```bash
cargo test -p cairn-core contract::memory_store::tests::dyn_store_supports_hot_memory_methods --locked
```

Expected: compile failure for missing hot-memory request types and trait methods.

- [ ] **Step 3: Add request/error/invalidation types and trait methods**

At the top of `memory_store.rs`, import hot-memory types:

```rust
use crate::hot_memory::{HotMemoryInput, HotMemoryOutput};
```

Add public types before the trait:

```rust
/// Request context for assembling hot memory.
#[derive(Debug, Clone, PartialEq)]
pub struct HotMemoryRequest {
    /// Session scope for the hot prefix.
    pub session_id: Option<String>,
    /// Agent scope when known.
    pub agent_id: Option<String>,
    /// Effective byte budget.
    pub budget_bytes: u32,
    /// Stable fingerprint of config values that affect hot memory.
    pub config_fingerprint: String,
    /// Centrality blend weight from config.
    pub god_node_weight: f32,
}

/// Scope of a cache invalidation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotMemoryInvalidationScope {
    /// Delete every hot-memory cache row in the vault.
    Vault,
    /// Delete cache rows for a session.
    Session(String),
    /// Delete cache rows for an agent.
    Agent(String),
}

/// Errors from store-backed hot-memory reads and cache operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryStoreError {
    /// The store cannot satisfy the request.
    #[error("memory store unavailable: {0}")]
    Unavailable(String),
    /// A backend query failed.
    #[error("memory store query failed: {0}")]
    Query(String),
    /// A backend cache operation failed.
    #[error("memory store cache failed: {0}")]
    Cache(String),
}
```

Add methods to `MemoryStore`:

```rust
/// Fetch prepared hot-memory inputs for pure assembly.
async fn hot_memory_input(
    &self,
    request: &HotMemoryRequest,
) -> Result<HotMemoryInput, MemoryStoreError>;

/// Build the deterministic hot-memory cache key for this request and input.
fn hot_memory_cache_key(
    &self,
    request: &HotMemoryRequest,
    input: &HotMemoryInput,
) -> Result<String, MemoryStoreError>;

/// Return a cached assembled prefix when available.
async fn load_hot_memory_cache(
    &self,
    key: &str,
) -> Result<Option<HotMemoryOutput>, MemoryStoreError>;

/// Store an assembled prefix in the hot cache.
async fn store_hot_memory_cache(
    &self,
    key: &str,
    output: &HotMemoryOutput,
) -> Result<(), MemoryStoreError>;

/// Invalidate hot cache rows after relevant writes.
async fn invalidate_hot_memory_cache(
    &self,
    scope: HotMemoryInvalidationScope,
) -> Result<u64, MemoryStoreError>;
```

- [ ] **Step 4: Run contract tests**

Run:

```bash
cargo test -p cairn-core contract::memory_store --locked
```

Expected: PASS.

- [ ] **Step 5: Commit Task 3**

Run:

```bash
git add crates/cairn-core/src/contract/memory_store.rs
git commit -m "feat(contract): add hot memory store methods"
```

---

### Task 4: SQLite Store Schema, Source Retrieval, Centrality, And Cache

**Files:**
- Modify: `crates/cairn-store-sqlite/Cargo.toml`
- Modify: `crates/cairn-store-sqlite/src/lib.rs`
- Create: `crates/cairn-store-sqlite/tests/hot_memory.rs`

- [ ] **Step 1: Add failing SQLite integration tests**

Create `crates/cairn-store-sqlite/tests/hot_memory.rs`:

```rust
use cairn_core::contract::memory_store::{
    HotMemoryInvalidationScope, HotMemoryRequest, MemoryStore,
};
use cairn_core::hot_memory::{assemble_hot_memory, HotMemoryCacheInfo, HotMemoryOptions, HotMemorySourceKind};
use cairn_store_sqlite::{HotRecordSeed, SqliteMemoryStore};

fn request() -> HotMemoryRequest {
    HotMemoryRequest {
        session_id: Some("session-a".to_owned()),
        agent_id: Some("agent-a".to_owned()),
        budget_bytes: 4096,
        config_fingerprint: "config-a".to_owned(),
        god_node_weight: 0.3,
    }
}

#[tokio::test]
async fn hot_memory_input_reads_vault_files_records_and_edges() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store.write_vault_file("purpose.md", "purpose text").expect("purpose");
    store.write_vault_file("index.md", "index text").expect("index");
    store.insert_hot_record(HotRecordSeed::new("01J0000000000000000000001", "user", "pinned text").tag("pinned").salience(0.9)).expect("pinned");
    store.insert_hot_record(HotRecordSeed::new("01J0000000000000000000002", "playbook", "playbook text").salience(0.8)).expect("playbook");
    store.insert_entity_edge("ImportantType", "ContainsOnly", "uses", "src/important.rs", None).expect("edge");
    let input = store.hot_memory_input(&request()).await.expect("input");
    assert!(input.sources.iter().any(|s| s.kind == HotMemorySourceKind::Purpose && s.body.contains("purpose text")));
    assert!(input.sources.iter().any(|s| s.kind == HotMemorySourceKind::ProjectState && s.body.contains("index text")));
    assert!(input.sources.iter().any(|s| s.kind == HotMemorySourceKind::Pinned && s.body.contains("pinned text")));
    assert!(input.sources.iter().any(|s| s.kind == HotMemorySourceKind::Playbook && s.body.contains("playbook text")));
    assert!(input.sources.iter().any(|s| s.centrality_score > 0.0));
}

#[tokio::test]
async fn hot_memory_cache_hits_and_invalidates_by_session() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store.write_vault_file("purpose.md", "purpose text").expect("purpose");
    let req = request();
    let input = store.hot_memory_input(&req).await.expect("input");
    let key = store.hot_memory_cache_key(&req, &input).expect("key");
    let output = assemble_hot_memory(&input, HotMemoryOptions {
        budget_bytes: req.budget_bytes,
        god_node_weight: req.god_node_weight,
        cache: HotMemoryCacheInfo::refreshed(key.clone()),
    });
    store.store_hot_memory_cache(&key, &output).await.expect("store cache");
    assert!(store.load_hot_memory_cache(&key).await.expect("load cache").is_some());
    let deleted = store.invalidate_hot_memory_cache(HotMemoryInvalidationScope::Session("session-a".to_owned())).await.expect("invalidate");
    assert_eq!(deleted, 1);
    assert!(store.load_hot_memory_cache(&key).await.expect("load cache").is_none());
}
```

- [ ] **Step 2: Run SQLite tests to verify they fail**

Run:

```bash
cargo test -p cairn-store-sqlite --test hot_memory --locked
```

Expected: compile failure for missing `SqliteMemoryStore::open_memory`, `HotRecordSeed`, file helpers, and trait method implementations.

- [ ] **Step 3: Add store dependencies**

Modify `crates/cairn-store-sqlite/Cargo.toml`:

```toml
[dependencies]
cairn-core = { workspace = true }
async-trait = { workspace = true }
thiserror = { workspace = true }
rusqlite = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
tempfile = { workspace = true }
```

Keep `cairn-test-fixtures` as a dev-dependency.

- [ ] **Step 4: Replace the SQLite stub with a real minimal adapter**

Implement `SqliteMemoryStore` in `crates/cairn-store-sqlite/src/lib.rs` with:

```rust
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, HotMemoryInvalidationScope, HotMemoryRequest, MemoryStore,
    MemoryStoreCapabilities, MemoryStoreError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::hot_memory::{
    HotMemoryCacheInfo, HotMemoryInput, HotMemoryOutput, HotMemorySource, HotMemorySourceKind,
};
use cairn_core::register_plugin;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
```

Use this struct:

```rust
pub struct SqliteMemoryStore {
    conn: Mutex<Connection>,
    vault_path: PathBuf,
    _tempdir: Option<tempfile::TempDir>,
}
```

Expose constructors:

```rust
impl SqliteMemoryStore {
    pub fn open(vault_path: impl AsRef<Path>) -> Result<Self, MemoryStoreError> {
        let vault_path = vault_path.as_ref().to_path_buf();
        std::fs::create_dir_all(vault_path.join(".cairn"))
            .map_err(|e| MemoryStoreError::Unavailable(e.to_string()))?;
        let conn = Connection::open(vault_path.join(".cairn/cairn.db"))
            .map_err(|e| MemoryStoreError::Unavailable(e.to_string()))?;
        let store = Self { conn: Mutex::new(conn), vault_path, _tempdir: None };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_memory() -> Result<Self, MemoryStoreError> {
        let tempdir = tempfile::tempdir().map_err(|e| MemoryStoreError::Unavailable(e.to_string()))?;
        let conn = Connection::open_in_memory().map_err(|e| MemoryStoreError::Unavailable(e.to_string()))?;
        let store = Self { conn: Mutex::new(conn), vault_path: tempdir.path().to_path_buf(), _tempdir: Some(tempdir) };
        store.migrate()?;
        Ok(store)
    }
}
```

Create these tables in `migrate()`:

```sql
CREATE TABLE IF NOT EXISTS records (
    record_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    class TEXT NOT NULL DEFAULT 'semantic',
    visibility TEXT NOT NULL DEFAULT 'private',
    session_id TEXT,
    agent_id TEXT,
    body TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    evidence_score REAL NOT NULL DEFAULT 0.0,
    salience REAL NOT NULL DEFAULT 0.0,
    tags_json TEXT NOT NULL DEFAULT '[]',
    extra_frontmatter_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS entity_edges (
    edge_id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_entity TEXT NOT NULL,
    to_entity TEXT NOT NULL,
    edge_kind TEXT NOT NULL,
    source_file TEXT NOT NULL,
    invalid_at TEXT
);

CREATE TABLE IF NOT EXISTS hot_memory_cache (
    cache_key TEXT PRIMARY KEY,
    session_id TEXT,
    agent_id TEXT,
    budget_bytes INTEGER NOT NULL,
    config_fingerprint TEXT NOT NULL,
    source_revision TEXT NOT NULL,
    prefix TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

Implement test helpers used by integration tests:

```rust
pub struct HotRecordSeed {
    record_id: String,
    kind: String,
    body: String,
    session_id: Option<String>,
    agent_id: Option<String>,
    evidence_score: f32,
    salience: f32,
    tags: Vec<String>,
    extra: serde_json::Value,
}
```

Methods: `new(record_id, kind, body)`, `session(id)`, `agent(id)`, `evidence(score)`, `salience(score)`, `tag(tag)`, and `extra(value)`.

Implement helpers on `SqliteMemoryStore`: `write_vault_file`, `insert_hot_record`, and `insert_entity_edge`.

- [ ] **Step 5: Implement hot-memory input query**

In `hot_memory_input`, gather:

- `purpose.md` as `HotMemorySourceKind::Purpose`
- `index.md` as `HotMemorySourceKind::ProjectState`
- records with tag `pinned` and kind `user` or `feedback` as `Pinned`
- kind `project` or `reference` as `ProjectState`
- kind `playbook` as `Playbook`
- kind `user_signal` as `RecentUserSignal`
- kind `trace` or `reasoning` with extra field `"rolling_summary": true` as `RollingSummary`
- all other records with salience >= `0.7` as `HighSalience`

Use SQL ordering for candidate extraction:

```sql
SELECT record_id, kind, body, updated_at, evidence_score, salience, tags_json, extra_frontmatter_json
FROM records
WHERE session_id IS NULL OR session_id = ?1
ORDER BY updated_at DESC, record_id ASC
```

Compute centrality with live edges:

```sql
SELECT from_entity, to_entity, edge_kind, source_file
FROM entity_edges
WHERE invalid_at IS NULL
```

Filter structural hubs in Rust:

- Exclude an entity when `entity == file_stem(source_file)` for every edge mentioning it.
- Exclude an entity when all edge kinds mentioning it are `contains` or `method`.
- Normalize remaining degrees by dividing by the maximum remaining degree.

Assign each record's `centrality_score` by matching `record.body` or `record_id` text that contains an entity name; use `0.0` when no entity matches.

- [ ] **Step 6: Implement cache methods**

Use `sha2` for deterministic keys:

```rust
fn hash_parts(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}
```

`hot_memory_cache_key` should hash session id, agent id, budget, config fingerprint, and `input.source_revision`.

Serialize `HotMemoryOutput` metadata as JSON:

```rust
let metadata = serde_json::json!({
    "sources": output.sources,
    "truncation": output.truncation,
    "cache": output.cache,
});
```

When loading, rebuild `HotMemoryOutput` from `prefix`, `prefix.len()`, and metadata.

- [ ] **Step 7: Run SQLite tests**

Run:

```bash
cargo test -p cairn-store-sqlite --test hot_memory --locked
```

Expected: PASS.

- [ ] **Step 8: Commit Task 4**

Run:

```bash
git add crates/cairn-store-sqlite/Cargo.toml crates/cairn-store-sqlite/src/lib.rs crates/cairn-store-sqlite/tests/hot_memory.rs
git commit -m "feat(sqlite): support hot memory inputs and cache"
```

---

### Task 5: Core Verb Helper For Store + Cache + Assembler

**Files:**
- Modify: `crates/cairn-core/src/hot_memory.rs`
- Test: `crates/cairn-core/src/hot_memory.rs`

- [ ] **Step 1: Write failing async helper test**

Add to `hot_memory.rs` tests:

```rust
struct CacheStore {
    input: HotMemoryInput,
    cached: std::sync::Mutex<Option<HotMemoryOutput>>,
}

#[async_trait::async_trait]
impl crate::contract::memory_store::MemoryStore for CacheStore {
    fn name(&self) -> &str { "cache-store" }
    fn capabilities(&self) -> &crate::contract::memory_store::MemoryStoreCapabilities {
        static CAPS: crate::contract::memory_store::MemoryStoreCapabilities = crate::contract::memory_store::MemoryStoreCapabilities {
            fts: true,
            vector: false,
            graph_edges: true,
            transactions: true,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> crate::contract::version::VersionRange {
        crate::contract::version::VersionRange::new(
            crate::contract::version::ContractVersion::new(0, 1, 0),
            crate::contract::version::ContractVersion::new(0, 2, 0),
        )
    }
    async fn hot_memory_input(&self, _request: &crate::contract::memory_store::HotMemoryRequest) -> Result<HotMemoryInput, crate::contract::memory_store::MemoryStoreError> {
        Ok(self.input.clone())
    }
    fn hot_memory_cache_key(&self, _request: &crate::contract::memory_store::HotMemoryRequest, input: &HotMemoryInput) -> Result<String, crate::contract::memory_store::MemoryStoreError> {
        Ok(format!("key-{}", input.source_revision))
    }
    async fn load_hot_memory_cache(&self, _key: &str) -> Result<Option<HotMemoryOutput>, crate::contract::memory_store::MemoryStoreError> {
        Ok(self.cached.lock().expect("test mutex").clone())
    }
    async fn store_hot_memory_cache(&self, _key: &str, output: &HotMemoryOutput) -> Result<(), crate::contract::memory_store::MemoryStoreError> {
        *self.cached.lock().expect("test mutex") = Some(output.clone());
        Ok(())
    }
    async fn invalidate_hot_memory_cache(&self, _scope: crate::contract::memory_store::HotMemoryInvalidationScope) -> Result<u64, crate::contract::memory_store::MemoryStoreError> {
        Ok(0)
    }
}

#[tokio::test]
async fn assemble_hot_with_store_returns_refreshed_then_hit() {
    let store = CacheStore {
        input: HotMemoryInput {
            sources: vec![source(HotMemorySourceKind::Purpose, "01J0000000000000000000001", "purpose", 1.0)],
            source_revision: "rev".to_owned(),
        },
        cached: std::sync::Mutex::new(None),
    };
    let request = crate::contract::memory_store::HotMemoryRequest {
        session_id: None,
        agent_id: None,
        budget_bytes: 4096,
        config_fingerprint: "config".to_owned(),
        god_node_weight: 0.3,
    };
    let first = assemble_hot_with_store(&store, &request).await.expect("first");
    assert_eq!(first.cache.status, HotMemoryCacheStatus::Refreshed);
    let second = assemble_hot_with_store(&store, &request).await.expect("second");
    assert_eq!(second.cache.status, HotMemoryCacheStatus::Hit);
}
```

- [ ] **Step 2: Run test to verify failure**

Run:

```bash
cargo test -p cairn-core hot_memory::tests::assemble_hot_with_store_returns_refreshed_then_hit --locked
```

Expected: compile failure for missing `assemble_hot_with_store`.

- [ ] **Step 3: Implement the helper**

Add:

```rust
use crate::contract::memory_store::{HotMemoryRequest, MemoryStore, MemoryStoreError};

pub async fn assemble_hot_with_store<S: MemoryStore + ?Sized>(
    store: &S,
    request: &HotMemoryRequest,
) -> Result<HotMemoryOutput, MemoryStoreError> {
    let input = store.hot_memory_input(request).await?;
    let key = store.hot_memory_cache_key(request, &input)?;
    if let Some(mut cached) = store.load_hot_memory_cache(&key).await? {
        cached.cache = HotMemoryCacheInfo::hit(key);
        return Ok(cached);
    }
    let output = assemble_hot_memory(
        &input,
        HotMemoryOptions {
            budget_bytes: request.budget_bytes,
            god_node_weight: request.god_node_weight,
            cache: HotMemoryCacheInfo::refreshed(key.clone()),
        },
    );
    store.store_hot_memory_cache(&key, &output).await?;
    Ok(output)
}
```

- [ ] **Step 4: Run helper tests**

Run:

```bash
cargo test -p cairn-core hot_memory --locked
```

Expected: PASS.

- [ ] **Step 5: Commit Task 5**

Run:

```bash
git add crates/cairn-core/src/hot_memory.rs
git commit -m "feat(core): assemble hot memory through store cache"
```

---

### Task 6: CLI Wire-Up

**Files:**
- Modify: `crates/cairn-cli/Cargo.toml`
- Modify: `crates/cairn-cli/src/verbs/assemble_hot.rs`
- Modify: `crates/cairn-cli/tests/envelope_tests.rs`
- Create: `crates/cairn-cli/tests/assemble_hot.rs`

- [ ] **Step 1: Write failing CLI tests**

Create `crates/cairn-cli/tests/assemble_hot.rs`:

```rust
use std::fs;
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

#[test]
fn assemble_hot_json_returns_committed_prefix_and_metadata() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join(".cairn")).expect("config dir");
    fs::write(dir.path().join("purpose.md"), "purpose from cli").expect("purpose");
    let out = cli()
        .current_dir(dir.path())
        .args(["assemble_hot", "--json"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["verb"], "assemble_hot");
    assert_eq!(v["status"], "committed");
    assert!(v["data"]["prefix"].as_str().expect("prefix").contains("purpose from cli"));
    assert!(v["data"]["bytes"].as_u64().expect("bytes") > 0);
    assert!(v["data"]["sources"].is_array());
    assert!(v["data"]["truncation"].is_array());
    assert!(v["data"]["cache"]["status"].is_string());
}

#[test]
fn assemble_hot_budget_zero_returns_empty_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("purpose.md"), "purpose from cli").expect("purpose");
    let out = cli()
        .current_dir(dir.path())
        .args(["assemble_hot", "--budget", "0", "--json"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(v["data"]["prefix"], "");
    assert_eq!(v["data"]["bytes"], 0);
}
```

In `crates/cairn-cli/tests/envelope_tests.rs`, remove the `assemble_hot_returns_aborted_internal` test because the verb becomes implemented.

- [ ] **Step 2: Run CLI tests to verify they fail**

Run:

```bash
cargo test -p cairn-cli --test assemble_hot --locked
```

Expected: FAIL because current `assemble_hot` returns aborted Internal.

- [ ] **Step 3: Add CLI dependency if missing**

In `crates/cairn-cli/Cargo.toml`, ensure dependencies include:

```toml
cairn-store-sqlite = { workspace = true }
tokio = { workspace = true, features = ["rt"] }
```

- [ ] **Step 4: Wire `assemble_hot`**

Replace `crates/cairn-cli/src/verbs/assemble_hot.rs` with:

```rust
//! `cairn assemble_hot` handler.

use std::process::ExitCode;

use cairn_core::contract::memory_store::HotMemoryRequest;
use cairn_core::generated::envelope::{Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb};
use cairn_core::generated::verbs::assemble_hot::{
    AssembleHotData, CacheInfo, HotCacheStatus, SourceSummary, TruncationDecision,
};
use cairn_core::hot_memory::{assemble_hot_with_store, HotMemoryCacheStatus};
use cairn_store_sqlite::SqliteMemoryStore;
use clap::ArgMatches;

use crate::config::{self, CliOverrides};

use super::envelope::{emit_json, human_error, new_operation_id};

/// Run `cairn assemble_hot`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    match run_inner(sub) {
        Ok((resp, json)) => {
            if json {
                emit_json(&resp);
            } else if let Some(ResponseData::AssembleHot(data)) = &resp.data {
                println!(
                    "cairn assemble_hot: {} bytes, {} source groups, cache={}",
                    data.bytes,
                    data.sources.len(),
                    data.cache.status.as_str()
                );
                if !data.prefix.is_empty() {
                    println!("{}", data.prefix);
                }
            }
            ExitCode::SUCCESS
        }
        Err((resp, json)) => {
            if json {
                emit_json(&resp);
            } else {
                let message = resp
                    .error
                    .as_ref()
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("assemble_hot failed");
                human_error("assemble_hot", "Internal", message, &resp.operation_id);
            }
            ExitCode::FAILURE
        }
    }
}
```

Then implement `run_inner`, `committed_response`, `aborted_response`, and conversion helpers in the same file. The conversion helpers must map core structs into generated structs by field name. `run_inner` must:

1. Read `json = sub.get_flag("json")`.
2. Set `vault_path = std::env::current_dir()`.
3. Load config with `config::load(&vault_path, &CliOverrides::default())`.
4. Open `SqliteMemoryStore::open(&vault_path)`.
5. Use `budget = sub.get_one::<u32>("budget").copied().unwrap_or(config.vault.hot_memory.max_bytes)`.
6. Use `session_id = sub.get_one::<String>("session_id").cloned()`.
7. Build `HotMemoryRequest` with `config_fingerprint = serde_json::to_string(&config.vault.hot_memory)`.
8. Call `tokio::runtime::Builder::new_current_thread().enable_all().build()` and `block_on(assemble_hot_with_store(&store, &request))`.

Use `Internal` aborted envelopes for config/store/runtime errors so generated envelope validation still passes.

- [ ] **Step 5: Run CLI tests**

Run:

```bash
cargo test -p cairn-cli --test assemble_hot --locked
cargo test -p cairn-cli --test envelope_tests --locked
```

Expected: PASS.

- [ ] **Step 6: Commit Task 6**

Run:

```bash
git add crates/cairn-cli/Cargo.toml crates/cairn-cli/src/verbs/assemble_hot.rs crates/cairn-cli/tests/envelope_tests.rs crates/cairn-cli/tests/assemble_hot.rs
git commit -m "feat(cli): wire assemble_hot"
```

---

### Task 7: Fixture And Snapshot Coverage

**Files:**
- Create: `fixtures/v0/hot-memory/default.json`
- Create: `fixtures/v0/hot-memory/truncated.json`
- Modify: `crates/cairn-test-fixtures/tests/schema_fixtures.rs`
- Snapshot updates under `crates/cairn-test-fixtures/tests/snapshots/`

- [ ] **Step 1: Add fixture JSON files**

Create `fixtures/v0/hot-memory/default.json`:

```json
{
  "prefix": "## purpose\npurpose text\n",
  "bytes": 24,
  "sources": [
    { "kind": "purpose", "attempted": 1, "included": 1, "omitted": 0, "bytes": 24 }
  ],
  "truncation": [],
  "cache": { "status": "miss", "key": "fixture-default" }
}
```

Create `fixtures/v0/hot-memory/truncated.json`:

```json
{
  "prefix": "## purpose\n",
  "bytes": 11,
  "sources": [
    { "kind": "purpose", "attempted": 1, "included": 1, "omitted": 0, "bytes": 11 }
  ],
  "truncation": [
    {
      "kind": "purpose",
      "reason": "section_truncated",
      "attempted_bytes": 32,
      "included_bytes": 11
    }
  ],
  "cache": { "status": "miss", "key": "fixture-truncated" }
}
```

- [ ] **Step 2: Add failing fixture tests**

In `crates/cairn-test-fixtures/tests/schema_fixtures.rs`, add tests that load those fixtures as `cairn_core::generated::verbs::assemble_hot::AssembleHotData` and snapshot them:

```rust
#[test]
fn hot_memory_default_fixture_validates() {
    let path = fixtures_dir().join("v0/hot-memory/default.json");
    let data: cairn_core::generated::verbs::assemble_hot::AssembleHotData = load_json(path);
    insta::assert_json_snapshot!("hot_memory_default", &data);
}

#[test]
fn hot_memory_truncated_fixture_validates() {
    let path = fixtures_dir().join("v0/hot-memory/truncated.json");
    let data: cairn_core::generated::verbs::assemble_hot::AssembleHotData = load_json(path);
    insta::assert_json_snapshot!("hot_memory_truncated", &data);
}
```

- [ ] **Step 3: Run fixture tests to verify snapshots are new**

Run:

```bash
cargo test -p cairn-test-fixtures hot_memory --locked
```

Expected: FAIL with new insta snapshots pending.

- [ ] **Step 4: Review fixture snapshots**

Run:

```bash
cargo insta review
```

Expected: accept only `hot_memory_default` and `hot_memory_truncated`.

- [ ] **Step 5: Commit Task 7**

Run:

```bash
git add fixtures/v0/hot-memory crates/cairn-test-fixtures/tests/schema_fixtures.rs crates/cairn-test-fixtures/tests/snapshots
git commit -m "test(fixtures): add hot memory response fixtures"
```

---

### Task 8: Full Verification And Fixups

**Files:**
- Modify only files touched by prior tasks when verification exposes issues.

- [ ] **Step 1: Run formatting**

Run:

```bash
cargo fmt --all --check
```

Expected: PASS. If it fails, run `cargo fmt --all`, inspect the diff, and commit the formatting with the relevant task files.

- [ ] **Step 2: Run focused package tests**

Run:

```bash
cargo test -p cairn-core hot_memory --locked
cargo test -p cairn-store-sqlite --test hot_memory --locked
cargo test -p cairn-cli --test assemble_hot --locked
cargo test -p cairn-test-fixtures hot_memory --locked
```

Expected: PASS.

- [ ] **Step 3: Run codegen drift check**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: PASS.

- [ ] **Step 4: Run workspace checks**

Run:

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
```

Expected: PASS.

- [ ] **Step 5: Run docs check**

Run:

```bash
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked
```

Expected: PASS.

- [ ] **Step 6: Inspect final diff**

Run:

```bash
git status --short
git diff --stat HEAD
git diff --check
```

Expected: only intentional tracked changes, no whitespace errors.

- [ ] **Step 7: Commit verification fixups**

If Step 1 through Step 6 required any fixes, commit them:

```bash
git add -A
git commit -m "fix: satisfy hot memory verification"
```

If there are no changes, do not create an empty commit.

---

## Self-Review Checklist

- Spec coverage: Tasks 1 and 7 cover wire shape and fixture metadata; Tasks 2 and 5 cover pure assembly, ordering, budgeting, truncation, and cache integration; Tasks 3 and 4 cover store reads, centrality, cache, and invalidation; Task 6 covers CLI; Task 8 covers verification.
- Completeness scan: every task names exact files, commands, expected outcomes, and concrete code or schema snippets.
- Type consistency: core uses `HotMemorySourceKind`, `HotMemoryInput`, `HotMemoryOutput`, `HotMemoryCacheInfo`, and `HotMemoryCacheStatus`; contract and SQLite use those same core types; CLI converts those core types into generated `AssembleHotData` types after codegen.
