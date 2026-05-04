# Issue #49 — Hybrid search orchestration & `cairn admin reindex --from-db`

- **Date:** 2026-05-02
- **Issue:** [windoliver/cairn#49](https://github.com/windoliver/cairn/issues/49)
- **Parent epic:** #6 (SQLite record store with FTS5, sqlite-vec, local embeddings)
- **Dependencies (merged):** #47 (FTS5 keyword search), #48 (local embeddings + ANN)
- **Brief sections:** §3.0, §5.1, §8 (search), §15 (capability negotiation), §19 (sequencing)

## 1. Goal

Close the three deliverables in #49:

1. **Hybrid orchestration** — combine keyword + semantic legs into one result set with deterministic weighting, deduplication, visibility filtering, **token-budget trimming**, and **component-score explanations** for lint/eval debugging.
2. **`search_mode` exposure on every surface** — wire the `keyword | semantic | hybrid` modes through CLI, MCP, and SDK with capability negotiation that fails closed.
3. **`cairn admin reindex --from-db`** — rebuild FTS5 + vector indexes from the authoritative `records` table after derived indexes are deleted or corrupted.

The acceptance criteria translate to:

- US7 keyword/semantic/hybrid golden queries pass under v0.1 using local embeddings.
- Hybrid responses carry enough scoring detail to debug ranking decisions.
- A destructive-fixture test that nukes `records_fts` and `record_vectors` recovers via one `--from-db` invocation.

## 2. Context — what already exists

Most of #47/#48 landed before this issue. Existing surface:

| Layer | What's there |
|---|---|
| `cairn-core::search::orchestrator` | `hybrid_search(inputs, params)` pure function — RRF fusion + cosine re-rank, returns `Vec<RerankedCandidate>`. |
| `cairn-store-sqlite::store::hybrid` | `do_search_hybrid` — runs keyword + semantic legs in parallel, fetches doc vectors, drives the orchestrator, hydrates final candidates. |
| `cairn-cli::verbs::search` | Per-mode dispatchers: keyword (TODO), semantic (wired), hybrid (wired). Capability gate for `--explain`. |
| `cairn-core::config::CapabilitySet` | `keyword_search`, `semantic_search`, `hybrid_search` already advertised based on `search.local_embeddings` + model presence. |
| `cairn-cli::verbs::admin_reindex` | `cairn admin reindex --semantic [--all]` — drains `pending_embeddings`, optionally re-enqueues all active records (model-swap path). |
| IDL + MCP schemas | `search.input.json` declares mode-keyed capabilities (`cairn.mcp.v1.search.{keyword,semantic,hybrid}`) and `--explain` is gated by `cairn.mcp.v1.policy_trace`. |
| `cairn-sdk::transport` | `SdkClient::search()` validates args + capability, then returns `Unimplemented`. |
| `cairn-mcp::handler` | `search` tool dispatches to a stub. |

Acceptance gaps:

1. No keyword-mode CLI execution path (TODO since #46).
2. No token-budget trimming.
3. No component-score explanation surface.
4. No `--from-db` rebuild path.
5. SDK and MCP `search()` are stubs — they validate but never touch the store.
6. No golden-query integration tests across the three modes.

## 3. Architecture

**Single dispatcher, three thin surfaces.** Lift the per-mode runner that today lives in `crates/cairn-cli/src/verbs/search.rs` into `cairn-core::verbs::search::run()`. The dispatcher accepts `&dyn MemoryStore`, parsed `SearchArgs`, `&CairnConfig`, and a `CapabilitySet`. It performs capability gating, dispatches to the right `store.search_*` method, applies token-budget trimming, builds the response envelope (with optional `explain` block when `--explain` is requested), and returns `VerbResponse<SearchData>`.

CLI/SDK/MCP each construct the store and config once, then call the dispatcher. CLI keeps its render layer (human + JSON formatters); SDK and MCP serialize the envelope directly.

**Boundary preserved.** `cairn-core` keeps its zero-adapter-deps invariant — `MemoryStore` already lives in core (`contract::memory_store`), embedder lives behind another core trait. No new core deps.

## 4. Crate-by-crate changes

### 4.1 `cairn-core`

- New module `verbs/search.rs`:
  ```rust
  pub async fn run(
      store: &dyn MemoryStore,
      config: &CairnConfig,
      caps: &CapabilitySet,
      args: SearchArgs,
  ) -> Result<VerbResponse<SearchData>, SearchError>
  ```
  Pure dispatcher; no I/O beyond `store.*` calls. Order: mode-cap → explain-cap → dispatch.
- New module `search/trim.rs`:
  ```rust
  pub fn token_budget_trim(
      candidates: Vec<SearchCandidate>,
      explain: Option<Vec<ScoreExplain>>,
      max_chars: usize,
  ) -> (Vec<SearchCandidate>, Option<Vec<ScoreExplain>>)
  ```
  Sums `snippet.len()` across candidates; stops appending once the running total would exceed `max_chars`. Trims explain block in lockstep so record-id alignment is preserved. Pure function, deterministic, testable.
- New `search::explain::ScoreExplain`:
  ```rust
  pub struct ScoreExplain {
      pub record_id: RecordId,
      pub bm25_rank: Option<usize>,
      pub semantic_rank: Option<usize>,
      pub rrf_score: f64,
      pub cosine: Option<f32>,
      pub final_score: f64,
  }
  ```
- `SearchData` (generated from IDL) gains `explain: Option<Vec<ScoreExplain>>`. IDL schema update + codegen rerun.
- `SearchConfig` gains `max_snippet_chars_per_page: usize` (default `8000`).
- New error enum `SearchError`:
  ```rust
  #[derive(Debug, thiserror::Error)]
  #[non_exhaustive]
  pub enum SearchError {
      #[error("capability unavailable: {capability}")]
      CapabilityUnavailable { capability: &'static str },
      #[error("invalid args: {reason}")]
      InvalidArgs { reason: String },
      #[error(transparent)]
      Store(#[from] StoreError),
  }
  ```

### 4.2 `cairn-store-sqlite`

- `do_search_hybrid` extended to optionally emit a `Vec<ScoreExplain>` parallel to `candidates` when the args carry `with_explain: true`. The orchestrator already produces `RerankedCandidate { rrf_score, cosine, final_score }`; we just need to capture per-leg ranks (positions in the `kw_list` / `sem_list` arrays) and project alongside.
- `HybridSearchPage` gains `pub explain: Option<Vec<ScoreExplain>>`. Same field added to `KeywordSearchPage` and `SemanticSearchPage` for symmetry — keyword page populates `bm25_rank` only; semantic populates `semantic_rank` only. Hybrid populates the full struct.
- `KeywordSearchArgs`, `SemanticSearchArgs`, `HybridSearchArgs` each gain `with_explain: bool` (default `false`). Cheap; avoids a sibling method on the trait.
- New `store::reindex::rebuild_from_db(conn, embedder)`:
  1. **TX1** — `DELETE FROM records_fts;` then
     `INSERT INTO records_fts (rowid, body, …) SELECT rowid, body, … FROM records WHERE active = 1 AND tombstoned = 0;`
  2. **TX2** — `DELETE FROM record_vectors;` then
     `INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
        SELECT record_id, 'rebuild_from_db', 0, strftime('%s','now')
          FROM records WHERE active = 1 AND tombstoned = 0
          ON CONFLICT(record_id) DO UPDATE
            SET reason = 'rebuild_from_db', attempt_count = 0;`
  3. Returns `RebuildStats { fts_rebuilt: usize, enqueued: usize }`. Caller drives the `drain_once` loop separately, mirroring `--all` ergonomics.
- Public re-export from `lib.rs`: `pub use store::reindex::{rebuild_from_db, RebuildStats};`.
- vec0/FTS5 transaction note: vec0 virtual tables don't always cooperate inside a transaction with FTS5. The two-TX shape sidesteps the question. If a single TX works on the existing schema, fold them; first PR validates.

### 4.3 `cairn-cli`

- `verbs/search.rs` collapses: the three per-mode functions become one `run_async` that opens the store + embedder + config, then calls `cairn_core::verbs::search::run(...)`. The keyword TODO disappears. CLI retains the render layer (`render_keyword_results`, `render_semantic_results`, `render_hybrid_results`) and an `--explain` formatter that prints the explain block as a sub-section.
- `verbs/admin_reindex.rs`:
  - New flag `--from-db` (mutually exclusive with `--semantic`-only; `--from-db` implies both FTS + vectors).
  - On `--from-db`: call `store::reindex::rebuild_from_db`, then run the existing drain loop (capped at 10 000 passes) to backfill vectors. Emit a `ReindexOutput { fts_rebuilt, drained, failed, remaining }`.
- `command.rs`: register `--from-db` flag on the `admin reindex` subcommand. Update `cairn-docgen` snapshots.

### 4.4 `cairn-sdk`

- `SdkClient` gains a constructor variant:
  ```rust
  pub fn with_store(store: Arc<dyn MemoryStore>, config: CairnConfig) -> Self
  ```
  Existing capability-only constructor stays for stub workflows.
- `search()` stops returning `Unimplemented` when a store is wired: validates args (existing path), dispatches to `cairn_core::verbs::search::run`, maps `SearchError` to `SdkError`. Without a store, falls back to the existing `Unimplemented` envelope so transport-only callers don't break.

### 4.5 `cairn-mcp`

- `handler.rs` accepts a store + config at server construction.
- The `search` tool dispatches to `cairn_core::verbs::search::run` and returns the envelope. Validation already happens at the schema layer (`search.input.json`).

### 4.6 `cairn-test-fixtures`

- New helper `build_hybrid_test_vault(records: &[RecordSpec]) -> TestVault` that returns a `tempfile::TempDir`-rooted vault with `.cairn/cairn.db` populated, the `MiniLM-L6-v2` test model staged under `.cairn/models/`, and `cairn_core::config` defaults written. Used by all integration tests below.

## 5. Data flow

```
caller → CLI/SDK/MCP wrapper
  ├─ load CairnConfig (figment layered)
  ├─ probe model presence → CapabilitySet
  ├─ open SqliteMemoryStore (with embedder if local_embeddings on)
  └─ call cairn_core::verbs::search::run(&store, &config, &caps, args)
        ├─ capability gate (mode → caps.{keyword,semantic,hybrid}_search)
        ├─ explain gate (args.explain → caps.policy_trace)
        ├─ build mode-specific args struct (Keyword|Semantic|HybridSearchArgs)
        │     └─ thread `with_explain` so store knows whether to emit ScoreExplain
        ├─ store.search_*(...) → page (candidates + optional explain block)
        ├─ token_budget_trim(page.candidates, explain, max_snippet_chars_per_page)
        │     └─ trims explain block in lockstep (same record_ids)
        └─ wrap in VerbResponse<SearchData> envelope (operation_id, status, data)

reindex --from-db flow:
  CLI → open store with embedder
       → store::reindex::rebuild_from_db(conn, embedder)
            ├─ TX1: TRUNCATE records_fts; INSERT … FROM records (active, non-tombstoned)
            ├─ TX2: TRUNCATE record_vectors; enqueue all into pending_embeddings (reason='rebuild_from_db')
            └─ drain loop until queue empty (caps at 10 000 passes; existing pattern)
       → emit ReindexOutput { fts_rebuilt, drained, failed, remaining }
```

## 6. Error handling

`cairn-core::verbs::search::SearchError`:

- `CapabilityUnavailable { capability }` — mode or `--explain` not advertised. Maps to MCP `CapabilityUnavailable`, sysexit 69.
- `InvalidArgs { reason }` — query empty, limit out of range, malformed filter. Sysexit 64.
- `Store(#[from] StoreError)` — propagated from MemoryStore impl. Maps to `Internal`.

Capability gate fails closed **before** any store call. Order: mode-cap → explain-cap → dispatch.

`store::reindex::rebuild_from_db` uses `StoreError`. Truncate+repopulate runs in two transactions (FTS5 + vec0 may not share a TX safely; first PR verifies and folds if possible). Failure mid-rebuild leaves derived indexes empty + `pending_embeddings` partially populated; re-running `--from-db` is idempotent. Drain-loop poison-pill handling unchanged.

CLI uses existing `human_error` + JSON envelope formatting. Token-budget trim cannot fail (pure function).

SDK maps `SearchError` to existing `SdkError` variants (`CapabilityUnavailable`, `InvalidArgs`, `Internal`).

MCP returns the standard error envelope per existing handler convention.

## 7. Capability negotiation

Already-merged `CairnConfig::capabilities(model_present)` produces:

| Cap | Source |
|---|---|
| `keyword_search` | always `true` (FTS5 always built) |
| `semantic_search` | `search.local_embeddings && model_present` |
| `hybrid_search` | `search.local_embeddings` (keyword leg works without model, but degrades quality if vectors missing) |

`status` advertises the corresponding `cairn.mcp.v1.search.*` capabilities. Dispatcher reads the `CapabilitySet`, not `status` — no TOCTOU window.

`--explain` path additionally requires `cairn.mcp.v1.policy_trace`.

## 8. Testing

### 8.1 Unit (`cairn-core`)

- `search/trim.rs` — empty input, all-fits, overflow midway, single oversized candidate, explain-block trimmed in lockstep with candidates, monotone (output ≤ input) under `proptest`.
- `search/explain.rs` — `ScoreExplain` projection from `RerankedCandidate` + leg ranks for hybrid; rank-only projection for keyword/semantic.
- `verbs/search.rs` — capability gate per mode (mock `MemoryStore`); `--explain` rejected when `policy_trace` absent; correct dispatch routing per mode; `SearchError` → envelope mapping.
- `proptest` — trim is monotone, preserves order, never produces inconsistent (candidate, explain) pairs.

### 8.2 Integration (`cairn-store-sqlite/tests/`)

- `reindex_from_db.rs` — seed N records, `DELETE FROM records_fts;` + `DELETE FROM record_vectors;`, call `rebuild_from_db`, assert FTS5 rebuilt (keyword query returns expected hits), vectors backfilled (`SELECT count(*) FROM record_vectors`), drain converges.
- Idempotency: run `rebuild_from_db` twice back-to-back; final counts identical.
- Tombstoned/inactive records excluded.
- Explain block emitted by hybrid leg matches manual RRF/cosine recomputation on a tiny synthetic input.

### 8.3 Integration (`cairn-cli/tests/`)

- `search_modes_golden.rs` — build vault via `cairn-test-fixtures::build_hybrid_test_vault`; load real `MiniLM-L6-v2` test model. For each mode, run `cairn search "<query>" --mode {keyword|semantic|hybrid} --json` via `assert_cmd`; snapshot via `insta`.
- `search_explain.rs` — same harness with `--explain`; snapshot the explain block (record-id list deterministic by seed).
- `admin_reindex_from_db.rs` — destructive-fixture test: build vault, delete derived indexes via raw SQL, run `cairn admin reindex --from-db --json`, snapshot output, assert subsequent search returns the original results.
- Golden queries in `fixtures/golden/search/` as `<query>.<mode>.json`; `insta` snapshots map 1:1.

### 8.4 SDK (`cairn-sdk/tests/`)

- `search_dispatch.rs` — construct `SdkClient::with_store(in_mem_store, config)`, call `search()` per mode, assert envelope shape + capability-gating errors.

### 8.5 MCP (`cairn-mcp/tests/`)

- `search_tool.rs` — handler test, feed JSON request matching `search.input.json`, assert response matches `search.json#/$defs/Data`.

### 8.6 Verification commands (per CLAUDE.md §8)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
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

## 9. Out of scope

- Token-accurate trimming via the embedder's tokenizer (P1; deferred until hot-memory assembly in §11 actually consumes search output downstream).
- Nexus sandbox BM25S sidecar.
- Cloud embedding providers.
- Removing the existing `--semantic --all` model-swap path (kept as an alias / specialization of `--from-db`).

## 10. Open questions

1. **vec0 + FTS5 transaction interaction.** First PR runs the two-TX shape; if `EXPLAIN` confirms vec0 cooperates, fold into one TX in a follow-up.
2. **Where `with_explain` lives on the args struct.** Currently proposed as a sibling field; alternative is a wrapper `ExplainedSearchArgs<T>(T)`. Sibling field wins on simpler trait-method count and matches existing `model_label` pattern.
3. **SDK constructor variant naming.** `with_store` vs `connected` vs `in_process`. Pick on first PR; not load-bearing.

## 11. Migration / rollout

No DB migration. New CLI flag is additive. New SDK constructor is additive. New core dispatcher absorbs CLI logic; CLI shrinks. IDL schema gains an optional `explain` field on `SearchData` — additive, forward-compatible (`#[serde(default)]`).

PR sequencing (one PR per row keeps reviews tractable):

1. Core dispatcher + `SearchError` + token-budget trim + ScoreExplain types (no surface changes yet, just plumbing + unit tests).
2. Store-side wiring: `with_explain` field through `*SearchArgs`, explain emission in `do_search_hybrid`, hydrate explain in keyword/semantic legs.
3. `rebuild_from_db` in the store + destructive-fixture integration test.
4. CLI `verbs/search.rs` collapse to dispatcher + keyword wire-up + `--explain` formatter; CLI `admin reindex --from-db` flag + handler.
5. SDK `with_store` constructor + `search()` execution.
6. MCP `handler` store injection + `search` tool execution.
7. Golden-query integration tests (`cairn-cli/tests/search_modes_golden.rs`, `search_explain.rs`, `admin_reindex_from_db.rs`).
