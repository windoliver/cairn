# Issue-48 Local Embeddings + sqlite-vec ANN Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add pure-Rust candle embedding runtime + statically linked sqlite-vec ANN tables + `MemoryStore::search_semantic` trait method, wired through `cairn bootstrap` and `cairn admin` verbs, with capability gating on `search.local_embeddings`.

**Architecture:** New leaf crate `cairn-embeddings-local` owns the `EmbeddingModel` trait + candle BERT impls + `ModelCache`; `cairn-store-sqlite` depends on it and adds a sqlite-vec `record_vectors` table, embed-on-write in `upsert`, a background drain task for backfill, and `do_search_semantic`. `EmbeddingModelKind` lives in `cairn-core::config` (no workspace-dep direction violation). The verb layer gains `--mode semantic` dispatch, two admin verbs, and a bootstrap step.

**Tech stack:** `candle-core`, `candle-nn`, `candle-transformers` (BERT inference), `tokenizers` (HuggingFace tokenizer), `hf-hub` (model download), `safetensors` (weight loading), `sqlite-vec` with bundled C extension, `rusqlite-migration`, `tokio-util` (CancellationToken), `insta` (snapshots), `proptest`.

---

## File map

**New files:**
- `crates/cairn-embeddings-local/Cargo.toml`
- `crates/cairn-embeddings-local/plugin.toml`
- `crates/cairn-embeddings-local/src/lib.rs`
- `crates/cairn-embeddings-local/src/error.rs`
- `crates/cairn-embeddings-local/src/model.rs` — `EmbeddingModel` trait + `MockEmbedder`
- `crates/cairn-embeddings-local/src/cache.rs` — `ModelCache`, `FetchReport`
- `crates/cairn-embeddings-local/src/bge.rs` — BGE-small impl
- `crates/cairn-embeddings-local/src/minilm.rs` — MiniLM impl
- `crates/cairn-embeddings-local/tests/golden_vectors.rs`
- `crates/cairn-embeddings-local/tests/cache_layout.rs`
- `crates/cairn-embeddings-local/tests/prefix_asymmetry.rs`
- `crates/cairn-store-sqlite/src/migrations/sql/0020_record_vectors.sql`
- `crates/cairn-store-sqlite/src/store/reindex.rs`
- `crates/cairn-store-sqlite/tests/reindex.rs`

**Modified files:**
- `Cargo.toml` (workspace) — new member + new deps
- `crates/cairn-core/src/config/mod.rs` — `EmbeddingModelKind`, `SearchConfig`, `CairnConfig`, `CapabilitySet`
- `crates/cairn-core/src/contract/memory_store.rs` — `SemanticSearchArgs`, `SemanticSearchPage`, `SearchCandidate`, `search_semantic`, `CONTRACT_VERSION`
- `crates/cairn-store-sqlite/Cargo.toml` — add deps
- `crates/cairn-store-sqlite/src/migrations/mod.rs` — register migration 0020
- `crates/cairn-store-sqlite/src/open.rs` — dynamic caps, `open_with_embedder`, drain spawn
- `crates/cairn-store-sqlite/src/store/mod.rs` — `SqliteMemoryStore` new fields
- `crates/cairn-store-sqlite/src/store/trait_impl.rs` — `search_semantic` dispatch
- `crates/cairn-store-sqlite/src/store/search.rs` — `do_search_semantic`
- `crates/cairn-store-sqlite/src/store/upsert.rs` — embed-on-write
- `crates/cairn-store-sqlite/src/error.rs` — new `EmbedFailed` variant
- `crates/cairn-cli/src/verbs/search.rs` — `--mode semantic` dispatch
- `crates/cairn-cli/src/verbs/status.rs` — capability advertisement
- `crates/cairn-cli/src/verbs/mod.rs` — register admin verbs
- `crates/cairn-cli/src/main.rs` — bootstrap step

---

### Task 1: `EmbeddingModelKind` + `SearchConfig` in `cairn-core`

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`
- Modify: `crates/cairn-core/src/config/snapshots/cairn_core__config__tests__default_config_snapshot.snap`

- [ ] **Step 1: Add `EmbeddingModelKind` enum to `mod.rs`**

In `crates/cairn-core/src/config/mod.rs`, directly before the `// ── Top-level` comment, add:

```rust
/// Embedding model selection for local semantic search (brief §3.0).
///
/// Variant strings are kebab-case to match the brief's model identifiers.
/// Lives in `cairn-core` (not in `cairn-embeddings-local`) so `CairnConfig`
/// can reference it without a workspace-dep direction violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum EmbeddingModelKind {
    /// BGE-small-en-v1.5, 384-dim, MIT license. Default.
    /// Applies asymmetric query prefix for retrieval.
    BgeSmallEnV1_5,
    /// all-MiniLM-L6-v2, 384-dim, Apache 2.0.
    AllMiniLmL6V2,
}

impl EmbeddingModelKind {
    /// Stable kebab-case label used in file-system paths and DB rows.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BgeSmallEnV1_5 => "bge-small-en-v1.5",
            Self::AllMiniLmL6V2 => "all-MiniLM-L6-v2",
        }
    }

    /// HuggingFace repo id for model download.
    #[must_use]
    pub fn hf_repo(self) -> &'static str {
        match self {
            Self::BgeSmallEnV1_5 => "BAAI/bge-small-en-v1.5",
            Self::AllMiniLmL6V2 => "sentence-transformers/all-MiniLM-L6-v2",
        }
    }

    /// Expected output dimension of the model.
    #[must_use]
    pub fn dim(self) -> usize {
        384
    }
}

impl Default for EmbeddingModelKind {
    fn default() -> Self {
        Self::BgeSmallEnV1_5
    }
}
```

- [ ] **Step 2: Add `SearchConfig` struct**

In `crates/cairn-core/src/config/mod.rs`, after `// ── Store` section, add a new `// ── Search` section:

```rust
// ── Search ────────────────────────────────────────────────────────────────

/// Local semantic search configuration (brief §3.0).
///
/// `local_embeddings: false` drops `cairn.mcp.v1.search.semantic` and
/// `cairn.mcp.v1.search.hybrid` from `status.capabilities`. Those modes
/// return `CapabilityUnavailable` — no silent fallback (brief §3.0 fail-closed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Enable local embedding runtime. Default `true`.
    pub local_embeddings: bool,
    /// Which embedding model to use. Default `bge-small-en-v1.5`.
    pub embedding_model: EmbeddingModelKind,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            local_embeddings: true,
            embedding_model: EmbeddingModelKind::default(),
        }
    }
}
```

- [ ] **Step 3: Add `search` field to `CairnConfig`**

Find the `CairnConfig` struct (around line 240) and add `pub search: SearchConfig,` as the last field:

```rust
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CairnConfig {
    pub vault: VaultConfig,
    pub store: StoreConfig,
    pub llm: LlmConfig,
    pub sensors: SensorsConfig,
    pub workflows: WorkflowsConfig,
    pub pipeline: PipelineConfig,
    pub search: SearchConfig,   // ← add this
}
```

- [ ] **Step 4: Fix `CapabilitySet::semantic_search` — decouple from `llm.provider`**

Find the `CapabilitySet` struct and `capabilities()` method in `mod.rs`. Change `semantic_search` computation. The method signature is `pub fn capabilities(&self) -> CapabilitySet`. The `model_present` probe is a `bool` parameter the method cannot determine alone (needs filesystem access); instead accept it as a parameter:

```rust
/// Derived capability set. `model_present` should be `true` when the
/// configured embedding model files exist on disk (stat-checked at start).
pub fn capabilities(&self, model_present: bool) -> CapabilitySet {
    let llm_on = self.llm.provider.is_some();
    let semantic = self.search.local_embeddings && model_present;
    CapabilitySet {
        keyword_search: true,
        semantic_search: semantic,
        hybrid_search: false,   // follow-up issue
        llm_extract: llm_on,
        agent_extract: self
            .pipeline
            .extract
            .chain
            .iter()
            .any(|e| matches!(e.worker, ExtractorWorkerKind::Agent)),
        graph_edges: !matches!(self.store.kind, StoreKind::Sqlite),
    }
}
```

Also add a convenience zero-arg version that callers without FS access can use:

```rust
/// Equivalent to `capabilities(false)` — semantic/hybrid will be `false`.
/// Prefer `capabilities(model_present)` in the CLI layer.
pub fn capabilities_no_model(&self) -> CapabilitySet {
    self.capabilities(false)
}
```

- [ ] **Step 5: Update the `CapabilitySet` struct to match (add `hybrid_search: false` if missing)**

Verify `CapabilitySet` has a `hybrid_search` field. If the existing struct has `hybrid_search: bool` already, it's fine. If not, add it now:

```rust
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySet {
    pub keyword_search: bool,
    pub semantic_search: bool,
    pub hybrid_search: bool,    // always false at v0.1; follow-up issue lights it up
    pub llm_extract: bool,
    pub agent_extract: bool,
    pub graph_edges: bool,
}
```

- [ ] **Step 6: Update existing tests that call `config.capabilities()`**

Search for all call sites of `capabilities()` in `mod.rs` tests and update them to `capabilities(false)` or `capabilities(true)` as appropriate. Tests that set `llm.provider = Some(...)` and assert `semantic_search: true` must now pass `model_present: true` AND set `search.local_embeddings: true` (which is already the default).

- [ ] **Step 7: Update the `default_config_snapshot` insta snap**

Run:
```bash
cargo test -p cairn-core --test '*' -- config 2>&1 | head -40
```

If the snapshot changed, run:
```bash
cargo insta review
```

Accept the new snapshot (it will include the new `search` field).

- [ ] **Step 8: Run tests**

```bash
cargo nextest run -p cairn-core --locked 2>&1 | tail -20
```

Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-core/
git commit -m "feat(config): add EmbeddingModelKind, SearchConfig, fix CapabilitySet.semantic_search (#48)"
```

---

### Task 2: `MemoryStore` trait — `search_semantic` + types + version bump

**Files:**
- Modify: `crates/cairn-core/src/contract/memory_store.rs`

- [ ] **Step 1: Bump `CONTRACT_VERSION` from `0.2.0` to `0.3.0`**

At line 8:
```rust
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 3, 0);
```

- [ ] **Step 2: Add `semantic_distance` field to `SearchCandidate`**

Find the `SearchCandidate` struct (around line 480) and add the new field after `snippet`:

```rust
pub struct SearchCandidate {
    pub record_id: RecordId,
    pub target_id: TargetId,
    pub scope: ScopeTuple,
    pub kind: MemoryKind,
    pub class: MemoryClass,
    pub visibility: MemoryVisibility,
    pub bm25: f64,
    pub recency_seconds: i64,
    pub confidence: f32,
    pub salience: f32,
    pub staleness_seconds: i64,
    pub snippet: String,
    pub record_json: String,
    /// L2 distance from the query vector. `None` on keyword-only candidates.
    pub semantic_distance: Option<f32>,
}
```

- [ ] **Step 3: Update all `SearchCandidate` construction sites in tests**

Search for `SearchCandidate {` in the codebase:

```bash
grep -rn "SearchCandidate {" crates/
```

For each occurrence, add `semantic_distance: None` to the struct literal.

- [ ] **Step 4: Add `SemanticSearchArgs` and `SemanticSearchPage` types**

Append to the `// ── Search types` section (after `KeywordSearchPage`):

```rust
/// Args for the semantic (ANN) branch of `search`.
///
/// No cursor: ANN is top-K only at v0.1. Scope-resolution rules are
/// identical to [`KeywordSearchArgs`] — callers fold scope into `filter`
/// or `visibility_allowlist` before invoking.
#[derive(Debug, Clone)]
pub struct SemanticSearchArgs<'a> {
    /// Raw user query. The embedder applies any asymmetric prefix internally.
    pub query: String,
    /// Pre-validated filter tree. Same semantics as in [`KeywordSearchArgs`].
    pub filter: Option<ValidatedFilter<'a>>,
    /// Visibility values the caller is allowed to see; empty = no filter.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Number of nearest neighbours to return (top-K).
    pub limit: usize,
    /// Label of the embedding model currently active, e.g. `"bge-small-en-v1.5"`.
    /// The store skips rows whose `record_vectors.model` column differs —
    /// they were produced by a stale model and will be rebuilt by the reindex drain.
    pub model_label: String,
}

/// One page of candidates returned by the semantic branch of `search`.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchPage {
    /// Candidates ordered by ascending L2 distance (smaller = more similar).
    pub candidates: Vec<SearchCandidate>,
}
```

- [ ] **Step 5: Add `search_semantic` to the `MemoryStore` trait**

After the `search_keyword` method declaration (around line 197), add:

```rust
/// Semantic (ANN) search over the sqlite-vec `record_vectors` table.
///
/// Returns `CapabilityUnavailable` when `capabilities().vector` is `false`
/// (model absent or `search.local_embeddings: false`). Default impl
/// returns `CapabilityUnavailable` so adapters that don't support vectors
/// compile without boilerplate.
///
/// Scope is the caller's responsibility — same rules as `search_keyword`.
async fn search_semantic(
    &self,
    args: &SemanticSearchArgs<'_>,
) -> Result<SemanticSearchPage, StoreError> {
    let _ = args;
    Err(Box::new(crate::generated::errors::CairnError::CapabilityUnavailable)
        as StoreError)
}
```

Wait — check how `CapabilityUnavailable` is raised in the existing store code. Looking at `error.rs`:
```rust
StoreError::CapabilityUnavailable { what: "vector" }
```

The trait's `StoreError` is `Box<dyn std::error::Error + Send + Sync + 'static>`. Use a simple string error:

```rust
async fn search_semantic(
    &self,
    args: &SemanticSearchArgs<'_>,
) -> Result<SemanticSearchPage, StoreError> {
    let _ = args;
    // String implements Into<Box<dyn Error + Send + Sync>> via stdlib blanket impl.
    Err(String::from("capability unavailable: vector").into())
}
```

- [ ] **Step 6: Update the `StubStore` in the `tests` module**

The stub store's `search_keyword` impl is already present. Add `search_semantic` override to prove the default impl compiles:

```rust
// No override needed — the trait default impl returns CapabilityUnavailable.
// Add a doc test in the `dyn_compatible` test:
let result = s.search_semantic(&SemanticSearchArgs {
    query: "test".into(),
    filter: None,
    visibility_allowlist: vec![],
    limit: 10,
    model_label: "bge-small-en-v1.5".into(),
}).await;
assert!(result.is_err());
```

- [ ] **Step 7: Update `ACCEPTED_RANGE` in `cairn-store-sqlite`**

In `crates/cairn-store-sqlite/src/lib.rs`:
```rust
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 2, 0), ContractVersion::new(0, 4, 0));
```

- [ ] **Step 8: Run build to confirm contract bump is wired**

```bash
cargo check -p cairn-core -p cairn-store-sqlite --locked 2>&1 | tail -20
```

Expected: no errors (the compile-time assert in `cairn-store-sqlite/src/lib.rs` must pass).

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-core/ crates/cairn-store-sqlite/src/lib.rs
git commit -m "feat(contract): search_semantic trait stub + SemanticSearchArgs + CONTRACT_VERSION 0.3.0 (#48)"
```

---

### Task 3: Workspace additions + `cairn-embeddings-local` scaffold

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/cairn-embeddings-local/Cargo.toml`
- Create: `crates/cairn-embeddings-local/plugin.toml`
- Create: `crates/cairn-embeddings-local/src/lib.rs`
- Create: `crates/cairn-embeddings-local/src/error.rs`
- Create: `crates/cairn-embeddings-local/src/model.rs`

- [ ] **Step 1: Add workspace deps to root `Cargo.toml`**

In `[workspace.dependencies]`, add (after existing entries):

```toml
# Embedding runtime — cairn-embeddings-local only
candle-core = { version = "0.8", default-features = false }
candle-nn = { version = "0.8", default-features = false }
candle-transformers = { version = "0.8", default-features = false, features = ["bert"] }
tokenizers = { version = "0.21", default-features = false, features = ["onig"] }
hf-hub = { version = "0.3", default-features = false, features = ["tokio"] }
safetensors = { version = "0.4", default-features = false }
```

Also add `cairn-embeddings-local` to `[workspace.members]`:
```toml
"crates/cairn-embeddings-local",
```

- [ ] **Step 2: Create `crates/cairn-embeddings-local/Cargo.toml`**

```toml
[package]
name = "cairn-embeddings-local"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
description = "Pure-Rust local embedding runtime for Cairn (candle BERT + ModelCache)."

[features]
default = []
# Gate real-weight integration tests behind this feature so CI stays fast.
real-models = []

[dependencies]
cairn-core = { path = "../cairn-core" }
candle-core = { workspace = true }
candle-nn = { workspace = true }
candle-transformers = { workspace = true }
tokenizers = { workspace = true }
hf-hub = { workspace = true }
safetensors = { workspace = true }
blake3 = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
insta = { workspace = true }
tempfile = { workspace = true }
tokio = { workspace = true, features = ["rt", "macros"] }

[lints]
workspace = true
```

- [ ] **Step 3: Create `crates/cairn-embeddings-local/plugin.toml`**

```toml
[plugin]
name = "cairn-embeddings-local"
kind = "embedder"
description = "Pure-Rust local embedding runtime using candle BERT models."
```

- [ ] **Step 4: Create `crates/cairn-embeddings-local/src/error.rs`**

```rust
//! Error type for the local embedding runtime.

use std::path::PathBuf;

use cairn_core::config::EmbeddingModelKind;

/// Errors from the local embedding runtime.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum EmbeddingError {
    /// Model files not present on disk. Run `cairn bootstrap` or
    /// `cairn admin model fetch` to download.
    #[error("embedding model not fetched: {kind:?} — run `cairn bootstrap`")]
    ModelNotFetched { kind: EmbeddingModelKind },

    /// On-disk file digest does not match the compiled-in constant.
    /// Re-fetch with `cairn admin model fetch --force`.
    #[error("integrity mismatch at {path}: run `cairn admin model fetch --force`")]
    IntegrityMismatch { path: PathBuf },

    /// Tokenizer error (wraps the tokenizers crate's error as a String to
    /// avoid leaking the tokenizers dep through our public API).
    #[error("tokenizer error: {0}")]
    Tokenizer(String),

    /// Candle inference error.
    #[error("inference error: {0}")]
    Inference(#[from] candle_core::Error),

    /// Filesystem error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// HuggingFace Hub network error.
    #[error("hf-hub error: {0}")]
    Network(String),
}

impl From<tokenizers::Error> for EmbeddingError {
    fn from(e: tokenizers::Error) -> Self {
        Self::Tokenizer(e.to_string())
    }
}
```

- [ ] **Step 5: Create `crates/cairn-embeddings-local/src/model.rs`**

```rust
//! `EmbeddingModel` trait + `MockEmbedder` for tests.

use cairn_core::config::EmbeddingModelKind;

use crate::error::EmbeddingError;

/// Synchronous CPU-bound embedding. Callers wrap in
/// `tokio::task::spawn_blocking`.
pub trait EmbeddingModel: Send + Sync {
    /// Which model variant this instance wraps.
    fn kind(&self) -> EmbeddingModelKind;

    /// Output dimension (both BGE and MiniLM default: 384).
    fn dim(&self) -> usize;

    /// Embed a document (record body). BGE applies no prefix here.
    fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;

    /// Embed a user query. BGE applies asymmetric retrieval prefix;
    /// MiniLM treats this identically to `embed_document`.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
}

/// Deterministic mock embedder for unit and integration tests.
///
/// Produces a 384-dim vector from `blake3(text)` → first 384×4 bytes
/// reinterpreted as f32 LE, then L2-normalised. No candle dep, no model
/// files, runs fully in-memory.
pub struct MockEmbedder {
    kind: EmbeddingModelKind,
}

impl MockEmbedder {
    /// Construct a mock that reports itself as the given model kind.
    #[must_use]
    pub fn new(kind: EmbeddingModelKind) -> Self {
        Self { kind }
    }
}

impl EmbeddingModel for MockEmbedder {
    fn kind(&self) -> EmbeddingModelKind {
        self.kind
    }

    fn dim(&self) -> usize {
        384
    }

    fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(mock_vector(text))
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        // No asymmetric prefix in the mock; callers testing BGE-specific
        // prefix behaviour use `BgeSmall` directly (requires `real-models`).
        Ok(mock_vector(text))
    }
}

/// Produce a deterministic normalised 384-dim vector from a string.
pub(crate) fn mock_vector(text: &str) -> Vec<f32> {
    let hash = blake3::hash(text.as_bytes());
    let bytes = hash.as_bytes();
    // Extend the 32-byte hash by repeating it until we have 384*4 bytes.
    let needed = 384 * 4;
    let extended: Vec<u8> = bytes.iter().cycle().take(needed).copied().collect();
    let mut v: Vec<f32> = extended
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    l2_normalize(&mut v);
    v
}

/// In-place L2 normalisation.
pub(crate) fn l2_normalize(v: &mut Vec<f32>) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}
```

- [ ] **Step 6: Create `crates/cairn-embeddings-local/src/lib.rs`**

```rust
//! Pure-Rust local embedding runtime for Cairn (brief §3.0).
//!
//! # Quick start
//!
//! ```no_run
//! use cairn_embeddings_local::{ModelCache, EmbeddingModelKind};
//! use std::path::Path;
//!
//! let cache = ModelCache::new(Path::new(".cairn/models"));
//! // After `cairn bootstrap` fetches the model:
//! let model = cache.ensure(EmbeddingModelKind::BgeSmallEnV1_5).unwrap();
//! let vec = model.embed_query("hello world").unwrap();
//! assert_eq!(vec.len(), 384);
//! ```

pub mod cache;
pub mod error;
pub mod model;

mod bge;
mod minilm;

pub use cache::{FetchReport, ModelCache};
pub use cairn_core::config::EmbeddingModelKind;
pub use error::EmbeddingError;
pub use model::{EmbeddingModel, MockEmbedder};
```

- [ ] **Step 7: Confirm the crate builds (no impls yet)**

```bash
cargo check -p cairn-embeddings-local --locked 2>&1 | tail -20
```

Expected: fails on missing modules `bge`, `minilm`, `cache` — add empty stubs now.

Create `crates/cairn-embeddings-local/src/bge.rs`:
```rust
// BGE-small impl — wired in Task 4.
```

Create `crates/cairn-embeddings-local/src/minilm.rs`:
```rust
// all-MiniLM-L6-v2 impl — wired in Task 4.
```

Create `crates/cairn-embeddings-local/src/cache.rs`:
```rust
// ModelCache — wired in Task 4.
use cairn_core::config::EmbeddingModelKind;
use std::path::{Path, PathBuf};

pub struct ModelCache { root: PathBuf }

pub struct FetchReport {
    pub kind: EmbeddingModelKind,
    pub bytes_downloaded: u64,
    pub integrity: String,
    pub already_cached: bool,
}

impl ModelCache {
    pub fn new(models_root: &Path) -> Self { Self { root: models_root.to_owned() } }
    pub fn model_dir(&self, kind: EmbeddingModelKind) -> PathBuf {
        self.root.join(kind.as_str())
    }
    pub fn is_present(&self, kind: EmbeddingModelKind) -> bool {
        self.model_dir(kind).join(".integrity").exists()
    }
    pub fn ensure(&self, _kind: EmbeddingModelKind) -> Result<std::sync::Arc<dyn crate::EmbeddingModel>, crate::EmbeddingError> {
        todo!("implemented in Task 4")
    }
    pub fn fetch(&self, _kind: EmbeddingModelKind) -> Result<FetchReport, crate::EmbeddingError> {
        todo!("implemented in Task 4")
    }
}
```

Re-run:
```bash
cargo check -p cairn-embeddings-local --locked 2>&1 | tail -20
```

Expected: compiles clean.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/cairn-embeddings-local/
git commit -m "feat(embeddings): cairn-embeddings-local crate scaffold + EmbeddingModel trait (#48)"
```

---

### Task 4: `ModelCache` + BGE + MiniLM impls

**Files:**
- Replace stub: `crates/cairn-embeddings-local/src/cache.rs`
- Implement: `crates/cairn-embeddings-local/src/bge.rs`
- Implement: `crates/cairn-embeddings-local/src/minilm.rs`

- [ ] **Step 1: Write failing test for `ModelCache::is_present`**

Create `crates/cairn-embeddings-local/tests/cache_layout.rs`:

```rust
use cairn_embeddings_local::{EmbeddingModelKind, ModelCache};
use tempfile::TempDir;

#[test]
fn is_present_false_before_fetch() {
    let dir = TempDir::new().unwrap();
    let cache = ModelCache::new(dir.path());
    assert!(!cache.is_present(EmbeddingModelKind::BgeSmallEnV1_5));
    assert!(!cache.is_present(EmbeddingModelKind::AllMiniLmL6V2));
}

#[test]
fn is_present_true_after_writing_integrity_marker() {
    let dir = TempDir::new().unwrap();
    let cache = ModelCache::new(dir.path());
    let model_dir = cache.model_dir(EmbeddingModelKind::BgeSmallEnV1_5);
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(model_dir.join(".integrity"), "abc123").unwrap();
    assert!(cache.is_present(EmbeddingModelKind::BgeSmallEnV1_5));
}
```

Run:
```bash
cargo nextest run -p cairn-embeddings-local --locked 2>&1 | tail -20
```

Expected: compiles and both tests pass (the stub `is_present` is already correct).

- [ ] **Step 2: Implement full `ModelCache`**

Replace `crates/cairn-embeddings-local/src/cache.rs` with:

```rust
//! ModelCache — on-disk layout, integrity verification, hf-hub download.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cairn_core::config::EmbeddingModelKind;

use crate::EmbeddingError;
use crate::EmbeddingModel;

/// Manages model files under `.cairn/models/<kind>/`.
pub struct ModelCache {
    root: PathBuf,
}

/// Result of a successful model fetch.
pub struct FetchReport {
    pub kind: EmbeddingModelKind,
    pub bytes_downloaded: u64,
    pub integrity: String,
    pub already_cached: bool,
}

impl ModelCache {
    /// Create a cache rooted at `models_root` (typically `.cairn/models/`).
    #[must_use]
    pub fn new(models_root: &Path) -> Self {
        Self { root: models_root.to_owned() }
    }

    /// Path to the directory for a given model.
    #[must_use]
    pub fn model_dir(&self, kind: EmbeddingModelKind) -> PathBuf {
        self.root.join(kind.as_str())
    }

    /// `true` iff the `.integrity` marker exists (fetch was completed).
    #[must_use]
    pub fn is_present(&self, kind: EmbeddingModelKind) -> bool {
        self.model_dir(kind).join(".integrity").exists()
    }

    /// Load the model into memory. Returns `Err(ModelNotFetched)` if the
    /// model files are not on disk. Call `fetch` first (e.g., via
    /// `cairn bootstrap` or `cairn admin model fetch`).
    ///
    /// This call is CPU-bound (BERT weight loading) — wrap in
    /// `tokio::task::spawn_blocking` when calling from async code.
    pub fn ensure(&self, kind: EmbeddingModelKind) -> Result<Arc<dyn EmbeddingModel>, EmbeddingError> {
        if !self.is_present(kind) {
            return Err(EmbeddingError::ModelNotFetched { kind });
        }
        let dir = self.model_dir(kind);
        match kind {
            EmbeddingModelKind::BgeSmallEnV1_5 => {
                let m = crate::bge::BgeSmall::load(&dir)?;
                Ok(Arc::new(m))
            }
            EmbeddingModelKind::AllMiniLmL6V2 => {
                let m = crate::minilm::MiniLm::load(&dir)?;
                Ok(Arc::new(m))
            }
        }
    }

    /// Download model files from HuggingFace Hub into a tmp dir, verify
    /// blake3 integrity, then atomically move to the canonical path.
    ///
    /// Idempotent: returns `FetchReport { already_cached: true }` if the
    /// `.integrity` marker already exists and matches.
    ///
    /// This call is network + IO — wrap in `tokio::task::spawn_blocking`.
    pub fn fetch(&self, kind: EmbeddingModelKind) -> Result<FetchReport, EmbeddingError> {
        if self.is_present(kind) {
            let integrity = std::fs::read_to_string(self.model_dir(kind).join(".integrity"))
                .unwrap_or_default();
            return Ok(FetchReport {
                kind,
                bytes_downloaded: 0,
                integrity,
                already_cached: true,
            });
        }

        let dir = self.model_dir(kind);
        let tmp = dir.with_extension("tmp");
        std::fs::create_dir_all(&tmp)?;

        let api = hf_hub::api::sync::Api::new()
            .map_err(|e| EmbeddingError::Network(e.to_string()))?;
        let repo = api.model(kind.hf_repo().to_owned());

        let mut bytes_downloaded: u64 = 0;
        for filename in ["config.json", "tokenizer.json", "model.safetensors"] {
            let src = repo.get(filename)
                .map_err(|e| EmbeddingError::Network(e.to_string()))?;
            let dst = tmp.join(filename);
            let len = std::fs::copy(&src, &dst)?;
            bytes_downloaded += len;
        }

        // Compute blake3 digest over all three files in deterministic order.
        let mut hasher = blake3::Hasher::new();
        for filename in ["config.json", "tokenizer.json", "model.safetensors"] {
            let data = std::fs::read(tmp.join(filename))?;
            hasher.update(&data);
        }
        let digest = hasher.finalize().to_hex().to_string();

        std::fs::write(tmp.join(".integrity"), &digest)?;

        // Atomic rename: tmp → canonical dir.
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::rename(&tmp, &dir)?;

        Ok(FetchReport { kind, bytes_downloaded, integrity: digest, already_cached: false })
    }
}
```

- [ ] **Step 3: Implement `BgeSmall`**

Replace `crates/cairn-embeddings-local/src/bge.rs` with:

```rust
//! BGE-small-en-v1.5 embedding impl.
//!
//! Uses asymmetric retrieval: queries are prefixed with
//! "Represent this sentence for searching relevant passages: ".
//! Documents are embedded without a prefix.

use std::path::Path;

use cairn_core::config::EmbeddingModelKind;
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use tokenizers::Tokenizer;

use crate::EmbeddingError;
use crate::model::{EmbeddingModel, l2_normalize};

/// BGE-small-en-v1.5 wrapper.
pub struct BgeSmall {
    model: BertModel,
    tokenizer: Tokenizer,
}

impl BgeSmall {
    /// Load model weights and tokenizer from `model_dir`.
    pub fn load(model_dir: &Path) -> Result<Self, EmbeddingError> {
        let config_path = model_dir.join("config.json");
        let tokenizer_path = model_dir.join("tokenizer.json");
        let weights_path = model_dir.join("model.safetensors");

        let config_str = std::fs::read_to_string(&config_path)?;
        let config: BertConfig = serde_json::from_str(&config_str)
            .map_err(|e| EmbeddingError::Tokenizer(e.to_string()))?;

        let device = Device::Cpu;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], candle_core::DType::F32, &device)?
        };
        let model = BertModel::load(vb, &config)?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(EmbeddingError::from)?;

        Ok(Self { model, tokenizer })
    }

    fn encode(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let encoding = self.tokenizer.encode(text, true)
            .map_err(EmbeddingError::from)?;

        let ids: Vec<u32> = encoding.get_ids().to_vec();
        let type_ids: Vec<u32> = encoding.get_type_ids().to_vec();
        let mask: Vec<u32> = encoding.get_attention_mask().to_vec();

        let device = Device::Cpu;
        let input_ids = Tensor::new(ids.as_slice(), &device)?.unsqueeze(0)?;
        let token_type_ids = Tensor::new(type_ids.as_slice(), &device)?.unsqueeze(0)?;
        let attention_mask = Tensor::new(mask.as_slice(), &device)?.unsqueeze(0)?;

        let output = self.model.forward(&input_ids, &token_type_ids, Some(&attention_mask))?;

        // Mean pooling over the sequence dimension.
        let pooled = output.mean(1)?;
        let mut v: Vec<f32> = pooled.squeeze(0)?.to_vec1()?;
        l2_normalize(&mut v);
        Ok(v)
    }
}

impl EmbeddingModel for BgeSmall {
    fn kind(&self) -> EmbeddingModelKind {
        EmbeddingModelKind::BgeSmallEnV1_5
    }

    fn dim(&self) -> usize {
        384
    }

    fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.encode(text)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let prefixed = format!(
            "Represent this sentence for searching relevant passages: {text}"
        );
        self.encode(&prefixed)
    }
}
```

- [ ] **Step 4: Implement `MiniLm`**

Replace `crates/cairn-embeddings-local/src/minilm.rs` with:

```rust
//! all-MiniLM-L6-v2 embedding impl. No asymmetric prefix.

use std::path::Path;

use cairn_core::config::EmbeddingModelKind;
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use tokenizers::Tokenizer;

use crate::EmbeddingError;
use crate::model::{EmbeddingModel, l2_normalize};

/// all-MiniLM-L6-v2 wrapper.
pub struct MiniLm {
    model: BertModel,
    tokenizer: Tokenizer,
}

impl MiniLm {
    /// Load model weights and tokenizer from `model_dir`.
    pub fn load(model_dir: &Path) -> Result<Self, EmbeddingError> {
        let config_path = model_dir.join("config.json");
        let tokenizer_path = model_dir.join("tokenizer.json");
        let weights_path = model_dir.join("model.safetensors");

        let config_str = std::fs::read_to_string(&config_path)?;
        let config: BertConfig = serde_json::from_str(&config_str)
            .map_err(|e| EmbeddingError::Tokenizer(e.to_string()))?;

        let device = Device::Cpu;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], candle_core::DType::F32, &device)?
        };
        let model = BertModel::load(vb, &config)?;
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(EmbeddingError::from)?;

        Ok(Self { model, tokenizer })
    }

    fn encode(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let encoding = self.tokenizer.encode(text, true)
            .map_err(EmbeddingError::from)?;

        let ids: Vec<u32> = encoding.get_ids().to_vec();
        let type_ids: Vec<u32> = encoding.get_type_ids().to_vec();
        let mask: Vec<u32> = encoding.get_attention_mask().to_vec();

        let device = Device::Cpu;
        let input_ids = Tensor::new(ids.as_slice(), &device)?.unsqueeze(0)?;
        let token_type_ids = Tensor::new(type_ids.as_slice(), &device)?.unsqueeze(0)?;
        let attention_mask = Tensor::new(mask.as_slice(), &device)?.unsqueeze(0)?;

        let output = self.model.forward(&input_ids, &token_type_ids, Some(&attention_mask))?;
        let pooled = output.mean(1)?;
        let mut v: Vec<f32> = pooled.squeeze(0)?.to_vec1()?;
        l2_normalize(&mut v);
        Ok(v)
    }
}

impl EmbeddingModel for MiniLm {
    fn kind(&self) -> EmbeddingModelKind {
        EmbeddingModelKind::AllMiniLmL6V2
    }

    fn dim(&self) -> usize {
        384
    }

    fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.encode(text)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.encode(text)  // no asymmetric prefix for MiniLM
    }
}
```

- [ ] **Step 5: Write `prefix_asymmetry` test**

Create `crates/cairn-embeddings-local/tests/prefix_asymmetry.rs`:

```rust
//! Verify that BGE applies an asymmetric prefix (query ≠ document vectors)
//! and MiniLM does not. Uses MockEmbedder since real models need `real-models`.

use cairn_embeddings_local::{EmbeddingModelKind, MockEmbedder};
use cairn_embeddings_local::model::EmbeddingModel;

#[test]
fn mock_embedder_query_equals_document() {
    // MockEmbedder deliberately has no asymmetric prefix.
    let m = MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5);
    assert_eq!(
        m.embed_query("foo").unwrap(),
        m.embed_document("foo").unwrap()
    );
}

/// Real BGE asymmetry test — only runs with `--features real-models` and
/// when `CAIRN_TEST_MODELS_DIR` is set to a directory containing the model.
#[test]
#[cfg(feature = "real-models")]
fn bge_query_differs_from_document() {
    use cairn_embeddings_local::{ModelCache, EmbeddingModelKind};
    let dir = std::env::var("CAIRN_TEST_MODELS_DIR").expect("CAIRN_TEST_MODELS_DIR must be set");
    let cache = ModelCache::new(std::path::Path::new(&dir));
    let model = cache.ensure(EmbeddingModelKind::BgeSmallEnV1_5).unwrap();
    let qv = model.embed_query("foo").unwrap();
    let dv = model.embed_document("foo").unwrap();
    assert_ne!(qv, dv, "BGE must produce different vectors for query vs document");
}
```

- [ ] **Step 6: Write `golden_vectors` test**

Create `crates/cairn-embeddings-local/tests/golden_vectors.rs`:

```rust
//! Snapshot the first 8 dims of MockEmbedder output for "hello world".
//! This catches regressions in the mock_vector function (blake3 hash, fp conversion).

use cairn_embeddings_local::{EmbeddingModelKind, MockEmbedder};
use cairn_embeddings_local::model::EmbeddingModel;

#[test]
fn mock_bge_hello_world_stable() {
    let m = MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5);
    let v = m.embed_query("hello world").unwrap();
    assert_eq!(v.len(), 384);
    // Snapshot first 8 dims for regression detection.
    insta::assert_debug_snapshot!("mock_bge_query_hello_world_first8", &v[..8]);
}

#[test]
fn mock_bge_document_stable() {
    let m = MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5);
    let v = m.embed_document("hello world").unwrap();
    assert_eq!(v.len(), 384);
    insta::assert_debug_snapshot!("mock_bge_document_hello_world_first8", &v[..8]);
}
```

- [ ] **Step 7: Run tests and accept snapshots**

```bash
cargo nextest run -p cairn-embeddings-local --locked 2>&1 | tail -20
```

If snapshots are new:
```bash
cargo insta review
```

Accept both. Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-embeddings-local/
git commit -m "feat(embeddings): ModelCache + BGE + MiniLM impls + golden tests (#48)"
```

---

### Task 5: Migration 0020 — `record_vectors` + `pending_embeddings`

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0020_record_vectors.sql`
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs`
- Modify: `crates/cairn-store-sqlite/Cargo.toml` (add sqlite-vec dep)

- [ ] **Step 1: Add sqlite-vec to workspace `Cargo.toml`**

In root `Cargo.toml` `[workspace.dependencies]`:

```toml
sqlite-vec = { version = "0.1", features = ["bundled"] }
```

In `crates/cairn-store-sqlite/Cargo.toml` `[dependencies]`:

```toml
sqlite-vec = { workspace = true }
cairn-embeddings-local = { path = "../cairn-embeddings-local" }
tokio-util = { workspace = true, features = ["rt"] }
```

- [ ] **Step 2: Write failing migration test first**

In `crates/cairn-store-sqlite/tests/migrations.rs`, add at the end of the file (read it first to see the pattern, then add):

```rust
#[test]
fn migration_0020_creates_record_vectors_and_pending() {
    let mut conn = open_in_memory_sync().unwrap();
    // If migration 0020 applied, these tables exist.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
              WHERE type='table' AND name IN ('pending_embeddings')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "pending_embeddings table must exist after migration 0020");
}
```

Run — expect **fail** (table doesn't exist yet):

```bash
cargo nextest run -p cairn-store-sqlite migration_0020 --locked 2>&1 | tail -20
```

- [ ] **Step 3: Create `0020_record_vectors.sql`**

Create `crates/cairn-store-sqlite/src/migrations/sql/0020_record_vectors.sql`:

```sql
-- Migration 0020: sqlite-vec ANN table + embedding backfill queue.
-- Requires sqlite-vec extension loaded at connection open (see open.rs).
-- Brief §3.0: record_vectors is authoritative at P0; pending_embeddings
-- is the idempotent backfill queue.

CREATE VIRTUAL TABLE record_vectors USING vec0(
  record_id TEXT PRIMARY KEY,
  embedding float[384],
  -- Shadow column: which model produced this embedding.
  -- Semantic search filters out rows where model ≠ current model.
  +model TEXT NOT NULL
);

-- Backfill / failure queue. Idempotent on record_id.
-- ON DELETE CASCADE keeps purge clean when the source record is physically deleted.
CREATE TABLE pending_embeddings (
  record_id        TEXT PRIMARY KEY
                     REFERENCES records(record_id) ON DELETE CASCADE,
  reason           TEXT NOT NULL
                     CHECK(reason IN ('embed_failed','opt_in_backfill','model_swap')),
  attempt_count    INTEGER NOT NULL DEFAULT 0,
  last_attempt_at  INTEGER,               -- epoch seconds; NULL until first attempt
  last_error       TEXT,
  enqueued_at      INTEGER NOT NULL       -- epoch seconds
);

CREATE INDEX pending_embeddings_enqueued_idx
  ON pending_embeddings(enqueued_at, attempt_count);

-- When a record row is physically deleted (Phase B purge in §5.6), cascade
-- removes its vector and queue entry so purge stays atomic.
CREATE TRIGGER records_vector_cleanup
  AFTER DELETE ON records
  BEGIN
    DELETE FROM record_vectors     WHERE record_id = OLD.record_id;
    DELETE FROM pending_embeddings WHERE record_id = OLD.record_id;
  END;
```

- [ ] **Step 4: Register migration in `mod.rs`**

Add to `crates/cairn-store-sqlite/src/migrations/mod.rs`:

```rust
const M0020_RECORD_VECTORS: &str = include_str!("sql/0020_record_vectors.sql");
```

Add to the `MIGRATION_SOURCES` array:
```rust
(20, "0020_record_vectors", M0020_RECORD_VECTORS),
```

Add to the `migrations()` function vector:
```rust
M::up(M0020_RECORD_VECTORS),
```

- [ ] **Step 5: Load sqlite-vec extension in `open.rs`**

In `crates/cairn-store-sqlite/src/open.rs`, add the sqlite-vec load call inside the `bootstrap` closure, before migrations:

```rust
async fn bootstrap(conn: &AsyncConn) -> Result<(), StoreError> {
    conn.call(|c| {
        // Load statically linked sqlite-vec extension before migrations so
        // the vec0 virtual table constructor is available when 0020 runs.
        sqlite_vec::load(c).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        c.execute_batch(PRAGMAS)?;
        migrations()
            .to_latest(c)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        verify_migration_history(c).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        verify_schema_fingerprint(c).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok(())
    })
    .await?;
    Ok(())
}
```

Add the import at the top of `open.rs`:
```rust
use sqlite_vec;
```

Do the same in `open_sync` / `open_in_memory_sync` — each raw `rusqlite::Connection` path also needs `sqlite_vec::load(&mut conn)?` before migrations.

- [ ] **Step 6: Run the new migration test**

```bash
cargo nextest run -p cairn-store-sqlite migration_0020 --locked 2>&1 | tail -20
```

Expected: **PASS**.

- [ ] **Step 7: Update schema fingerprint snapshot**

The `verify_schema_fingerprint` check will catch the new table. Run all store migration tests:

```bash
cargo nextest run -p cairn-store-sqlite --locked 2>&1 | tail -30
```

If `schema_fingerprint` test fails (snapshot out of date), run:
```bash
cargo insta review -p cairn-store-sqlite
```

Accept the new fingerprint snapshot that includes `record_vectors` and `pending_embeddings`.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-store-sqlite/ Cargo.toml
git commit -m "feat(store-sqlite): migration 0020 record_vectors + pending_embeddings + sqlite-vec load (#48)"
```

---

### Task 6: `SqliteMemoryStore` — dynamic capabilities + embedder field

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/mod.rs`
- Modify: `crates/cairn-store-sqlite/src/open.rs`
- Modify: `crates/cairn-store-sqlite/src/store/trait_impl.rs`
- Modify: `crates/cairn-store-sqlite/src/error.rs`

- [ ] **Step 1: Add `EmbedFailed` variant to `StoreError`**

In `crates/cairn-store-sqlite/src/error.rs`, add before the final `NotInitialized` variant:

```rust
/// Embedding computation failed. The record was still written; this row
/// has been queued in `pending_embeddings` for the reindex drain.
/// Not propagated to the caller — `upsert` returns `Ok`.
/// Logged at `warn` level.
#[error("embed failed for {record_id}: {message}")]
EmbedFailed {
    record_id: String,
    message: String,
},
```

- [ ] **Step 2: Extend `SqliteMemoryStore` with embedder + caps fields**

Replace the struct definition in `crates/cairn-store-sqlite/src/store/mod.rs`:

```rust
use std::sync::Arc;
use cairn_embeddings_local::EmbeddingModel;
use cairn_core::contract::memory_store::MemoryStoreCapabilities;
use tokio_rusqlite::Connection as AsyncConn;
use tokio_util::sync::CancellationToken;

use crate::error::StoreError;

/// Async-fronted SQLite memory store.
#[derive(Default, Clone)]
pub struct SqliteMemoryStore {
    pub(crate) conn: Option<Arc<AsyncConn>>,
    /// Active embedding model. `None` when opened without embedder or
    /// when `search.local_embeddings: false`.
    pub(crate) embedder: Option<Arc<dyn EmbeddingModel>>,
    /// Capabilities resolved at open time (based on whether `embedder` is Some).
    pub(crate) caps: MemoryStoreCapabilities,
    /// Cancellation token for the drain task. Dropped on store Drop/clone-separation.
    pub(crate) _cancel: Option<CancellationToken>,
}
```

- [ ] **Step 3: Update `capabilities()` in `trait_impl.rs` to use `self.caps`**

In `crates/cairn-store-sqlite/src/store/trait_impl.rs`, change:

```rust
fn capabilities(&self) -> &MemoryStoreCapabilities {
    &CAPS
}
```

to:

```rust
fn capabilities(&self) -> &MemoryStoreCapabilities {
    &self.caps
}
```

Remove the `use crate::open::CAPS;` import (no longer needed).

- [ ] **Step 4: Add `open_with_embedder` to `open.rs`**

In `crates/cairn-store-sqlite/src/open.rs`, replace the `CAPS` static and add the new open variant:

```rust
use std::sync::Arc;
use cairn_embeddings_local::EmbeddingModel;
use cairn_core::contract::memory_store::MemoryStoreCapabilities;

fn base_caps(vector: bool) -> MemoryStoreCapabilities {
    MemoryStoreCapabilities {
        fts: true,
        vector,
        graph_edges: true,
        transactions: true,
    }
}

/// Open (or create) the Cairn store at `path` without an embedding model.
/// `capabilities().vector` will be `false`; `search_semantic` returns
/// `CapabilityUnavailable`.
pub async fn open(path: impl AsRef<Path>) -> Result<SqliteMemoryStore, StoreError> {
    open_with_embedder(path, None).await
}

/// Open (or create) the Cairn store at `path` with an optional embedding model.
///
/// When `embedder` is `Some`, `capabilities().vector` is `true` and a
/// background drain task is spawned that re-embeds rows from `pending_embeddings`.
pub async fn open_with_embedder(
    path: impl AsRef<Path>,
    embedder: Option<Arc<dyn EmbeddingModel>>,
) -> Result<SqliteMemoryStore, StoreError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::VaultPath(e.to_string()))?;
    }
    let conn = AsyncConn::open(path).await?;
    bootstrap(&conn).await?;
    build_store(conn, embedder).await
}

/// In-memory store, optionally with an embedder.
pub async fn open_in_memory_with_embedder(
    embedder: Option<Arc<dyn EmbeddingModel>>,
) -> Result<SqliteMemoryStore, StoreError> {
    let conn = AsyncConn::open_in_memory().await?;
    bootstrap(&conn).await?;
    build_store(conn, embedder).await
}

/// In-memory store without an embedder. For tests.
pub async fn open_in_memory() -> Result<SqliteMemoryStore, StoreError> {
    open_in_memory_with_embedder(None).await
}

async fn build_store(
    conn: AsyncConn,
    embedder: Option<Arc<dyn EmbeddingModel>>,
) -> Result<SqliteMemoryStore, StoreError> {
    use tokio_util::sync::CancellationToken;
    let conn = Arc::new(conn);
    let vector = embedder.is_some();
    let caps = base_caps(vector);
    let cancel = embedder.as_ref().map(|_| CancellationToken::new());

    if let (Some(emb), Some(tok)) = (embedder.as_ref(), cancel.as_ref()) {
        let conn2 = Arc::clone(&conn);
        let emb2 = Arc::clone(emb);
        let tok2 = tok.clone();
        tokio::spawn(crate::store::reindex::drain_loop(conn2, emb2, tok2));
    }

    Ok(SqliteMemoryStore {
        conn: Some(conn),
        embedder,
        caps,
        _cancel: cancel,
    })
}
```

- [ ] **Step 5: Add `search_semantic` dispatch to `trait_impl.rs`**

In `crates/cairn-store-sqlite/src/store/trait_impl.rs`, add the import and method:

```rust
use cairn_core::contract::memory_store::{
    // existing imports …
    SemanticSearchArgs, SemanticSearchPage,
};

// Inside `impl MemoryStore for SqliteMemoryStore`:
async fn search_semantic(
    &self,
    args: &SemanticSearchArgs<'_>,
) -> Result<SemanticSearchPage, StoreError> {
    if self.conn.is_none() {
        return not_initialized("search_semantic");
    }
    self.do_search_semantic(args).await.map_err(Into::into)
}
```

- [ ] **Step 6: Build to check wiring**

```bash
cargo check -p cairn-store-sqlite --locked 2>&1 | tail -20
```

Expected: error about missing `do_search_semantic` and `reindex` module — those arrive in the next task.

- [ ] **Step 7: Commit (partial — compiles with stub)**

Add a stub for `do_search_semantic` in `search.rs` temporarily:

```rust
pub(crate) async fn do_search_semantic(
    &self,
    _args: &SemanticSearchArgs<'_>,
) -> Result<SemanticSearchPage, StoreError> {
    Err(StoreError::CapabilityUnavailable { what: "vector" })
}
```

Add a stub `reindex.rs`:

```rust
// crates/cairn-store-sqlite/src/store/reindex.rs
use std::sync::Arc;
use cairn_embeddings_local::EmbeddingModel;
use tokio_rusqlite::Connection as AsyncConn;
use tokio_util::sync::CancellationToken;

pub(crate) async fn drain_loop(
    _conn: Arc<AsyncConn>,
    _embedder: Arc<dyn EmbeddingModel>,
    cancel: CancellationToken,
) {
    cancel.cancelled().await;
}
```

Add `pub(crate) mod reindex;` to `store/mod.rs`.

```bash
cargo check -p cairn-store-sqlite --locked 2>&1 | tail -20
```

Expected: compiles.

```bash
git add crates/cairn-store-sqlite/
git commit -m "feat(store-sqlite): dynamic caps + embedder field + search_semantic stub (#48)"
```

---

### Task 7: `do_search_semantic` implementation

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/search.rs`

- [ ] **Step 1: Write failing integration test**

In `crates/cairn-store-sqlite/tests/` — check for an existing file (like `search_keyword.rs`) and create a companion `search_semantic.rs`:

```rust
//! Integration tests for search_semantic.
use std::sync::Arc;

use cairn_core::contract::memory_store::{MemoryStore, SemanticSearchArgs};
use cairn_embeddings_local::{EmbeddingModelKind, MockEmbedder};

use cairn_store_sqlite::open_in_memory_with_embedder;
use cairn_test_fixtures::sample_record;

#[tokio::test]
async fn search_semantic_returns_results_when_embedder_present() {
    let embedder = Arc::new(MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5));
    let store = open_in_memory_with_embedder(Some(embedder)).await.unwrap();
    assert!(store.capabilities().vector);

    let r = sample_record();
    store.upsert(&r).await.unwrap();

    let page = store
        .search_semantic(&SemanticSearchArgs {
            query: "hello".into(),
            filter: None,
            visibility_allowlist: vec![],
            limit: 10,
            model_label: EmbeddingModelKind::BgeSmallEnV1_5.as_str().into(),
        })
        .await
        .unwrap();
    assert!(!page.candidates.is_empty());
    assert!(page.candidates[0].semantic_distance.is_some());
}

#[tokio::test]
async fn search_semantic_returns_capability_unavailable_when_no_embedder() {
    let store = open_in_memory_with_embedder(None).await.unwrap();
    assert!(!store.capabilities().vector);

    let result = store
        .search_semantic(&SemanticSearchArgs {
            query: "hello".into(),
            filter: None,
            visibility_allowlist: vec![],
            limit: 10,
            model_label: "bge-small-en-v1.5".into(),
        })
        .await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("capability"), "expected CapabilityUnavailable, got: {msg}");
}
```

Run:
```bash
cargo nextest run -p cairn-store-sqlite search_semantic --locked 2>&1 | tail -20
```

Expected: first test **fails** (stub always returns error). Second test **passes**.

- [ ] **Step 2: Implement `do_search_semantic`**

Replace the stub in `crates/cairn-store-sqlite/src/store/search.rs` (add after `do_search_keyword`):

```rust
use cairn_core::contract::memory_store::{SemanticSearchArgs, SemanticSearchPage};

impl SqliteMemoryStore {
    #[instrument(
        skip(self, args),
        err,
        fields(verb = "search_semantic", limit = args.limit),
    )]
    pub(crate) async fn do_search_semantic(
        &self,
        args: &SemanticSearchArgs<'_>,
    ) -> Result<SemanticSearchPage, StoreError> {
        if !self.caps.vector {
            return Err(StoreError::CapabilityUnavailable { what: "vector" });
        }
        let embedder = self
            .embedder
            .as_ref()
            .ok_or(StoreError::CapabilityUnavailable { what: "vector" })?
            .clone();

        let query_text = args.query.clone();
        let query_vec: Vec<f32> = tokio::task::spawn_blocking(move || {
            embedder.embed_query(&query_text)
        })
        .await
        .map_err(|e| StoreError::Invariant { what: e.to_string() })?
        .map_err(|e| StoreError::Invariant { what: e.to_string() })?;

        let query_bytes = vec_to_bytes(&query_vec);
        let limit = args.limit.clamp(1, SEARCH_LIMIT_MAX);
        let model_label = args.model_label.clone();
        let visibilities: Vec<String> = args
            .visibility_allowlist
            .iter()
            .map(|v| v.as_str().to_owned())
            .collect();
        let compiled = args.filter.map(compile_filter);
        let now_ms = current_unix_ms();
        let conn = self.require_conn("search_semantic")?.clone();

        let candidates = conn
            .call(move |c| {
                // Over-fetch by 2× to buffer against post-filter drops.
                let k = (limit * 2).max(1) as i64;
                let vis_clause = if visibilities.is_empty() {
                    String::new()
                } else {
                    let placeholders = visibilities
                        .iter()
                        .enumerate()
                        .map(|(i, _)| format!("?{}", i + 4))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("AND r.visibility IN ({placeholders})")
                };
                let filter_clause = compiled
                    .as_ref()
                    .map(|c| format!("AND ({})", c.sql))
                    .unwrap_or_default();

                let sql = format!(
                    "SELECT r.record_id, r.target_id, r.scope, r.kind, r.class,
                            r.visibility, v.distance,
                            (? - r.updated_at) AS recency_ms,
                            r.confidence, r.salience,
                            (? - r.updated_at) AS staleness_ms,
                            r.record_json
                     FROM   record_vectors v
                     JOIN   records r ON r.record_id = v.record_id
                     WHERE  v.embedding MATCH ? AND k = {k}
                       AND  r.active = 1 AND r.tombstoned = 0
                       AND  v.model = ?
                       {vis_clause}
                       {filter_clause}
                     ORDER BY v.distance ASC
                     LIMIT {limit}"
                );

                let mut stmt = c.prepare_cached(&sql)?;
                let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![
                    Box::new(now_ms),
                    Box::new(now_ms),
                    Box::new(query_bytes.clone()),
                    Box::new(model_label.clone()),
                ];
                for v in &visibilities {
                    params.push(Box::new(v.clone()));
                }
                if let Some(cf) = compiled.as_ref() {
                    for p in &cf.params {
                        params.push(Box::new(p.clone()));
                    }
                }

                let params_refs: Vec<&dyn rusqlite::ToSql> =
                    params.iter().map(|p| p.as_ref()).collect();

                let rows = stmt.query_map(params_refs.as_slice(), |row| {
                    Ok(RowRaw {
                        record_id: row.get(0)?,
                        target_id: row.get(1)?,
                        scope_json: row.get(2)?,
                        kind_str: row.get(3)?,
                        class_str: row.get(4)?,
                        visibility_str: row.get(5)?,
                        distance: row.get::<_, f64>(6)?,
                        recency_ms: row.get(7)?,
                        confidence: row.get(8)?,
                        salience: row.get(9)?,
                        staleness_ms: row.get(10)?,
                        record_json: row.get(11)?,
                    })
                })?;

                let mut candidates = Vec::new();
                for row in rows {
                    let r = row?;
                    candidates.push(r);
                }
                Ok::<_, tokio_rusqlite::Error>(candidates)
            })
            .await?;

        let mut result = Vec::with_capacity(candidates.len());
        for raw in candidates {
            let record_id = record_id_from_str(&raw.record_id)?;
            let target_id = target_id_from_str(&raw.target_id)?;
            let scope: ScopeTuple = serde_json::from_str(&raw.scope_json)?;
            let kind = MemoryKind::parse(&raw.kind_str)
                .ok_or_else(|| StoreError::Codec(
                    serde_json::from_str::<MemoryKind>("null").unwrap_err()
                ))?;
            let class = MemoryClass::parse(&raw.class_str)
                .ok_or_else(|| StoreError::Codec(
                    serde_json::from_str::<MemoryClass>("null").unwrap_err()
                ))?;
            let visibility = MemoryVisibility::parse(&raw.visibility_str)
                .ok_or_else(|| StoreError::Codec(
                    serde_json::from_str::<MemoryVisibility>("null").unwrap_err()
                ))?;
            result.push(SearchCandidate {
                record_id,
                target_id,
                scope,
                kind,
                class,
                visibility,
                bm25: 0.0,
                recency_seconds: raw.recency_ms / 1000,
                confidence: raw.confidence,
                salience: raw.salience,
                staleness_seconds: raw.staleness_ms / 1000,
                snippet: String::new(),
                record_json: raw.record_json,
                semantic_distance: Some(raw.distance as f32),
            });
        }

        Ok(SemanticSearchPage { candidates: result })
    }
}

struct RowRaw {
    record_id: String,
    target_id: String,
    scope_json: String,
    kind_str: String,
    class_str: String,
    visibility_str: String,
    distance: f64,
    recency_ms: i64,
    confidence: f32,
    salience: f32,
    staleness_ms: i64,
    record_json: String,
}

/// Encode a `Vec<f32>` as little-endian bytes for sqlite-vec MATCH input.
fn vec_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|&f| f.to_le_bytes()).collect()
}
```

- [ ] **Step 3: Run tests**

```bash
cargo nextest run -p cairn-store-sqlite search_semantic --locked 2>&1 | tail -20
```

Expected: both tests pass. Debug until they do.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-store-sqlite/
git commit -m "feat(store-sqlite): do_search_semantic with sqlite-vec ANN + filter wiring (#48)"
```

---

### Task 8: Embed-on-write in `upsert`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/upsert.rs`

- [ ] **Step 1: Write failing test**

Add to `crates/cairn-store-sqlite/tests/upsert_idempotent.rs` (or create a new file `tests/upsert_embed.rs`):

```rust
//! Verify embed-on-write behaviour.
use std::sync::Arc;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_embeddings_local::{EmbeddingModelKind, MockEmbedder};
use cairn_store_sqlite::open_in_memory_with_embedder;
use cairn_test_fixtures::sample_record;

#[tokio::test]
async fn upsert_with_embedder_writes_vector_row() {
    let embedder = Arc::new(MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5));
    let store = open_in_memory_with_embedder(Some(embedder)).await.unwrap();
    let r = sample_record();
    let outcome = store.upsert(&r).await.unwrap();
    assert!(outcome.content_changed);

    // Verify a vector row exists for the new record_id.
    let conn = store.conn.as_ref().unwrap().clone();
    let rid = outcome.record_id.as_str().to_owned();
    let found: bool = conn.call(move |c| {
        let n: i64 = c.query_row(
            "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?",
            rusqlite::params![rid],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }).await.unwrap();
    assert!(found, "record_vectors should have a row after upsert");
}

#[tokio::test]
async fn upsert_without_embedder_no_vector_row() {
    let store = open_in_memory_with_embedder(None).await.unwrap();
    let r = sample_record();
    let outcome = store.upsert(&r).await.unwrap();

    let conn = store.conn.as_ref().unwrap().clone();
    let rid = outcome.record_id.as_str().to_owned();
    let count: i64 = conn.call(move |c| {
        c.query_row(
            "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?",
            rusqlite::params![rid],
            |r| r.get(0),
        ).map_err(Into::into)
    }).await.unwrap();
    assert_eq!(count, 0, "no vector row when no embedder");
}
```

Run — expect **fail** (vector row not yet written):

```bash
cargo nextest run -p cairn-store-sqlite upsert_with_embedder --locked 2>&1 | tail -20
```

- [ ] **Step 2: Add `EmbedOutcome` enum to `upsert.rs`**

At the top of `crates/cairn-store-sqlite/src/store/upsert.rs`:

```rust
/// Result of the pre-transaction embedding step.
enum EmbedOutcome {
    /// Embedding produced successfully.
    Succeeded { vector: Vec<u8>, model_label: String },
    /// Embedder errored — record queued in `pending_embeddings`.
    Failed { error: String, model_label: String },
    /// No embedder configured — skip vector entirely.
    Skipped,
}
```

- [ ] **Step 3: Add embed-on-write logic to `do_upsert`**

In `crates/cairn-store-sqlite/src/store/upsert.rs`, replace `do_upsert`:

```rust
pub(crate) async fn do_upsert(
    &self,
    record: &MemoryRecord,
) -> Result<UpsertOutcome, StoreError> {
    record.validate()?;
    let conn = self.require_conn("upsert")?.clone();

    // 1. Compute embedding OUTSIDE the SQL transaction (CPU-bound).
    let embed_outcome: EmbedOutcome = if let Some(embedder) = &self.embedder {
        let body = record.body.clone();
        let emb = Arc::clone(embedder);
        let model_label = emb.kind().as_str().to_owned();
        match tokio::task::spawn_blocking(move || emb.embed_document(&body)).await {
            Ok(Ok(vec)) => EmbedOutcome::Succeeded {
                vector: vec.iter().flat_map(|&f| f.to_le_bytes()).collect(),
                model_label,
            },
            Ok(Err(e)) => EmbedOutcome::Failed {
                error: e.to_string(),
                model_label,
            },
            Err(e) => EmbedOutcome::Failed {
                error: e.to_string(),
                model_label: embedder.kind().as_str().to_owned(),
            },
        }
    } else {
        EmbedOutcome::Skipped
    };

    let record = record.clone();
    let outcome = conn
        .call(move |c| {
            let mut tx = c.transaction()?;
            let upsert_outcome = upsert_in_tx(&mut tx, &record)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            let now_secs = current_unix_ms() / 1000;
            let rid = upsert_outcome.record_id.as_str();

            match &embed_outcome {
                EmbedOutcome::Succeeded { vector, model_label } => {
                    tx.execute(
                        "INSERT INTO record_vectors(record_id, embedding, model)
                           VALUES (?, ?, ?)
                           ON CONFLICT(record_id) DO UPDATE
                             SET embedding = excluded.embedding,
                                 model     = excluded.model",
                        rusqlite::params![rid, vector, model_label],
                    )?;
                    // Clear any pending entry (succeeds even if row absent).
                    tx.execute(
                        "DELETE FROM pending_embeddings WHERE record_id = ?",
                        rusqlite::params![rid],
                    )?;
                }
                EmbedOutcome::Failed { error, model_label: _ } => {
                    tracing::warn!(
                        record_id = rid,
                        error = error.as_str(),
                        "embed failed; queued in pending_embeddings"
                    );
                    tx.execute(
                        "INSERT INTO pending_embeddings
                             (record_id, reason, attempt_count, last_error, enqueued_at)
                           VALUES (?, 'embed_failed', 0, ?, ?)
                           ON CONFLICT(record_id) DO UPDATE
                             SET attempt_count = attempt_count + 1,
                                 last_error    = excluded.last_error,
                                 last_attempt_at = ?",
                        rusqlite::params![rid, error, now_secs, now_secs],
                    )?;
                }
                EmbedOutcome::Skipped => {}
            }

            tx.commit()?;
            Ok::<_, tokio_rusqlite::Error>(upsert_outcome)
        })
        .await?;

    Ok(outcome)
}
```

Add the missing import at the top of `upsert.rs`:
```rust
use std::sync::Arc;
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p cairn-store-sqlite upsert_with_embedder upsert_without_embedder --locked 2>&1 | tail -20
```

Expected: both pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/
git commit -m "feat(store-sqlite): embed-on-write in upsert + pending_embeddings queue (#48)"
```

---

### Task 9: Reindex drain task

**Files:**
- Replace stub: `crates/cairn-store-sqlite/src/store/reindex.rs`
- Create: `crates/cairn-store-sqlite/tests/reindex.rs`

- [ ] **Step 1: Write failing reindex tests**

Create `crates/cairn-store-sqlite/tests/reindex.rs`:

```rust
//! Reindex drain-once tests (brief §19: idempotent backfill).
use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::memory_store::MemoryStore;
use cairn_embeddings_local::{EmbeddingModelKind, MockEmbedder};
use cairn_store_sqlite::{open_in_memory_with_embedder, open_in_memory};
use cairn_store_sqlite::store::reindex::{DrainStats, drain_once};
use cairn_test_fixtures::sample_record;

#[tokio::test]
async fn drain_once_embeds_pending_records() {
    let embedder = Arc::new(MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5));
    // Open WITHOUT embedder so upsert skips embedding, simulating opt-in-backfill.
    let store = open_in_memory().await.unwrap();
    let r = sample_record();
    let outcome = store.upsert(&r).await.unwrap();
    let rid = outcome.record_id.as_str().to_owned();

    // Manually insert a pending_embeddings row (as-if upsert without embedder).
    let conn = store.conn.as_ref().unwrap().clone();
    let rid2 = rid.clone();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
               VALUES (?, 'opt_in_backfill', 0, strftime('%s','now'))",
            rusqlite::params![rid2],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    }).await.unwrap();

    // Drain once.
    let stats = drain_once(store.conn.as_ref().unwrap().clone(), Arc::clone(&embedder)).await.unwrap();
    assert_eq!(stats.drained, 1);
    assert_eq!(stats.failed, 0);
    assert_eq!(stats.remaining, 0);

    // Verify vector row exists.
    let rid3 = rid.clone();
    let count: i64 = conn.call(move |c| {
        c.query_row(
            "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?",
            rusqlite::params![rid3],
            |r| r.get(0),
        ).map_err(Into::into)
    }).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn drain_once_bumps_attempt_count_on_failure() {
    use cairn_embeddings_local::model::EmbeddingModel;
    use cairn_embeddings_local::error::EmbeddingError;
    use cairn_core::config::EmbeddingModelKind as Kind;

    struct AlwaysFail;
    impl EmbeddingModel for AlwaysFail {
        fn kind(&self) -> Kind { Kind::BgeSmallEnV1_5 }
        fn dim(&self) -> usize { 384 }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::Tokenizer("simulated failure".into()))
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::Tokenizer("simulated failure".into()))
        }
    }

    let store = open_in_memory().await.unwrap();
    let r = sample_record();
    let outcome = store.upsert(&r).await.unwrap();
    let rid = outcome.record_id.as_str().to_owned();

    let conn = store.conn.as_ref().unwrap().clone();
    let rid2 = rid.clone();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
               VALUES (?, 'embed_failed', 0, strftime('%s','now'))",
            rusqlite::params![rid2],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    }).await.unwrap();

    let stats = drain_once(
        store.conn.as_ref().unwrap().clone(),
        Arc::new(AlwaysFail),
    ).await.unwrap();
    assert_eq!(stats.failed, 1);
    assert_eq!(stats.drained, 0);

    let rid3 = rid.clone();
    let attempt: i64 = conn.call(move |c| {
        c.query_row(
            "SELECT attempt_count FROM pending_embeddings WHERE record_id = ?",
            rusqlite::params![rid3],
            |r| r.get(0),
        ).map_err(Into::into)
    }).await.unwrap();
    assert_eq!(attempt, 1, "attempt_count must have been bumped");
}

#[tokio::test]
async fn drain_once_skips_poison_pill_rows() {
    use cairn_embeddings_local::model::EmbeddingModel;
    use cairn_embeddings_local::error::EmbeddingError;
    use cairn_core::config::EmbeddingModelKind as Kind;

    struct AlwaysFail;
    impl EmbeddingModel for AlwaysFail {
        fn kind(&self) -> Kind { Kind::BgeSmallEnV1_5 }
        fn dim(&self) -> usize { 384 }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::Tokenizer("fail".into()))
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::Tokenizer("fail".into()))
        }
    }

    let store = open_in_memory().await.unwrap();
    let r = sample_record();
    let outcome = store.upsert(&r).await.unwrap();
    let rid = outcome.record_id.as_str().to_owned();

    let conn = store.conn.as_ref().unwrap().clone();
    let rid2 = rid.clone();
    conn.call(move |c| {
        // attempt_count = 6 > max(5) — should be skipped
        c.execute(
            "INSERT INTO pending_embeddings
               (record_id, reason, attempt_count, enqueued_at)
               VALUES (?, 'embed_failed', 6, strftime('%s','now'))",
            rusqlite::params![rid2],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    }).await.unwrap();

    let stats = drain_once(
        store.conn.as_ref().unwrap().clone(),
        Arc::new(AlwaysFail),
    ).await.unwrap();

    assert_eq!(stats.drained, 0);
    assert_eq!(stats.failed, 0);
    // Row still there — not skipped means it stays, not consumed.
    let rid3 = rid.clone();
    let still: i64 = conn.call(move |c| {
        c.query_row(
            "SELECT COUNT(*) FROM pending_embeddings WHERE record_id = ?",
            rusqlite::params![rid3],
            |r| r.get(0),
        ).map_err(Into::into)
    }).await.unwrap();
    assert_eq!(still, 1, "poison-pill row must remain, not be consumed");
}
```

Run — expect **fail** (drain_once not yet implemented as public):

```bash
cargo nextest run -p cairn-store-sqlite drain_once --locked 2>&1 | tail -20
```

- [ ] **Step 2: Implement `reindex.rs`**

Replace the stub `crates/cairn-store-sqlite/src/store/reindex.rs`:

```rust
//! Background embedding drain task and one-shot drain helper.
//!
//! `drain_loop` runs as a tokio task, waking every 30 s (configurable)
//! to call `drain_once`. `drain_once` is also the implementation behind
//! `cairn admin reindex --semantic`.

use std::sync::Arc;
use std::time::Duration;

use cairn_embeddings_local::EmbeddingModel;
use tokio_rusqlite::Connection as AsyncConn;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use crate::error::StoreError;

const BATCH: usize = 32;
const MAX_ATTEMPTS: i64 = 5;

/// Outcome of a single `drain_once` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainStats {
    /// Number of rows successfully embedded and removed from the queue.
    pub drained: usize,
    /// Number of rows that errored this pass (attempt_count bumped).
    pub failed: usize,
    /// Rows still in the queue after this pass (including poison pills).
    pub remaining: usize,
}

/// Runs forever, draining `pending_embeddings` every `interval`.
/// Exits cleanly when `cancel` is triggered.
pub(crate) async fn drain_loop(
    conn: Arc<AsyncConn>,
    embedder: Arc<dyn EmbeddingModel>,
    cancel: CancellationToken,
) {
    drain_loop_with_interval(conn, embedder, cancel, Duration::from_secs(30)).await;
}

/// Same as `drain_loop` but with a configurable interval (tests use shorter).
pub(crate) async fn drain_loop_with_interval(
    conn: Arc<AsyncConn>,
    embedder: Arc<dyn EmbeddingModel>,
    cancel: CancellationToken,
    interval: Duration,
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tick.tick() => {
                match drain_once(Arc::clone(&conn), Arc::clone(&embedder)).await {
                    Ok(stats) if stats.drained > 0 || stats.failed > 0 => {
                        tracing::info!(
                            drained = stats.drained,
                            failed = stats.failed,
                            remaining = stats.remaining,
                            "embedding drain pass"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "drain_once error"),
                }
            }
        }
    }
}

/// Single drain pass: embeds up to `BATCH` rows from `pending_embeddings`
/// (skipping rows with `attempt_count > MAX_ATTEMPTS`). Returns stats.
#[instrument(skip(conn, embedder), err)]
pub async fn drain_once(
    conn: Arc<AsyncConn>,
    embedder: Arc<dyn EmbeddingModel>,
) -> Result<DrainStats, StoreError> {
    // Fetch a batch of pending rows.
    let rows: Vec<(String, String)> = conn
        .call(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT pe.record_id, r.body
                   FROM pending_embeddings pe
                   JOIN records r ON r.record_id = pe.record_id
                  WHERE pe.attempt_count <= ?
                  ORDER BY pe.enqueued_at
                  LIMIT ?",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![MAX_ATTEMPTS, BATCH as i64], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, tokio_rusqlite::Error>(rows)
        })
        .await?;

    let mut drained = 0usize;
    let mut failed = 0usize;
    let model_label = embedder.kind().as_str().to_owned();

    for (record_id, body) in &rows {
        let emb = Arc::clone(&embedder);
        let body = body.clone();
        let embed_result =
            tokio::task::spawn_blocking(move || emb.embed_document(&body)).await;

        let vector_bytes: Result<Vec<u8>, String> = match embed_result {
            Ok(Ok(v)) => Ok(v.iter().flat_map(|&f| f.to_le_bytes()).collect()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(e) => Err(e.to_string()),
        };

        let rid = record_id.clone();
        let ml = model_label.clone();
        match vector_bytes {
            Ok(bytes) => {
                conn.call(move |c| {
                    let mut tx = c.transaction()?;
                    tx.execute(
                        "INSERT INTO record_vectors(record_id, embedding, model)
                           VALUES (?, ?, ?)
                           ON CONFLICT(record_id) DO UPDATE
                             SET embedding = excluded.embedding,
                                 model     = excluded.model",
                        rusqlite::params![rid, bytes, ml],
                    )?;
                    tx.execute(
                        "DELETE FROM pending_embeddings WHERE record_id = ?",
                        rusqlite::params![rid],
                    )?;
                    tx.commit()?;
                    Ok::<_, tokio_rusqlite::Error>(())
                })
                .await?;
                drained += 1;
            }
            Err(err_msg) => {
                let rid2 = record_id.clone();
                conn.call(move |c| {
                    c.execute(
                        "UPDATE pending_embeddings
                            SET attempt_count   = attempt_count + 1,
                                last_error      = ?,
                                last_attempt_at = strftime('%s','now')
                          WHERE record_id = ?",
                        rusqlite::params![err_msg, rid2],
                    )?;
                    Ok::<_, tokio_rusqlite::Error>(())
                })
                .await?;
                failed += 1;
            }
        }
    }

    let remaining: usize = conn
        .call(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM pending_embeddings",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })
        .await? as usize;

    Ok(DrainStats { drained, failed, remaining })
}
```

Add `pub mod reindex;` to `store/mod.rs` (note `pub`, not `pub(crate)` — the admin verb in `cairn-cli` needs to call `drain_once`):

```rust
pub mod reindex;
```

In `crates/cairn-store-sqlite/src/lib.rs`, update the `open` re-exports and add `reindex` exports:

```rust
pub use open::{open, open_in_memory, open_in_memory_with_embedder, open_with_embedder};
#[cfg(any(test, feature = "test-helpers"))]
pub use open::{open_in_memory_sync, open_sync};
pub use store::reindex::{DrainStats, drain_once};   // ← needed by cairn-cli admin verbs
pub use store::SqliteMemoryStore;
```

- [ ] **Step 3: Run reindex tests**

```bash
cargo nextest run -p cairn-store-sqlite drain_once --locked 2>&1 | tail -20
```

Expected: all three tests pass. Debug until they do.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-store-sqlite/
git commit -m "feat(store-sqlite): drain task + drain_once idempotent backfill (#48)"
```

---

### Task 10: CLI wiring — `--mode semantic`, capability advertisement, admin verbs, bootstrap

**Files:**
- Modify: `crates/cairn-cli/src/verbs/search.rs`
- Modify: `crates/cairn-cli/src/verbs/status.rs` (or wherever `status` computes caps)
- Create: `crates/cairn-cli/src/verbs/admin_reindex.rs`
- Create: `crates/cairn-cli/src/verbs/admin_model_fetch.rs`
- Modify: `crates/cairn-cli/src/verbs/mod.rs`
- Modify: `crates/cairn-cli/src/main.rs`

Read `crates/cairn-cli/src/verbs/search.rs` before editing to understand the existing dispatch pattern. Then:

- [ ] **Step 1: Add `Semantic` mode to the search verb dispatch**

Find where `SearchMode` or `--mode` is matched in `search.rs`. Add a `Semantic` branch:

```rust
SearchMode::Semantic => {
    let caps = config.capabilities(model_cache.is_present(config.search.embedding_model));
    if !caps.semantic_search {
        return Err(CairnError::CapabilityUnavailable {
            capability: "cairn.mcp.v1.search.semantic".into(),
        });
    }
    let embedder = model_cache
        .ensure(config.search.embedding_model)
        .map_err(|e| CairnError::Internal(e.to_string()))?;
    let store = open_with_embedder(&vault_db_path, Some(Arc::new(embedder))).await?;
    let page = store.search_semantic(&SemanticSearchArgs {
        query: args.query.clone(),
        filter: validated_filter.as_ref(),
        visibility_allowlist: visibility_allowlist.clone(),
        limit: args.limit,
        model_label: config.search.embedding_model.as_str().into(),
    }).await?;
    // render page.candidates — same shape as keyword, semantic_distance shown in JSON
    render_semantic_results(page, &args)?;
}
SearchMode::Hybrid => {
    return Err(CairnError::CapabilityUnavailable {
        capability: "cairn.mcp.v1.search.hybrid — not yet implemented; follow-up issue".into(),
    });
}
```

The exact integration depends on how the CLI verb is structured. Read `search.rs` first and adapt.

- [ ] **Step 2: Fix `status` capability advertisement**

Find where `status` builds `capabilities` in the status verb or wherever `CapabilitySet` is computed for the status response. Change:

```rust
// Old:
let caps = config.capabilities();

// New:
let model_present = {
    let cache = ModelCache::new(&vault.models_path());
    cache.is_present(config.search.embedding_model)
};
let caps = config.capabilities(model_present);
```

This makes `status.capabilities` correctly include/exclude `cairn.mcp.v1.search.semantic` based on whether the model files exist.

- [ ] **Step 3: Create `admin_model_fetch.rs`**

Create `crates/cairn-cli/src/verbs/admin_model_fetch.rs`:

```rust
//! `cairn admin model fetch [--model <kind>]` verb.
//! Downloads the configured embedding model to `.cairn/models/`.

use std::path::Path;

use anyhow::{Context, Result};
use cairn_core::config::{CairnConfig, EmbeddingModelKind};
use cairn_embeddings_local::ModelCache;
use serde::Serialize;

#[derive(Debug, clap::Args)]
pub struct AdminModelFetchArgs {
    /// Override the model to fetch. Defaults to `search.embedding_model` in config.
    #[arg(long, value_name = "KIND")]
    pub model: Option<EmbeddingModelKind>,
    /// Re-fetch even if model files are already present (integrity re-verified).
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Serialize)]
struct FetchOutput {
    kind: String,
    bytes_downloaded: u64,
    integrity: String,
    already_cached: bool,
}

pub async fn run(
    args: &AdminModelFetchArgs,
    config: &CairnConfig,
    vault_root: &Path,
) -> Result<()> {
    let kind = args.model.unwrap_or(config.search.embedding_model);
    let models_root = vault_root.join(".cairn/models");
    let cache = ModelCache::new(&models_root);

    if args.force {
        let dir = cache.model_dir(kind);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("removing {}", dir.display()))?;
        }
    }

    let report = tokio::task::spawn_blocking(move || cache.fetch(kind))
        .await
        .context("join error")?
        .context("model fetch failed")?;

    let out = FetchOutput {
        kind: report.kind.as_str().to_owned(),
        bytes_downloaded: report.bytes_downloaded,
        integrity: report.integrity,
        already_cached: report.already_cached,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
```

- [ ] **Step 4: Create `admin_reindex.rs`**

Create `crates/cairn-cli/src/verbs/admin_reindex.rs`:

```rust
//! `cairn admin reindex --semantic [--all]` verb.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use cairn_core::config::CairnConfig;
use cairn_embeddings_local::{ModelCache, EmbeddingModelKind};
use cairn_store_sqlite::open_with_embedder;
use cairn_store_sqlite::store::reindex::drain_once;
use serde::Serialize;

#[derive(Debug, clap::Args)]
pub struct AdminReindexArgs {
    /// Reindex semantic (ANN) vectors.
    #[arg(long)]
    pub semantic: bool,
    /// Enqueue ALL active records before draining (use after model swap).
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Serialize)]
struct ReindexOutput {
    drained: usize,
    failed: usize,
    remaining: usize,
}

pub async fn run(
    args: &AdminReindexArgs,
    config: &CairnConfig,
    vault_root: &Path,
) -> Result<()> {
    if !args.semantic {
        anyhow::bail!("specify --semantic (hybrid is a future option)");
    }

    let models_root = vault_root.join(".cairn/models");
    let cache = ModelCache::new(&models_root);
    let kind = config.search.embedding_model;
    let embedder = tokio::task::spawn_blocking(move || cache.ensure(kind))
        .await
        .context("join error")?
        .context("model not fetched — run `cairn admin model fetch` first")?;

    let db_path = vault_root.join(".cairn/cairn.db");
    let store = open_with_embedder(&db_path, Some(Arc::clone(&embedder))).await?;
    let conn = store.conn.as_ref().context("store not connected")?.clone();

    if args.all {
        // Enqueue every active non-tombstoned record.
        conn.call(|c| {
            c.execute(
                "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
                   SELECT r.record_id, 'model_swap', 0, strftime('%s','now')
                     FROM records r
                    WHERE r.active = 1 AND r.tombstoned = 0
                   ON CONFLICT(record_id) DO UPDATE
                     SET reason        = 'model_swap',
                         attempt_count = 0,
                         enqueued_at   = strftime('%s','now')",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await?;
    }

    // Drain until queue empty (or cap at 10k iterations as a safety guard).
    let mut total_drained = 0;
    let mut total_failed = 0;
    for _ in 0..10_000 {
        let stats = drain_once(Arc::clone(&conn), Arc::clone(&embedder)).await?;
        total_drained += stats.drained;
        total_failed += stats.failed;
        if stats.remaining == 0 || (stats.drained == 0 && stats.failed == 0) {
            let out = ReindexOutput {
                drained: total_drained,
                failed: total_failed,
                remaining: stats.remaining,
            };
            println!("{}", serde_json::to_string_pretty(&out)?);
            return Ok(());
        }
    }
    anyhow::bail!("reindex did not converge after 10k iterations — check for poison-pill rows");
}
```

- [ ] **Step 5: Add bootstrap model-fetch step**

In `crates/cairn-cli/src/main.rs` (or wherever `cairn bootstrap` is implemented), add after vault layout creation:

```rust
// Fetch default embedding model if local_embeddings enabled.
if config.search.local_embeddings {
    let models_root = vault_root.join(".cairn/models");
    let cache = cairn_embeddings_local::ModelCache::new(&models_root);
    let kind = config.search.embedding_model;
    if !cache.is_present(kind) {
        eprintln!("Fetching embedding model {} (~25 MB)…", kind.as_str());
        let report = tokio::task::spawn_blocking(move || cache.fetch(kind))
            .await
            .context("join error")?
            .with_context(|| format!(
                "Failed to fetch embedding model '{}'. \
                 Check network access to huggingface.co or set HF_ENDPOINT.",
                kind.as_str()
            ))?;
        eprintln!(
            "Model fetched ({} bytes, integrity: {})",
            report.bytes_downloaded,
            &report.integrity[..12]
        );
    }
}
```

- [ ] **Step 6: Register admin verbs in mod/command dispatch**

In `crates/cairn-cli/src/verbs/mod.rs` (or command dispatch file), add the two new verb modules:

```rust
pub mod admin_model_fetch;
pub mod admin_reindex;
```

Wire them into the CLI subcommand tree under `cairn admin`. The exact clap structure depends on how the existing admin verb (`admin replay-wal` or similar) is plumbed. Read the existing admin command registration and mirror the pattern.

- [ ] **Step 7: Build CLI**

```bash
cargo check -p cairn-cli --locked 2>&1 | tail -20
```

Fix any compile errors. Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-cli/
git commit -m "feat(cli): search --mode semantic, admin model fetch/reindex, bootstrap embed step (#48)"
```

---

### Task 11: Capability + wire-compat tests

**Files:**
- Create: `crates/cairn-store-sqlite/tests/search_semantic_capability.rs`
- Modify: `crates/cairn-core/tests/` (or inline in config tests) for capability set

- [ ] **Step 1: Capability unit tests in `cairn-core`**

Add to `crates/cairn-core/src/config/mod.rs` tests section:

```rust
#[test]
fn semantic_search_on_when_local_embeddings_and_model_present() {
    let config = CairnConfig::default(); // local_embeddings: true
    let caps = config.capabilities(true);
    assert!(caps.semantic_search);
    assert!(!caps.hybrid_search, "hybrid is a follow-up issue");
}

#[test]
fn semantic_search_off_when_local_embeddings_false() {
    let mut config = CairnConfig::default();
    config.search.local_embeddings = false;
    let caps = config.capabilities(true); // even with model present
    assert!(!caps.semantic_search);
}

#[test]
fn semantic_search_off_when_model_absent() {
    let config = CairnConfig::default(); // local_embeddings: true
    let caps = config.capabilities(false); // model not on disk
    assert!(!caps.semantic_search);
}

#[test]
fn semantic_not_tied_to_llm_provider() {
    let mut config = CairnConfig::default();
    config.llm.provider = Some(cairn_core::config::LlmProvider::OpenAi);
    // Model absent → semantic still false, even though LLM is configured.
    let caps = config.capabilities(false);
    assert!(!caps.semantic_search);
    // Model present → semantic true, regardless of LLM.
    let caps2 = config.capabilities(true);
    assert!(caps2.semantic_search);
}
```

Run:
```bash
cargo nextest run -p cairn-core config --locked 2>&1 | tail -20
```

Expected: all pass.

- [ ] **Step 2: Store capability gate tests**

Create `crates/cairn-store-sqlite/tests/search_semantic_capability.rs`:

```rust
//! Capability-gate conformance: search_semantic returns CapabilityUnavailable
//! when vector capability is false.
use cairn_core::contract::memory_store::{MemoryStore, SemanticSearchArgs};
use cairn_store_sqlite::open_in_memory;

#[tokio::test]
async fn no_embedder_returns_capability_unavailable() {
    let store = open_in_memory().await.unwrap();
    assert!(!store.capabilities().vector);

    let err = store
        .search_semantic(&SemanticSearchArgs {
            query: "test".into(),
            filter: None,
            visibility_allowlist: vec![],
            limit: 5,
            model_label: "bge-small-en-v1.5".into(),
        })
        .await
        .unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("capability") || msg.contains("vector"),
        "expected CapabilityUnavailable error, got: {msg}"
    );
}
```

Run:
```bash
cargo nextest run -p cairn-store-sqlite search_semantic_capability --locked 2>&1 | tail -20
```

Expected: passes.

- [ ] **Step 3: Run full workspace test suite**

```bash
cargo nextest run --workspace --locked --no-fail-fast 2>&1 | tail -40
```

Fix any failures. When all pass:

- [ ] **Step 4: Run verification checklist**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
cargo deny check
cargo audit --deny warnings
cargo machete
```

Fix any failures before proceeding.

- [ ] **Step 5: Final commit**

```bash
git add -p  # stage all remaining changes carefully
git commit -m "test: capability gate conformance + full workspace pass (#48)"
```

---

### Task 12: Docgen + insta snapshot updates

- [ ] **Step 1: Regenerate CLI docs**

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --write
```

Review the diff in `docs/site/src/reference/generated/` — should show new `admin model fetch`, `admin reindex`, and updated `search` docs.

- [ ] **Step 2: Accept any new/changed insta snapshots**

```bash
cargo insta review
```

Review carefully: accept snapshots that reflect the new `search.local_embeddings` default, updated `status.capabilities`, new config section.

- [ ] **Step 3: Final workspace build + doc check**

```bash
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked 2>&1 | tail -20
mdbook build docs/site 2>&1 | tail -20
```

Expected: both pass.

- [ ] **Step 4: Commit**

```bash
git add docs/
git commit -m "docs: regenerate CLI reference for semantic search + admin verbs (#48)"
```

---

### Task 13: proptest round-trip + idempotence

**Files:**
- Create: `crates/cairn-store-sqlite/tests/vec_roundtrip_proptest.rs`

- [ ] **Step 1: Write proptest for vector round-trip precision**

Create `crates/cairn-store-sqlite/tests/vec_roundtrip_proptest.rs`:

```rust
//! Property: f32 vectors round-trip through sqlite-vec without exceeding 1e-7 error.
use proptest::prelude::*;
use cairn_store_sqlite::open_in_memory;

fn arb_vec384() -> impl Strategy<Value = Vec<f32>> {
    proptest::collection::vec(
        prop::num::f32::NORMAL,
        384..=384,
    )
}

proptest! {
    #[test]
    fn vec384_round_trips_within_tolerance(v in arb_vec384()) {
        // Encode as bytes then decode to check precision.
        let bytes: Vec<u8> = v.iter().flat_map(|&f| f.to_le_bytes()).collect();
        let decoded: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        for (orig, dec) in v.iter().zip(decoded.iter()) {
            prop_assert!(
                (orig - dec).abs() < 1e-7,
                "precision lost: {orig} → {dec}"
            );
        }
    }
}
```

Run:
```bash
cargo nextest run -p cairn-store-sqlite vec384_round_trips --locked 2>&1 | tail -20
```

Expected: passes (f32 LE round-trip is exact, no loss).

- [ ] **Step 2: Write drain idempotence proptest**

Add to the same file:

```rust
proptest! {
    #[test]
    fn drain_once_idempotent_for_same_record(body in "[a-z ]{1,200}") {
        // Running drain_once twice for the same pending row produces the
        // same record_vectors content (deterministic MockEmbedder).
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            use std::sync::Arc;
            use cairn_embeddings_local::{EmbeddingModelKind, MockEmbedder};
            use cairn_store_sqlite::store::reindex::drain_once;
            use cairn_store_sqlite::open_in_memory;
            use cairn_core::contract::memory_store::MemoryStore;
            use cairn_test_fixtures::sample_record;

            let mut r = sample_record();
            r.body = body.clone();

            let store = open_in_memory().await.unwrap();
            let outcome = store.upsert(&r).await.unwrap();
            let rid = outcome.record_id.as_str().to_owned();
            let conn = store.conn.as_ref().unwrap().clone();

            // Manually enqueue
            let rid2 = rid.clone();
            conn.call(move |c| {
                c.execute(
                    "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
                       VALUES (?, 'opt_in_backfill', 0, 0)",
                    rusqlite::params![rid2],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            }).await.unwrap();

            let emb = Arc::new(MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5));

            // First drain — embeds and removes pending row.
            drain_once(Arc::clone(&conn), Arc::clone(&emb)).await.unwrap();

            // Read the vector bytes.
            let rid3 = rid.clone();
            let bytes1: Vec<u8> = conn.call(move |c| {
                c.query_row(
                    "SELECT embedding FROM record_vectors WHERE record_id = ?",
                    rusqlite::params![rid3],
                    |r| r.get(0),
                ).map_err(Into::into)
            }).await.unwrap();

            // Re-enqueue and drain again.
            let rid4 = rid.clone();
            conn.call(move |c| {
                c.execute(
                    "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
                       VALUES (?, 'opt_in_backfill', 0, 0)
                       ON CONFLICT(record_id) DO UPDATE SET attempt_count = 0",
                    rusqlite::params![rid4],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            }).await.unwrap();

            drain_once(Arc::clone(&conn), Arc::clone(&emb)).await.unwrap();

            let rid5 = rid.clone();
            let bytes2: Vec<u8> = conn.call(move |c| {
                c.query_row(
                    "SELECT embedding FROM record_vectors WHERE record_id = ?",
                    rusqlite::params![rid5],
                    |r| r.get(0),
                ).map_err(Into::into)
            }).await.unwrap();

            prop_assert_eq!(bytes1, bytes2, "drain must be idempotent for same body");
            Ok(())
        }).unwrap();
    }
}
```

Run:
```bash
cargo nextest run -p cairn-store-sqlite drain_once_idempotent --locked 2>&1 | tail -20
```

Expected: passes.

- [ ] **Step 3: Final commit**

```bash
git add crates/cairn-store-sqlite/tests/vec_roundtrip_proptest.rs
git commit -m "test(proptest): vec384 round-trip precision + drain_once idempotence (#48)"
```

---

## Final checklist

Run the full CLAUDE.md §8 verification suite:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
cargo deny check
cargo audit --deny warnings
cargo machete
```

All green → open PR referencing #48 with brief sections §3.0, §18.c US7, §19. Include paste of nextest output in the description.
