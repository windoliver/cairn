# Hybrid Search, OpenAI Embeddings, Field-Weighted FTS5, and BrainBench Cats 1+2

**Date:** 2026-04-30
**Status:** Draft (brainstorm complete, awaiting user review)
**Branch:** `worktree-glittery-marinating-abelson`
**Builds on:** issue #48 (local embeddings + sqlite-vec ANN, closed)
**Tracks/refines:** #191 (RRF hybrid), #106 (embedding providers), #117 (public bench)
**Brief sections:** §3.0 (Nexus projections), §4 (`MemoryStore` contract), §6.5 (CLI), §6.11 (SQLite migrations), §8.0 (search verb), §15 (evaluation)

---

## 1. Summary

Build cairn's hybrid retrieval pipeline (FTS5 + semantic ANN + RRF fusion + cosine re-rank), add an opt-in OpenAI embedding adapter, strengthen the keyword baseline with field-weighted FTS5, and ship a `cairn-bench` binary that scores cairn against an external public 240-page rich-prose IR corpus across **eight columns** (4 cairn adapters × 4 upstream reference adapters) on retrieval Cats 1+2.

The deliverable proves cairn's offline-default retrieval (BGE-small + RRF + cosine) is competitive with a cloud-embedding hybrid pipeline, surfaces the gap to a graph-augmented retrieval (which cairn does not yet have), and gives release engineering a reproducible pass/fail gate.

---

## 2. Goals and non-goals

### Goals

1. Hybrid retrieval API: `cairn search --mode hybrid` returns RRF-fused, cosine-re-ranked results.
2. OpenAI embedding provider behind a Cargo feature, opt-in via `OPENAI_API_KEY`.
3. Field-weighted FTS5: structurally important columns (kind, class, scope) outweigh body prose in BM25 ranking.
4. `cairn-bench` binary that produces a deterministic 8-column scorecard (markdown + JSONL) on the world-v1 corpus.
5. Release-gate threshold: `hybrid-bge-rrf` P@5 ≥ 70% of `hybrid-openai-rrf` P@5, justifying the offline default.

### Non-goals

1. Implementing the graph-expansion leg of #191 (depends on #186 edges layer).
2. Compiled-truth or backlink boosts (cairn has no equivalent fields).
3. Other embedding providers (Ollama, Cohere, Voyage) — separate sub-issues under #106.
4. BrainBench Cats 3–12 (identity, temporal, provenance, auto-link, performance, agent-loop) — separate issues, blocked on prerequisite features.
5. BM25S external lexical projection (#105 — orthogonal).
6. Query expansion or spelling correction.

---

## 3. The 8-column comparison

The benchmark report's headline table:

| # | Adapter             | Source     | Keyword      | Embedder              | Fusion       | Re-rank        | Graph |
|---|---------------------|-----------|--------------|------------------------|--------------|----------------|-------|
| 1 | `bm25-only`         | cairn      | weighted FTS5 | —                       | —            | —              | —     |
| 2 | `vector-bge`        | cairn      | —            | local BGE-small (384d) | —            | —              | —     |
| 3 | `hybrid-bge-rrf`    | cairn      | weighted FTS5 | local BGE-small (384d) | RRF k=60     | cosine 0.7/0.3 | —     |
| 4 | `hybrid-openai-rrf` | cairn      | weighted FTS5 | OpenAI 3-large (1536d) | RRF k=60     | cosine 0.7/0.3 | —     |
| 5 | `gbrain-grep-only`  | upstream   | pg `ts_rank` | —                      | —            | —              | —     |
| 6 | `gbrain-vector`     | upstream   | —            | OpenAI 3-large (1536d) | —            | —              | —     |
| 7 | `gbrain-no-graph`   | upstream   | pg `ts_rank` | OpenAI 3-large (1536d) | RRF          | cosine         | —     |
| 8 | `gbrain-full`       | upstream   | pg `ts_rank` | OpenAI 3-large (1536d) | RRF          | cosine         | yes   |

Same corpus (240 pages, world-v1). Same 145 graded queries. Macro-averaged metrics: P@5, R@5, MRR, nDCG@5.

Columns 5–8 are static reference numbers captured once from upstream, pinned to `gbrain v0.20.0 + gbrain-evals 8dab7f7`, committed as `fixtures/v0/brainbench-world-v1/upstream-baseline.json`. They are not re-run by `cairn-bench`.

What each pair of columns answers:

- 1 vs 5 — does cairn's keyword retrieval keep up with a Postgres `ts_rank` baseline?
- 2 vs 6 — does local 384d BGE keep up with cloud 1536d OpenAI on vector-only retrieval?
- 3 vs 4 — is local BGE good enough that paying for OpenAI doesn't move the needle?
- 4 vs 7 — does cairn's RRF + cosine implementation match the upstream's same-recipe number? Sanity check.
- 4 vs 8 — what is the headroom from the graph layer cairn does not yet have?

---

## 4. Architecture

### 4.1 Workspace topology

Five crates change. Two are new.

| Crate                       | Status | Change                                                                                          |
|-----------------------------|--------|-------------------------------------------------------------------------------------------------|
| `cairn-core`                | edit   | RRF + cosine pure functions; `SearchConfig` extension; `HybridSearchOrchestrator` pure fn       |
| `cairn-store-sqlite`        | edit   | new migration `0030_records_fts_weighted.sql`; weighted-bm25 in `do_search_keyword`; `do_search_hybrid` |
| `cairn-embeddings-openai`   | new    | leaf crate, `OpenAiEmbedder` impl `EmbeddingModel`, behind `--features openai`                  |
| `cairn-cli`                 | edit   | `--mode {bm25,vector,hybrid}` + `--embed {local,openai}` + `--rerank-blend <f32>` flags         |
| `cairn-bench`               | new    | workspace bin; loads world-v1; runs cairn adapters; merges upstream baseline; emits report       |

`cairn-core` keeps its zero-workspace-deps invariant. `cairn-embeddings-openai` depends on `cairn-embeddings-local` (for the trait) and `cairn-core` (for `EmbeddingModelKind`).

### 4.2 Cargo features

- `cairn-cli`: `openai` feature (off by default) gates compile-time inclusion of `cairn-embeddings-openai` and exposes `--embed openai` in `clap` value enum.
- `cairn-bench`: `openai` feature (off by default) gates the `hybrid-openai-rrf` column at compile time. Without it, the column is reported as `[skipped: feature not compiled]`.
- The default `cairn` binary remains 100% offline P0. The OpenAI feature must be explicitly opted into at build time.

### 4.3 Dependency rule

`cairn-core` still has zero workspace deps. The `HybridSearchOrchestrator` is a pure function: it takes a `&dyn MemoryStore` handle and a `&dyn EmbeddingModel` (only used when `--mode != bm25`) and returns `Vec<ScoredRecord>`. The store implementation produces RRF inputs; the orchestrator merges them.

---

## 5. Components

### 5.1 `cairn-core::search` (new module)

Pure functions, zero side effects.

```rust
pub fn rrf_fusion(
    inputs: &[Vec<ScoredCandidate>],   // each input list pre-sorted by its own score
    k: usize,                          // RRF constant; default 60
) -> Vec<RrfCandidate>;

pub fn cosine_rerank(
    rrf: &[RrfCandidate],              // top-K from RRF
    doc_vectors: &HashMap<RecordId, Vec<f32>>,
    query_vector: &[f32],
    blend: f32,                        // alpha; final = alpha * rrf + (1-alpha) * cos
) -> Vec<RerankedCandidate>;
```

Both are unit-testable against fixed inputs.

### 5.2 `cairn-core::config::SearchConfig` (extended)

```rust
pub struct SearchConfig {
    pub local_embeddings: bool,
    pub embedding_model: EmbeddingModelKind,

    // new in this design
    pub default_mode: SearchMode,           // Bm25 | Vector | Hybrid
    pub default_provider: EmbeddingProvider, // Local | OpenAi
    pub rerank_blend: f32,                  // 0.0-1.0; default 0.7
    pub fts_column_weights: [f64; 4],       // [kind, class, scope, body]; default [10.0, 10.0, 5.0, 1.0]
    pub rrf_k: usize,                       // default 60
    pub rerank_topk: usize,                 // default 20
}
```

`EmbeddingProvider` is a closed enum (`Local`, `OpenAi`) with `#[non_exhaustive]`. Adding Ollama/Cohere/Voyage requires a new variant, which is a forcing function for the registry update.

### 5.3 `cairn-store-sqlite::store::search`

New method `do_search_hybrid` orchestrates parallel keyword + semantic, then calls `cairn-core::search::rrf_fusion` and `cosine_rerank`.

The cosine re-rank does a single second-pass `vec0` query for the top-20 RRF candidates (no N+1):

```sql
SELECT record_id, vector
FROM record_vectors
WHERE record_id IN (?, ?, ?, ...)   -- top-20 RRF survivors
  AND model = ?                      -- visible model_label only
```

### 5.4 `cairn-store-sqlite` migration `0030_records_fts_weighted.sql`

```sql
DROP TABLE records_fts;

CREATE VIRTUAL TABLE records_fts USING fts5(
  scope_user UNINDEXED,
  kind,                              -- column 1, weight 10.0
  class,                             -- column 2, weight 10.0
  scope_concat,                      -- column 3, weight  5.0
  body,                              -- column 4, weight  1.0
  tokenize='porter unicode61',
  content='records',
  content_rowid='rowid'
);

-- Backfill: rebuild FTS index for all existing rows. The migration is
-- append-only, so existing data is preserved in `records`; we just need
-- to populate the new FTS shape.
INSERT INTO records_fts(rowid, scope_user, kind, class, scope_concat, body)
SELECT
  rowid,
  scope_user,
  kind,
  class,
  scope_user || ' ' || scope_agent || ' ' || scope_project_root,
  body
FROM records
WHERE tombstoned_at IS NULL;

-- Triggers re-sync all four FTS columns when records mutates.
CREATE TRIGGER records_ai AFTER INSERT ON records BEGIN
  INSERT INTO records_fts(rowid, scope_user, kind, class, scope_concat, body)
  VALUES (
    NEW.rowid,
    NEW.scope_user,
    NEW.kind,
    NEW.class,
    NEW.scope_user || ' ' || NEW.scope_agent || ' ' || NEW.scope_project_root,
    NEW.body
  );
END;
-- (analogous _ad and _au triggers omitted for brevity)
```

Schema fingerprint regenerated. `verify_schema_fingerprint` verifies on open.

### 5.5 `cairn-embeddings-openai` (new leaf crate)

```rust
pub struct OpenAiEmbedder {
    api_key: String,                  // never logged above debug
    model: OpenAiModel,               // TextEmbedding3Large | TextEmbedding3Small
    http: reqwest::Client,
}

impl EmbeddingModel for OpenAiEmbedder {
    fn kind(&self) -> EmbeddingModelKind { /* OpenAiTextEmbedding3Large or 3Small */ }
    fn dim(&self) -> usize { 1536 }
    fn embed_query(&self, q: &str) -> Result<Vec<f32>, EmbeddingError> { /* HTTP POST */ }
    fn embed_document(&self, d: &str) -> Result<Vec<f32>, EmbeddingError> { /* HTTP POST */ }
    fn embed_documents(&self, ds: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> { /* batch */ }
}
```

Retry policy: exponential backoff with jitter, max 3 retries. Timeout 30s per request. Errors mapped to:
- HTTP 401/403 → `EmbeddingError::AuthFailed { provider: "openai" }`
- HTTP 429    → `EmbeddingError::RateLimited`
- HTTP 5xx after retries → `EmbeddingError::Network(String)`

The `EmbeddingError::AuthFailed` variant is added to the existing `EmbeddingError` enum (`#[non_exhaustive]` already, so no semver break).

`EmbeddingModelKind` (in `cairn-core`) gains:
```rust
pub enum EmbeddingModelKind {
    BgeSmallEnV1_5,
    AllMiniLmL6V2,
    OpenAiTextEmbedding3Large,    // new
    OpenAiTextEmbedding3Small,    // new
}
```

`hf_repo()` is changed to return `Option<&'static str>`, returning `None` for OpenAI variants — they have no HuggingFace repo. `ModelCache::ensure` and `ModelCache::fetch` reject `None`-repo variants with `EmbeddingError::ModelNotFetched { kind }` (no panic; preserves the cairn-core no-`unwrap`/`expect` invariant). OpenAI provider construction goes through `cairn-embeddings-openai::OpenAiEmbedder::new(api_key, model)` directly, never through the local cache.

### 5.6 `cairn-cli::verbs::search`

Adds three flags, all `clap` `ValueEnum` where applicable:

```
--mode {bm25, vector, hybrid}             (default: hybrid if vector cap, else bm25)
--embed {local, openai}                    (default: from config; ignored when --mode bm25)
--rerank-blend <f32>                       (default: from config; ignored unless --mode hybrid)
```

Capability gates produce specific exit codes:

| Condition                                                             | Exit code | Message |
|-----------------------------------------------------------------------|-----------|---------|
| `--mode vector` or `hybrid` + `vector: false` capability              | 69 (`EX_UNAVAILABLE`) | "Vector search unavailable. Run `cairn admin model fetch`." |
| `--embed openai` without `openai` feature compiled                    | 78 (`EX_CONFIG`) | "OpenAI embedder not compiled in. Recompile with `--features openai`." |
| `--embed openai` + feature compiled + no `OPENAI_API_KEY`             | 78 (`EX_CONFIG`) | "OpenAI embedder enabled but `OPENAI_API_KEY` is not set." |
| `--rerank-blend` outside `[0.0, 1.0]` or NaN                          | 2 (clap)  | clap usage error |

`status --json` reports the active default for `mode`, `embed`, and `rerank_blend` so callers can detect the chosen pipeline.

### 5.7 `cairn-bench` (new workspace crate)

Binary entry point `cairn-bench`. Reads:
- `fixtures/v0/brainbench-world-v1/pages/` — 240 page JSONs
- `fixtures/v0/brainbench-world-v1/queries.json` — 145 graded queries
- `fixtures/v0/brainbench-world-v1/upstream-baseline.json` — columns 5–8

Runs columns 1–4 by:
1. Spawning a fresh in-memory `SqliteMemoryStore` per adapter.
2. Ingesting all 240 pages.
3. Issuing each of the 145 queries through the appropriate `--mode`/`--embed` combination.
4. Computing per-query P@5, R@5, MRR, nDCG@5 against the gold from `queries.json`.
5. Macro-averaging across all 145 queries.

Embeddings are cached to `target/brainbench/embed-cache.bin` (page index → vector) so re-runs skip the inference and HTTP cost. Cache key is `(model_label, page_slug, content_hash)`.

Outputs:
- `target/brainbench/report.md` — markdown table with all 8 columns
- `target/brainbench/per-query.jsonl` — one row per (adapter, query) with rank list + metrics

Determinism: at N=1, stddev = 0 across re-runs, byte-identical reports. Covered by snapshot tests.

### 5.8 Fixture layout

```
fixtures/v0/brainbench-world-v1/
  LICENSE.NOTICE                    # MIT attribution to upstream corpus
  pages/                            # 240 JSON page files, copied from gbrain-evals/eval/data/world-v1/
    companies__acme-0.json
    people__alice-chen-12.json
    ...
  queries.json                      # 145 queries with relevance and grades, captured once from upstream
  upstream-baseline.json            # 4 reference adapters × 145 queries × per-query metrics
  README.md                         # provenance, version pin, regeneration instructions
```

The `queries.json` capture procedure is documented in the README:

```sh
# In a separate workspace, one-time setup:
git clone https://github.com/garrytan/gbrain-evals.git
cd gbrain-evals
git checkout 8dab7f7
bun install
OPENAI_API_KEY=... bun run --print-queries > /path/to/queries.json
OPENAI_API_KEY=... bun run --print-baseline > /path/to/upstream-baseline.json
```

A one-shot Bun helper script (committed to `scripts/capture-brainbench-baseline.ts`) drives upstream's existing modules to dump both files. The script is run manually outside the Cairn build; its output is what gets committed.

Re-capture only on upstream version bump; gated behind a manual procedure documented in `README.md`.

---

## 6. Data flow

### 6.1 Hybrid pipeline

```
                                     ┌───────────────┐
              ingest body ───────────▶│ records       │
                                     │ records_fts   │ (4-column weighted)
                                     │ record_vectors│ (model_label-tagged)
                                     └───────────────┘
                                              │
                       ┌──────────────────────┼──────────────────────┐
                       │                      │                      │
                       ▼                      ▼                      ▼
              ┌──────────────────┐   ┌──────────────────┐   ┌──────────────────┐
search query  │ FTS5 BM25        │   │ embed_query()    │   │ (graph leg)      │
              │ k=50             │   │  ↓               │   │ DEFERRED         │
              │ weighted bm25()  │   │ vec0 KNN k=50    │   │ (no edges yet)   │
              └──────────────────┘   └──────────────────┘   └──────────────────┘
                       │                      │                      │
                       └──────────────┬───────┴──────────────────────┘
                                      │
                                      ▼
                         ┌──────────────────────────┐
                         │ rrf_fusion(k=60)         │  pure cairn-core fn
                         │ → top-20 candidates      │
                         └──────────────────────────┘
                                      │
                                      ▼
                         ┌──────────────────────────┐
                         │ second-pass vec0 fetch    │  one query for top-20 vectors
                         │ cosine_rerank(α=0.7)      │  pure cairn-core fn
                         └──────────────────────────┘
                                      │
                                      ▼
                                  top-N to caller
```

### 6.2 Adapter modes

- `bm25-only` — keyword leg only; skip RRF, skip rerank.
- `vector-bge` / `vector-openai` — semantic leg only; skip RRF, skip rerank.
- `hybrid-bge-rrf` / `hybrid-openai-rrf` — both legs, RRF, cosine rerank.

The `do_search_hybrid` function selects which legs to run based on the `SearchMode` argument and the configured embedder.

---

## 7. Algorithms

### 7.1 RRF fusion

```
for each input list L_i (already sorted by L_i's own score, descending):
    for rank r in 1..=|L_i|:
        doc_id = L_i[r-1]
        rrf[doc_id] += 1.0 / (k + r)        # k=60

return sort_desc(rrf, key=score)
```

`k=60` constant matches upstream and is the usual default in IR literature. Pure function: input lists in, scored output out, no I/O.

### 7.2 Cosine re-rank

```
top_K = rrf_results[:rerank_topk]                       # default rerank_topk = 20
fetched = vec0_batch_fetch([d.id for d in top_K], model_label)

# Normalize RRF scores to [0, 1]
max_rrf = max(d.rrf_score for d in top_K)
for d in top_K:
    d.norm_rrf = d.rrf_score / max_rrf

# Blend
for d in top_K:
    cos = cosine_similarity(query_vec, fetched[d.id])
    d.final = blend * d.norm_rrf + (1 - blend) * cos    # blend = 0.7

return sort_desc(top_K, key=final)
```

### 7.3 Field-weighted BM25

```sql
SELECT id, bm25(records_fts, w_kind, w_class, w_scope, w_body) AS rank
FROM records_fts
WHERE records_fts MATCH ?
ORDER BY rank
LIMIT 50;
```

Weights from `SearchConfig::fts_column_weights`, default `[10.0, 10.0, 5.0, 1.0]`.

---

## 8. Configuration

`.cairn/config.yaml` example:

```yaml
search:
  local_embeddings: true
  embedding_model: bge-small-en-v1.5
  default_mode: hybrid
  default_provider: local           # or "openai"
  rerank_blend: 0.7
  fts_column_weights: [10.0, 10.0, 5.0, 1.0]
  rrf_k: 60
  rerank_topk: 20

embeddings:
  openai:
    model: text-embedding-3-large    # or 3-small
    api_key: ${OPENAI_API_KEY}       # env var indirection
    timeout_ms: 30000
    max_retries: 3
```

Precedence (CLAUDE.md §6.5): CLI flag > env > `.cairn/config.yaml` > user file > defaults.

---

## 9. Error handling

| Path                                     | Error                                                       | Surfaced as                                  |
|------------------------------------------|-------------------------------------------------------------|----------------------------------------------|
| `--mode hybrid` w/o vector cap           | `StoreError::CapabilityUnavailable`                         | exit 69, hint to fetch model                 |
| `--embed openai` w/o feature             | clap-time `ValueEnum` rejection                             | exit 78, hint to recompile                   |
| `--embed openai` + missing key           | `EmbeddingError::AuthFailed`                                | exit 78, hint to set `OPENAI_API_KEY`        |
| OpenAI 401/403                           | `EmbeddingError::AuthFailed`                                | exit 78, log no key material                 |
| OpenAI 429                               | `EmbeddingError::RateLimited`                               | exponential backoff, eventually 69           |
| OpenAI 5xx after retries                 | `EmbeddingError::Network`                                   | fall back to RRF-only score, WARN log        |
| Cosine rerank vector fetch fails         | `StoreError::Sqlite`                                        | propagate to verb layer                      |
| `--rerank-blend` out of range            | clap usage error                                            | exit 2                                       |

Logging: API keys never logged above `debug`. Record bodies never logged above `debug` (CLAUDE.md §6.6). All `tracing::instrument` boundaries use `skip(record, body, api_key)`.

---

## 10. Testing strategy

Per CLAUDE.md §7 (TDD: failing test first, in its own commit, before fix).

### 10.1 Unit tests (cairn-core)

- `rrf_fusion` with known input rank lists → expected merged order
- `cosine_rerank` with known vectors → expected blend
- `SearchConfig` deserialization from YAML/env
- Boundary: empty input lists, single-list fusion, all-empty
- Property test (`proptest`): RRF is order-preserving when one list dominates

### 10.2 Unit tests (cairn-store-sqlite)

- Migration `0030_records_fts_weighted.sql` is forward-runnable on a fresh DB and on a populated DB
- FTS5 triggers re-sync all four columns on UPDATE to records.kind, records.class, records.scope_*
- Weighted bm25 query returns expected ordering when kind matches strongly vs body matches strongly
- `do_search_hybrid` orchestration: small fixture, mock embedder, asserts ranking changes when blend changes from 1.0 → 0.5 → 0.0

### 10.3 Unit tests (cairn-embeddings-openai)

- Model dim = 1536 for both 3-large and 3-small
- HTTP error paths via `wiremock` or equivalent
- Retry on 5xx with backoff; no retry on 401
- Batch endpoint returns N vectors for N inputs

### 10.4 Integration tests

- `cairn search --mode hybrid` end-to-end with in-memory store + mock embedder
- `cairn search --mode bm25` works without any embedder configured
- `cairn search --mode vector` with capability unavailable returns exit 69
- `cairn-bench` on a 5-page mini-fixture runs deterministically twice (snapshot)
- OpenAI feature compiled but no key → exit 78
- OpenAI feature compiled with `wiremock` server simulating success → cols 4 + 7 produce expected fixture output

### 10.5 Snapshot tests

- `target/brainbench/report.md` byte-stable on the mini-fixture (insta)
- `cairn search --mode hybrid --json` output schema stable (insta)

### 10.6 Bench gate

- `cairn-bench` runs in CI on the mini 29-page fixture (already in `fixtures/v0/gbrain/`) — fast, no network.
- Full 240-page world-v1 run gated on `BENCH_FULL=1` env. Manual trigger only.
- `--features openai` columns gated on `OPENAI_API_KEY` repo secret. Without it: `[skipped]`.

---

## 11. Sequencing

Implementation order (each step lands as its own commit, each with failing tests committed first):

1. **`cairn-core::search::rrf_fusion` + `cosine_rerank` pure functions** — unit-tested, no consumers yet.
2. **`SearchConfig` extensions** + `EmbeddingModelKind` new variants — schema-only, no behavior change.
3. **Migration `0030_records_fts_weighted.sql`** — schema fingerprint regenerated, store tests still pass on the new schema.
4. **`do_search_keyword` weighted bm25** — uses new column weights from config.
5. **`do_search_hybrid` in `cairn-store-sqlite`** — orchestrates legs + calls cairn-core fns.
6. **`cairn-cli::verbs::search` flags** — `--mode`, `--embed`, `--rerank-blend`, with capability gates.
7. **`cairn-embeddings-openai` crate** — gated on `--features openai`, retry/error mapping, batch endpoint.
8. **`cairn-bench` crate** — fixture loader, adapter dispatch, metrics, report writer.
9. **Fixture stage**: copy world-v1 into `fixtures/v0/brainbench-world-v1/`, document capture procedure for `queries.json` + `upstream-baseline.json`.
10. **CI wiring** — bench step on mini-fixture default, full run gated by env.

Steps 1–2 unblock step 5. Step 7 is independent and can land in parallel after step 6 (they only meet in step 8).

---

## 12. Acceptance criteria

The branch ships when:

- [ ] All 11 verification commands in CLAUDE.md §8 pass on a clean checkout.
- [ ] `cairn search --mode bm25` returns weighted-bm25 results on a non-empty store.
- [ ] `cairn search --mode hybrid` returns RRF-fused, cosine-re-ranked results.
- [ ] `cairn search --mode hybrid` returns exit 69 when vector capability unavailable.
- [ ] `cairn search --embed openai` returns exit 78 when feature not compiled or key missing.
- [ ] `cairn-bench` on the mini-fixture produces a byte-stable report (snapshot test passes).
- [ ] `cairn-bench` on world-v1 completes in under 5 minutes on an M-series laptop with embeddings cached.
- [ ] World-v1 run produces all four cairn columns; OpenAI columns gracefully skipped when key absent.
- [ ] Release-gate threshold: `hybrid-bge-rrf` P@5 ≥ 70% of `hybrid-openai-rrf` P@5.
- [ ] `hybrid-bge-rrf` strictly beats both `bm25-only` and `vector-bge` — proves fusion is doing work.
- [ ] Brief §8.0 search verb table updated with new flags.
- [ ] Doctest version range `[0.1.0, 0.4.0)` unchanged (no semver bump from this work).
- [ ] No new entries in `cargo deny` allowlist beyond reqwest's transitive deps.

---

## 13. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| OpenAI cost overrun in CI | Feature gated; CI runs without `--features openai` by default; full bench manual trigger only |
| 135 MB BGE model download in CI | Cached after first run; `BENCH_FETCH_MODELS=1` gates fetch; pre-built CI image carries cache |
| Fixture license drift on upstream version bump | Pinned to `gbrain v0.20.0` + `gbrain-evals 8dab7f7`; re-capture is a manual gated procedure |
| Cosine re-rank adds latency | Re-rank only top-20; single batched `vec0` query; budget < 50ms p99 on 10k-record vault |
| Field-weight migration breaks existing stores | Migration is append-only; backfill `INSERT INTO records_fts SELECT ... FROM records` runs as part of `0030`; triggers handle ongoing writes |
| OpenAI rate limits flake CI | Feature off in CI default; tests use `wiremock` |
| Score normalization edge case (all-zero RRF scores) | `cosine_rerank` falls back to pure cosine when `max_rrf == 0` |

---

## 14. References

- `docs/design/design-brief.md` §3.0, §4, §6.5, §6.11, §8.0, §15
- `CLAUDE.md` §4.2 (offline P0), §6.5 (CLI), §6.6 (logging), §6.11 (SQLite migrations), §7 (TDD)
- Issue #48 — local embeddings + sqlite-vec ANN (closed; this builds on it)
- Issue #191 — RRF hybrid retrieval (this implements its first three legs)
- Issue #106 — embedding providers (this lands the OpenAI provider)
- Issue #117 — public bench reports (this is the first deliverable)
- Issue #105 — BM25S projection (out of scope; orthogonal)
- Issue #186 — bitemporal edges (out of scope; would unblock graph leg)
