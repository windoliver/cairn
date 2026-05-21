# Local embeddings with candle + sqlite-vec ANN — design

- **Issue:** [#48](https://github.com/windoliver/cairn/issues/48)
- **Parent:** [#6](https://github.com/windoliver/cairn/issues/6) — SQLite record store with FTS5, sqlite-vec, and local embeddings
- **Brief sections:** §3.0 storage topology, §18.c US7 search, §19 sequencing, §19.a KISS
- **Phase / priority:** v0.1 minimum substrate, P0
- **Contract impact:** `MemoryStore::CONTRACT_VERSION` 0.2.0 → 0.3.0
- **Status:** approved 2026-04-29

## 1. Goal

Land all three v0.1 search modes that the brief promises — keyword, semantic, hybrid — by adding the missing pieces this issue scopes:

- Pure-Rust embedding runtime (`candle`) over a small default model cached in `.cairn/models/`.
- Statically linked `sqlite-vec` ANN tables addressed by `record_id`.
- `MemoryStore::search_semantic` trait method with capability gating that returns `CapabilityUnavailable` when `search.local_embeddings: false`.
- Embed-on-write inside `upsert` plus an idempotent reindex backfill that survives transient embed failures and opt-in toggles.

This issue is intentionally scoped to the building blocks. Hybrid blending (RRF ranker pure-fn in `cairn-core` + `--mode hybrid` wiring in the search verb) is a follow-up issue; until that lands, `--mode hybrid` returns `CapabilityUnavailable`.

## 2. Non-goals

- **Hybrid blend ranker** (RRF or otherwise). Tracked separately. The `SearchCandidate.semantic_distance` column added here is the hook a future ranker reads.
- **Cloud embedding providers / litellm.** v0.2 concern (brief §3.0 row "Semantic search").
- **BM25S sidecar / Nexus.** v0.2 concern.
- **Embedding chunking.** One vector per record at v0.1; long bodies truncate at the tokenizer's 512-token max with a `debug` log.
- **ANN pagination.** Top-K only at v0.1; no cursor on `SemanticSearchArgs`.
- **EmbeddingProvider as a core contract.** Stays an internal trait inside the new leaf crate; v0.2 may promote it when litellm arrives.

## 3. Architecture

```
                                             pending_embeddings
                                             (queue table)
                                                 │
                                                 ▼
  upsert(record)                       ┌─ drain task ──────┐
        │                              │ (tokio interval,  │
        ▼                              │  cancel-on-drop)  │
  cairn-store-sqlite                   └────────┬──────────┘
    ├── compute_embedding ──► cairn-embeddings-local
    │       (pre-tx)                ├── EmbeddingModel trait (sync)
    │                               ├── BgeSmall  (candle BERT, default)
    │                               ├── MiniLm    (candle BERT)
    │                               └── ModelCache (.cairn/models/, hf-hub fetch)
    ├── BEGIN IMMEDIATE
    │      records (FTS triggers fire)
    │      record_vectors (sqlite-vec vec0)          ◄── if embed succeeded
    │      pending_embeddings  (insert / bump)       ◄── if embed errored
    └── COMMIT
```

### 3.1 Crate placement

New leaf crate `crates/cairn-embeddings-local/`. Depends only on `cairn-core` (for typed errors and `BodyHash`). `cairn-store-sqlite` adds it as a regular dep. No `cairn-core` contract bump — embedding is an internal trait owned by the new crate; promoting it to a core contract is deferred to v0.2 when a second backend (litellm) arrives.

This keeps candle's heavy compile out of `cairn-store-sqlite`'s default surface and gives the v0.2 swap a clean sibling-crate target.

### 3.2 Crate dependency rule check

`cairn-core` gains nothing — the trait change is internal to the existing `MemoryStore` module. The new leaf crate sits at the same level as other adapters (see CLAUDE.md §3 topology table). `scripts/check-core-boundary.sh` continues to pass.

## 4. The new crate — `cairn-embeddings-local`

```
crates/cairn-embeddings-local/
├── Cargo.toml
├── plugin.toml              ← matches the existing leaf-crate convention
├── src/
│   ├── lib.rs               ← pub re-exports
│   ├── error.rs             ← thiserror EmbeddingError
│   ├── model.rs             ← EmbeddingModel trait
│   ├── kind.rs              ← EmbeddingModelKind enum
│   ├── cache.rs             ← ModelCache (.cairn/models/, hf-hub fetch, integrity)
│   ├── bge.rs               ← BGE impl (asymmetric query prefix)
│   └── minilm.rs            ← MiniLM impl
└── tests/
    ├── golden_vectors.rs
    ├── cache_layout.rs
    └── prefix_asymmetry.rs
```

### 4.1 Trait surface

```rust
use cairn_core::config::EmbeddingModelKind;   // canonical home — see §7.1

/// Synchronous, CPU-bound. Caller wraps in `tokio::task::spawn_blocking`.
pub trait EmbeddingModel: Send + Sync {
    fn kind(&self) -> EmbeddingModelKind;
    fn dim(&self) -> usize;                                    // both defaults = 384
    fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
    fn embed_query(&self,    text: &str) -> Result<Vec<f32>, EmbeddingError>;
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum EmbeddingError {
    #[error("embedding model not fetched: {kind:?}")]
    ModelNotFetched { kind: EmbeddingModelKind },
    #[error("integrity mismatch at {path}")]
    IntegrityMismatch { path: PathBuf },
    #[error("tokenizer: {0}")]
    Tokenizer(String),                                          // tokenizers::Error wrapped
    #[error("inference: {0}")]
    Inference(#[from] candle_core::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("hf-hub: {0}")]
    Network(#[from] hf_hub::api::sync::ApiError),
}
```

`embed_query` is where BGE prepends `"Represent this sentence for searching relevant passages: "`; MiniLM treats both calls identically. The asymmetry never leaks to call sites.

### 4.2 Model cache

```
.cairn/models/
└── bge-small-en-v1.5/
    ├── config.json
    ├── tokenizer.json
    ├── model.safetensors
    └── .integrity            ← blake3 digest of {config + tokenizer + safetensors}
```

```rust
pub struct ModelCache { root: PathBuf }

impl ModelCache {
    pub fn new(vault_root: &Path) -> Self;                                       // .cairn/models/
    pub fn ensure(&self, kind: EmbeddingModelKind)
        -> Result<Arc<dyn EmbeddingModel>, EmbeddingError>;                      // load or err
    pub fn fetch(&self, kind: EmbeddingModelKind) -> Result<FetchReport, EmbeddingError>;
    pub fn is_present(&self, kind: EmbeddingModelKind) -> bool;                  // stat() probe
}

pub struct FetchReport {
    pub kind: EmbeddingModelKind,
    pub bytes_downloaded: u64,
    pub integrity: String,                                                       // hex-encoded blake3
    pub already_cached: bool,
}
```

`ensure` never auto-downloads. `fetch` stages into `.cairn/models/<kind>/.tmp/`, verifies the digest matches the per-version constant compiled into the crate, atomically renames into place, writes `.integrity`. Re-running `fetch` with an already-cached model is a no-op (returns `already_cached: true`).

`hf-hub` is the network layer — battle-tested in the candle ecosystem, supports `HF_ENDPOINT` mirror env var, sync API matches our `spawn_blocking` boundary.

### 4.3 Tokenization budget

512-token max for both default models. Inputs longer get truncated; `tracing::debug!(record_id, original_tokens, truncated_to = 512, "truncated input for embedding")`. No chunking at v0.1 — one vector per record matches the brief's "one ANN row per record_id" model.

## 5. SQLite schema and migration

New migration `0020_record_vectors.sql`:

```sql
-- sqlite-vec virtual table, fixed-size float[384] embeddings.
-- Loaded via the bundled sqlite-vec extension at every connection-open.
CREATE VIRTUAL TABLE record_vectors USING vec0(
  record_id TEXT PRIMARY KEY,
  embedding float[384],
  +model    TEXT NOT NULL                -- e.g., 'bge-small-en-v1.5'
);

-- Backfill / failure queue. Idempotent on (record_id).
CREATE TABLE pending_embeddings (
  record_id        TEXT PRIMARY KEY REFERENCES records(record_id) ON DELETE CASCADE,
  reason           TEXT NOT NULL,         -- 'embed_failed' | 'opt_in_backfill' | 'model_swap'
  attempt_count    INTEGER NOT NULL DEFAULT 0,
  last_attempt_at  INTEGER,               -- epoch seconds; NULL until first attempt
  last_error       TEXT,
  enqueued_at      INTEGER NOT NULL       -- epoch seconds
);
CREATE INDEX pending_embeddings_attempt_idx
  ON pending_embeddings(last_attempt_at, attempt_count);

CREATE TRIGGER records_vector_cleanup
  AFTER DELETE ON records
  BEGIN
    DELETE FROM record_vectors    WHERE record_id = OLD.record_id;
    DELETE FROM pending_embeddings WHERE record_id = OLD.record_id;
  END;
```

Why `+model` as a vec0 shadow column: lets queries skip rows whose vector was produced by a now-stale model after a swap, without scanning every row.

Why `pending_embeddings` references `records.record_id`, not `target_id`: a staged-but-not-active version that failed to embed is still recoverable; pointer-swap doesn't change `record_id`. `ON DELETE CASCADE` keeps physical purge clean.

Tombstone semantics: §5.6 logical tombstones flip a flag; vector and pending rows stay. The semantic search query joins `records WHERE active=1 AND tombstoned=0` so they are not surfaced. Physical `DELETE` (purge) cascades.

### 5.1 sqlite-vec linkage

`cairn-store-sqlite/Cargo.toml` adds the canonical `sqlite-vec` Rust crate that bundles the vec0 C source for static compile-in. Exact crate-name + version-pin land in the implementation PR after a `cargo deny check` pass; the `deny.toml` allowlist may need a license-stanza addition. Whichever crate version is selected, the build feature must produce a statically linked vec0 — no runtime extension load from disk. `SqliteMemoryStore::open` calls `sqlite_vec::load(&conn)` once per connection on the dedicated DB thread, before migrations run. No runtime extension loading from disk.

## 6. Trait change in `cairn-core`

### 6.1 New method on `MemoryStore`

```rust
async fn search_semantic(
    &self,
    args: &SemanticSearchArgs<'_>,
) -> Result<SemanticSearchPage, StoreError> {
    Err(Box::new(StoreError::CapabilityUnavailable("search.semantic")))
}
```

Default impl returns `CapabilityUnavailable` so adapters without vector support get the right behavior with no boilerplate.

### 6.2 New types

```rust
pub struct SemanticSearchArgs<'a> {
    pub query: String,                                          // raw user query
    pub filter: Option<ValidatedFilter<'a>>,
    pub visibility_allowlist: Vec<MemoryVisibility>,
    pub limit: usize,                                           // top-K
}

pub struct SemanticSearchPage {
    pub candidates: Vec<SearchCandidate>,                       // ascending by distance
}
```

### 6.3 `SearchCandidate` extension

```rust
pub struct SearchCandidate {
    // ... existing fields ...
    pub semantic_distance: Option<f32>,                         // None on keyword path
}
```

`None` on the keyword path keeps the column meaningful and gives a future hybrid ranker exactly the field it needs.

### 6.4 Contract version bump

`MemoryStore::CONTRACT_VERSION` 0.2.0 → 0.3.0 (added required-but-defaulted method + new types). `cairn-store-sqlite::ACCEPTED_RANGE` widens to `[0.2.0, 0.4.0)`. Compile-time guard in `lib.rs` enforces the host version stays in range.

## 7. Capability set + config

### 7.1 New `search` block in `CairnConfig`

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub local_embeddings: bool,                                 // default true
    pub embedding_model: EmbeddingModelKind,                    // default BgeSmallEnV1_5
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            local_embeddings: true,
            embedding_model: EmbeddingModelKind::BgeSmallEnV1_5,
        }
    }
}
```

Added to `CairnConfig` as a top-level field. `EmbeddingModelKind` lives in **`cairn-core`** (e.g., `cairn_core::config::EmbeddingModelKind` — a small enum with no functional code). The leaf crate imports it; this preserves the CLAUDE.md §3 invariant that `cairn-core` has zero workspace deps. Variant strings are kebab-case to match the brief's model identifiers (`bge-small-en-v1.5`, `all-mini-lm-l6-v2`).

### 7.2 Capability set fix

`CapabilitySet::semantic_search` was previously gated on `llm.provider.is_some()`. That was wrong per the brief — LLM provider is for completion, not embedding. New computation:

```rust
caps.semantic_search = config.search.local_embeddings
                   && cache.is_present(config.search.embedding_model);
caps.hybrid_search   = false;                                   // until follow-up issue lands
```

`cache.is_present(...)` is one stat() call at config-load time. Stale-after-load is acceptable: if the user deletes the model file mid-run, the next `search_semantic` call returns `CapabilityUnavailable` and `status` re-resolves on next start.

### 7.3 Capability flow

```
CairnConfig.search.local_embeddings
  └─ CairnConfig::capabilities() ──► CapabilitySet { semantic_search: bool }
       └─ status.capabilities advertises cairn.mcp.v1.search.semantic when true
            └─ verb dispatch checks cap before calling store
                 └─ store.capabilities().vector is the runtime ground truth
```

The verb-layer check is the cheap one (config); the store check is the authoritative one (runtime). Both required: `local_embeddings: true` + model present + adapter supports vectors.

## 8. Data flow

### 8.1 Write path — `upsert`

```rust
async fn upsert(&self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
    // 1. Compute the embedding outside the SQL transaction.
    let embed_outcome = match &self.embedder {
        Some(model) => {
            let body = record.body.clone();
            let m = Arc::clone(model);
            tokio::task::spawn_blocking(move || m.embed_document(&body))
                .await
                .map_err(StoreError::from)?
                .map_or_else(EmbedOutcome::Failed, EmbedOutcome::Succeeded)
        }
        None => EmbedOutcome::Skipped,
    };

    // 2. One transaction commits records + FTS + (vector | pending) atomically.
    self.conn.call(move |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let upsert_outcome = upsert::write_record_rows(&tx, &record)?;

        match embed_outcome {
            EmbedOutcome::Succeeded(vector) => {
                tx.execute(
                    "INSERT INTO record_vectors(record_id, embedding, model)
                       VALUES (?, ?, ?)
                       ON CONFLICT(record_id) DO UPDATE
                         SET embedding = excluded.embedding, model = excluded.model",
                    params![upsert_outcome.record_id, vector_blob(&vector), model_label],
                )?;
                tx.execute(
                    "DELETE FROM pending_embeddings WHERE record_id = ?",
                    params![upsert_outcome.record_id],
                )?;
            }
            EmbedOutcome::Failed(err) => {
                tx.execute(
                    "INSERT INTO pending_embeddings(record_id, reason, attempt_count, last_error, enqueued_at)
                       VALUES (?, 'embed_failed', 0, ?, ?)
                       ON CONFLICT(record_id) DO UPDATE
                         SET attempt_count = attempt_count + 1, last_error = excluded.last_error",
                    params![upsert_outcome.record_id, err.to_string(), now_secs()],
                )?;
                tracing::warn!(record_id = %upsert_outcome.record_id, error = %err,
                               "embed failed; queued for reindex");
            }
            EmbedOutcome::Skipped => { /* embedder absent */ }
        }
        tx.commit()?;
        Ok(upsert_outcome)
    }).await
}
```

Three properties:
1. **Record always lands.** Embed failure never blocks the FTS-visible write.
2. **Atomic side effects.** Vector + record + queue row commit together. No torn states.
3. **Idempotent on retry.** WAL replay re-runs the function; `ON CONFLICT … DO UPDATE` clauses handle either side of a crash.

### 8.2 Read path — `search_semantic`

```sql
SELECT r.record_id, r.target_id, r.scope, r.kind, r.class, r.visibility,
       v.distance AS semantic_distance,
       0.0 AS bm25,                                             -- not on this path
       (strftime('%s','now') - r.updated_at) AS recency_seconds,
       r.confidence, r.salience,
       (strftime('%s','now') - r.updated_at) AS staleness_seconds,
       '' AS snippet,                                           -- no FTS snippet at v0.1
       r.record_json
FROM   record_vectors v
JOIN   records        r ON r.record_id = v.record_id
WHERE  v.embedding MATCH ? AND k = ?                            -- vec0 KNN
  AND  r.active = 1 AND r.tombstoned = 0
  AND  v.model = ?                                              -- skip stale-model rows
  AND  <filter clause from ValidatedFilter>                     -- reuses the SQL builder
  AND  <visibility allowlist clause>                            --   already used by search_keyword
ORDER BY v.distance ASC
```

The filter and visibility clauses reuse the exact SQL builder helpers `search_keyword` already wires; no new filter compiler.

K is set to `args.limit * 2` for over-fetch; results trim post-filter. If post-filter results < `limit` we accept the short page rather than re-querying — typical filters retain most hits. Pagination is a v0.2 concern.

## 9. Drain task

```rust
// crates/cairn-store-sqlite/src/store/reindex.rs (new module)
pub(crate) async fn drain_loop(
    conn: AsyncConnection,
    embedder: Arc<dyn EmbeddingModel>,
    cancel: CancellationToken,
    interval: Duration,                                         // default 30 s
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tick.tick() => { let _ = drain_once(&conn, &embedder).await; }
        }
    }
}

pub(crate) async fn drain_once(...) -> Result<DrainStats, StoreError> {
    // 1. SELECT up to BATCH=32 rows from pending_embeddings ORDER BY enqueued_at,
    //    skipping rows where attempt_count > 5 (poison-pill cap).
    // 2. For each row: load body via get(record_id); spawn_blocking embed_document;
    //    in one tx, INSERT/UPDATE record_vectors and DELETE the pending row.
    //    On error, bump attempt_count + last_error + last_attempt_at.
    // 3. tracing::info span: {drained, failed, remaining}.
}

pub struct DrainStats { pub drained: usize, pub failed: usize, pub remaining: usize }
```

**Lifecycle.** `SqliteMemoryStore::open` spawns `drain_loop` only if `Some(embedder)` was passed in. Cancel token fires on store `Drop` (or explicit `close()`); inflight `spawn_blocking` finishes naturally.

**Poison-pill cap.** `attempt_count > 5` rows are skipped on subsequent drains. Operator can inspect them via SQL or (out of scope here) a future `cairn admin reindex --status`.

**Idempotence.** Re-running over the same row produces the same vector content (deterministic embedder for fixed `(input, model)`). Crash mid-drain replays cleanly.

## 10. CLI wiring

### 10.1 `cairn admin reindex --semantic`

Wraps `drain_once` with a one-shot-loop-to-completion. `--all` enqueues every active record first (for the model-swap case):

```sql
INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
  SELECT r.record_id, 'model_swap', 0, strftime('%s','now')
    FROM records r
   WHERE r.active = 1 AND r.tombstoned = 0
  ON CONFLICT(record_id) DO UPDATE
    SET reason = 'model_swap', attempt_count = 0;
```

Reports JSON `{drained, failed, remaining}`.

### 10.2 `cairn admin model fetch [--model <kind>]`

Wraps `ModelCache::fetch(kind)` with the default kind taken from `search.embedding_model`. Reports JSON `{kind, bytes_downloaded, integrity, already_cached}`.

Both verbs gated on `cairn.admin.v1` (existing extension namespace per §18.c v0.1 row).

### 10.3 `cairn bootstrap`

New ordered step `fetch_default_embedding_model`, runs after vault layout creation, before first WAL initialization. Skipped if `search.local_embeddings: false`. Bootstrap is the place where network is acceptable; on HF Hub failure surface a clear error and exit non-zero.

### 10.4 `cairn search --mode semantic`

Verb dispatch:
1. If `status.capabilities` lacks `cairn.mcp.v1.search.semantic` → `CapabilityUnavailable { semantic.disabled }`.
2. Embed query via `search_semantic` (store-internal); store re-validates `vector` capability.
3. Return same JSON / human shape as keyword search, plus `semantic_distance` per row.

`--mode hybrid` returns `CapabilityUnavailable { hybrid.not_implemented }` until the follow-up issue lands.

## 11. Error model

Every error path classified:

| Site | Error | Surface |
|---|---|---|
| Model file missing at `ensure()` | `EmbeddingError::ModelNotFetched` | wrapped to `StoreError::CapabilityUnavailable("semantic.model_not_fetched")` at the verb boundary |
| Tokenizer / inference failure | `EmbeddingError::Tokenizer/Inference` | logged at `warn`, queued in `pending_embeddings` with `reason='embed_failed'`, surfaced to the user as `Ok(upsert_outcome)` (record landed) |
| HF Hub network error during `fetch` | `EmbeddingError::Network` | bootstrap / admin verb exits non-zero with `EX_UNAVAILABLE=69` |
| Integrity mismatch on cached files | `EmbeddingError::IntegrityMismatch` | `ensure` returns it; verb maps to `CapabilityUnavailable { semantic.integrity }` and instructs the user to re-fetch |
| `sqlite-vec` MATCH error | `rusqlite::Error` | wrapped in `StoreError::Sql`, propagated to verb layer |
| `--mode semantic` + capability disabled | none generated locally | `CapabilityUnavailable { semantic.disabled }` from cap check before reaching the store |

No silent fallback to keyword search anywhere — fail-closed per brief §3.0.

## 12. Testing

### 12.1 Embedding fixtures (`cairn-embeddings-local/tests/`)

- `golden_vectors.rs` — `insta` snapshot of `embed_query("hello world")[..8]` for each model. Bit-exact against pinned weights.
- `cache_layout.rs` — tempfile vault root; assert `ensure` returns `ModelNotFetched` before fetch, canonical layout after, `IntegrityMismatch` if `.integrity` corrupted.
- `prefix_asymmetry.rs` — assert `embed_query("foo") != embed_document("foo")` for BGE; equal for MiniLM.

Real-weight tests gated behind `--features real-models`; default `cargo nextest run --workspace` uses a `MockEmbedder` only.

### 12.2 Reindex tests (`cairn-store-sqlite/tests/reindex.rs`)

- Records with missing vectors get embedded by `drain_once`. Inject 8 records via raw SQL bypassing upsert; run drain; assert `record_vectors` rowcount == 8, `pending_embeddings` rowcount == 0.
- Embedding failure paths via `MockEmbedder` first-call-fail; `attempt_count` increments, row stays queued, second drain succeeds + clears.
- Poison-pill cap: always-fail mock; after 5 attempts row is skipped; rowcount stays at 1.
- Model-swap rebuild: vectors written with model A; drain runs with model B; `search_semantic` ignores rows where `v.model != 'B'`; `cairn admin reindex --semantic --all` re-enqueues + drains.

### 12.3 Capability tests (`cairn-cli/tests/`, `cairn-store-sqlite/tests/`)

- `local_embeddings: true` + model present → status advertises `.semantic`; `search_semantic` returns rows.
- `local_embeddings: true` + model absent → status drops `.semantic`; verb returns `CapabilityUnavailable { semantic.model_not_fetched }`.
- `local_embeddings: false` → status drops `.semantic`; verb dispatch rejects `--mode semantic` before reaching the store.
- `MemoryStoreCapabilities { vector: false }` (FixtureStore) → trait default impl returns `CapabilityUnavailable`.
- §15 wire-compat snapshot of `status.capabilities` byte-identical across `local_embeddings ∈ {true, false}` with model present / absent — all four configurations snapshotted.

### 12.4 Property tests (`proptest`)

- `record_vectors` round-trip: random 384-dim float vectors round-trip without precision loss beyond 1e-7.
- `drain_once` idempotence: running twice produces the same `record_vectors` content for any pending set.

### 12.5 No DB mocking

Per CLAUDE.md §6.4: every store test uses tempfile vault + real `rusqlite::Connection` + statically linked sqlite-vec. The `MockEmbedder` is the only mock; it produces a deterministic 384-dim vector from `blake3(input).take(384*4)` interpreted as f32 LE.

## 13. Verification checklist

Per CLAUDE.md §8 and the issue's verification block:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check          # generated config + admin verb docs
mdbook build docs/site
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
cargo deny check
cargo audit --deny warnings
cargo machete
```

Plus the issue-specific tests in §12 above. The PR will paste the relevant nextest output into the description.

## 14. Out-of-scope deltas this PR also lands

- `CapabilitySet::semantic_search` decoupled from `llm.provider`. This is a pre-existing bug in `crates/cairn-core/src/config/mod.rs` — the brief says LLM provider is for completion, embeddings are separate. Fix is one diff inside this PR's config rewrite (the file is already being edited for the new `SearchConfig` block, so this fix sits in the same hunk).

That is the only adjacent fix bundled in. No other drive-by refactors.

## 15. Sequencing within this issue

The natural commit order, each commit independently buildable:

1. Add `cairn-embeddings-local` crate with trait + cache + BGE/MiniLM impls + golden tests. Workspace member, no consumers yet.
2. Migration `0020_record_vectors.sql` + `pending_embeddings` table. Migration tests assert schema.
3. Add `MemoryStore::search_semantic` trait method (defaulted) + new types + `CONTRACT_VERSION` bump. `cairn-store-sqlite::ACCEPTED_RANGE` widened. Trait-stub tests pass.
4. `SqliteMemoryStore::open` accepts `Option<Arc<dyn EmbeddingModel>>`; `upsert` integrates the embed-pre-tx path; `search_semantic` impl. Reindex tests + integration tests for embed-on-write.
5. Drain task module + lifecycle wiring; idempotence proptest.
6. `SearchConfig` block in `CairnConfig`, `CapabilitySet` rewrite, `cairn.admin.v1` verbs (`reindex`, `model fetch`), bootstrap step, search-verb dispatch updates. CLI snapshot tests.

Whether this lands as one PR or a stack is the implementer's call; either way each commit above is a coherent unit.
