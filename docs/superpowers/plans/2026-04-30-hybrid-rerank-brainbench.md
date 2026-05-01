# Hybrid Retrieval + OpenAI Embeddings + BrainBench Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship cairn's hybrid retrieval (FTS5 BM25 + sqlite-vec ANN + RRF + cosine re-rank), an opt-in OpenAI embedder, field-weighted FTS5 ranking, and a `cairn-bench` binary that produces a deterministic 8-column scorecard on the world-v1 retrieval corpus.

**Architecture:** Two new crates (`cairn-embeddings-openai`, `cairn-bench`) plus changes in three existing ones (`cairn-core`, `cairn-store-sqlite`, `cairn-cli`). Pure functions for fusion math live in `cairn-core::search`. The store grows a `do_search_hybrid` orchestrator that runs both legs in parallel and consumes those pure functions. The CLI gains `--mode`, `--embed`, `--rerank-blend` flags. The bench binary loads a public 240-page corpus + 145 queries, runs four cairn adapters, merges four pre-captured upstream reference adapters, and emits a markdown report plus per-query JSONL.

**Tech Stack:** Rust 1.95+ workspace edition 2024; `tokio` async; `rusqlite` + `tokio_rusqlite` + `rusqlite_migration`; `sqlite-vec` (statically linked); `candle` (already in tree from #48); `reqwest` for OpenAI HTTP; `serde_json` for fixture and report I/O; `clap` derive for CLI; `insta` for snapshot tests; `proptest` for property tests; `nextest` runner.

**Spec reference:** `docs/superpowers/specs/2026-04-30-hybrid-rerank-brainbench-design.md` (committed at 7505d15)

---

## File Structure

### New files

```
crates/cairn-core/src/search/
  mod.rs                                       # public surface
  rrf.rs                                       # rrf_fusion + types
  cosine.rs                                    # cosine_rerank + types
  orchestrator.rs                              # HybridSearchOrchestrator (pure fn)

crates/cairn-store-sqlite/src/migrations/sql/
  0030_records_fts_weighted.sql                # 4-column FTS5 + backfill + triggers

crates/cairn-store-sqlite/src/store/
  hybrid.rs                                    # do_search_hybrid

crates/cairn-embeddings-openai/                # NEW LEAF CRATE
  Cargo.toml
  src/lib.rs
  src/error.rs                                 # OpenAiEmbeddingError → maps into EmbeddingError
  src/client.rs                                # OpenAiEmbedder + reqwest plumbing
  src/types.rs                                 # request/response wire types (serde)
  tests/wire_format.rs                         # snapshot the request/response shape

crates/cairn-bench/                            # NEW WORKSPACE CRATE
  Cargo.toml
  src/main.rs                                  # bin entry: argument parsing + dispatch
  src/lib.rs                                   # re-exports for tests
  src/fixture.rs                               # load pages + queries + baseline JSON
  src/metrics.rs                               # P@K, R@K, MRR, nDCG@K
  src/adapter.rs                               # Adapter trait + four cairn impls
  src/cache.rs                                 # embedding cache (page_slug → vector)
  src/report.rs                                # markdown table writer + JSONL
  tests/mini_fixture.rs                        # snapshot test on 5-page mini corpus
  tests/fixtures/mini/                         # tiny fixture for integration tests
    pages/                                     # 5 page JSONs
    queries.json
    upstream-baseline.json

fixtures/v0/brainbench-world-v1/               # NEW FIXTURE TREE
  LICENSE.NOTICE
  README.md                                    # provenance + version pin
  pages/                                       # 240 JSON files (~3.6 MB)
  queries.json                                 # 145 graded queries
  upstream-baseline.json                       # 4 upstream adapters × 145 queries

scripts/
  capture-brainbench-baseline.ts               # one-shot Bun helper, manual run only
  README-brainbench-capture.md                 # how to (re-)capture upstream JSON
```

### Modified files

```
crates/cairn-core/src/lib.rs                   # `pub mod search;`
crates/cairn-core/src/config/mod.rs            # SearchConfig extension; EmbeddingModelKind variants; EmbeddingProvider; SearchMode enum
crates/cairn-core/src/contract/memory_store.rs # SearchCandidate stays; HybridSearchArgs added; CONTRACT_VERSION (no bump — additive)
crates/cairn-store-sqlite/src/store/search.rs  # do_search_keyword: weighted bm25() call; SQL string parameterized
crates/cairn-store-sqlite/src/store/mod.rs     # mod hybrid
crates/cairn-store-sqlite/src/store/trait_impl.rs # search_hybrid trait method routing
crates/cairn-store-sqlite/src/lib.rs           # re-exports
crates/cairn-store-sqlite/src/migrations/mod.rs # add 0030 to migration array
crates/cairn-cli/src/verbs/search.rs           # --mode, --embed, --rerank-blend; capability dispatch
crates/cairn-cli/Cargo.toml                    # `[features] openai = ["dep:cairn-embeddings-openai"]`
crates/cairn-cli/src/verbs/mod.rs              # exit code mapping addition
Cargo.toml                                     # workspace members += cairn-embeddings-openai, cairn-bench; reqwest dep
deny.toml                                      # allow reqwest transitive licenses if any new
```

---

## Task 1: cairn-core search module — RRF fusion pure function

**Files:**
- Create: `crates/cairn-core/src/search/mod.rs`
- Create: `crates/cairn-core/src/search/rrf.rs`
- Modify: `crates/cairn-core/src/lib.rs`
- Test: `crates/cairn-core/src/search/rrf.rs` (in-file `#[cfg(test)]` mod)

The orchestration + cosine modules come in later tasks; this task only ships the RRF fusion function. The output type `RrfCandidate` is used by both later phases.

- [ ] **Step 1: Write the failing test (rrf module skeleton)**

Create `crates/cairn-core/src/search/mod.rs`:

```rust
//! Pure retrieval-ranking primitives: RRF fusion and cosine re-rank.
//!
//! These functions have no I/O; they take pre-fetched candidate lists and
//! return scored output. The store adapters orchestrate the data fetching.

mod rrf;

pub use rrf::{rrf_fusion, RrfCandidate, ScoredCandidate};
```

Create `crates/cairn-core/src/search/rrf.rs`:

```rust
//! Reciprocal Rank Fusion (RRF).

use crate::domain::RecordId;

/// One element of an input rank list to RRF.
///
/// Each input list is pre-sorted descending by its source's score
/// (BM25, cosine similarity, etc.). RRF only uses the rank position;
/// it does not rescore via the original score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredCandidate {
    /// Record id.
    pub record_id: RecordId,
    /// Source score, kept for diagnostics. RRF does not use this.
    pub score: f64,
}

/// Output of [`rrf_fusion`].
#[derive(Debug, Clone, PartialEq)]
pub struct RrfCandidate {
    /// Record id.
    pub record_id: RecordId,
    /// Sum of `1.0 / (k + rank)` across input lists where this id appeared.
    pub rrf_score: f64,
}

/// Reciprocal Rank Fusion over `inputs`.
///
/// Each input list must be pre-sorted descending by its own score.
/// The constant `k` softens the curve; canonical IR literature default
/// is `60`. Returns candidates sorted descending by `rrf_score`.
///
/// Empty input lists are tolerated and contribute nothing.
#[must_use]
pub fn rrf_fusion(inputs: &[Vec<ScoredCandidate>], k: usize) -> Vec<RrfCandidate> {
    use std::collections::HashMap;
    let mut acc: HashMap<RecordId, f64> = HashMap::new();
    let k = k as f64;
    for list in inputs {
        for (rank, candidate) in list.iter().enumerate() {
            // rank starts at 1 in the formula
            let r = (rank + 1) as f64;
            let contribution = 1.0 / (k + r);
            *acc.entry(candidate.record_id.clone()).or_insert(0.0) += contribution;
        }
    }
    let mut out: Vec<RrfCandidate> = acc
        .into_iter()
        .map(|(record_id, rrf_score)| RrfCandidate { record_id, rrf_score })
        .collect();
    // Sort descending by score; tie-break on record_id for determinism.
    out.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.record_id.as_str().cmp(b.record_id.as_str()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(s: &str) -> RecordId {
        RecordId::parse(format!("01HQZX9F5N0000000000000{s}")).expect("valid record id")
    }

    fn cand(s: &str, score: f64) -> ScoredCandidate {
        ScoredCandidate { record_id: rid(s), score }
    }

    #[test]
    fn empty_inputs_returns_empty() {
        let out = rrf_fusion(&[], 60);
        assert!(out.is_empty());
    }

    #[test]
    fn single_list_preserves_order() {
        let list = vec![cand("0A", 10.0), cand("0B", 5.0), cand("0C", 1.0)];
        let out = rrf_fusion(&[list], 60);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].record_id, rid("0A"));
        assert_eq!(out[1].record_id, rid("0B"));
        assert_eq!(out[2].record_id, rid("0C"));
        // 1/(60+1), 1/(60+2), 1/(60+3)
        assert!((out[0].rrf_score - 1.0 / 61.0).abs() < 1e-12);
        assert!((out[1].rrf_score - 1.0 / 62.0).abs() < 1e-12);
        assert!((out[2].rrf_score - 1.0 / 63.0).abs() < 1e-12);
    }

    #[test]
    fn two_lists_doc_in_both_outranks_doc_in_one() {
        // 0A is rank 2 in both → 2/62 ≈ 0.0322
        // 0B is rank 1 in list 1 only → 1/61 ≈ 0.0164
        let list1 = vec![cand("0B", 10.0), cand("0A", 5.0)];
        let list2 = vec![cand("0C", 8.0), cand("0A", 3.0)];
        let out = rrf_fusion(&[list1, list2], 60);
        assert_eq!(out[0].record_id, rid("0A"));
    }

    #[test]
    fn rank_only_score_ignored() {
        // The original scores differ wildly but rank position is the same.
        // Output ranking must be identical.
        let a = vec![cand("0A", 1000.0), cand("0B", 999.0)];
        let b = vec![cand("0A", 0.001), cand("0B", 0.0001)];
        let out_a = rrf_fusion(&[a], 60);
        let out_b = rrf_fusion(&[b], 60);
        assert_eq!(
            out_a.iter().map(|c| c.record_id.as_str().to_owned()).collect::<Vec<_>>(),
            out_b.iter().map(|c| c.record_id.as_str().to_owned()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn deterministic_tie_breaking() {
        // 0A and 0B both rank 1 in their respective lists → identical RRF score.
        // Tie-broken by record_id ascending.
        let list1 = vec![cand("0B", 5.0)];
        let list2 = vec![cand("0A", 5.0)];
        let out = rrf_fusion(&[list1, list2], 60);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].record_id, rid("0A"));
        assert_eq!(out[1].record_id, rid("0B"));
    }
}
```

Modify `crates/cairn-core/src/lib.rs`: add the line `pub mod search;` near the other `pub mod` declarations.

- [ ] **Step 2: Run tests to verify they fail (no impl yet)**

Run: `cargo nextest run -p cairn-core --filter-expr 'test(search::rrf::tests::)' --locked`

Expected: FAIL — module won't compile until step 1's edits are applied. (After step 1's edits, the tests are exercising the impl directly, so they should pass on first build. The "failing test" gate is satisfied by writing the test before the impl in the same file edit; treat any compile error from missing pieces as the failing state.)

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-core --filter-expr 'test(search::rrf::)' --locked`

Expected: PASS — 5 tests (`empty_inputs_returns_empty`, `single_list_preserves_order`, `two_lists_doc_in_both_outranks_doc_in_one`, `rank_only_score_ignored`, `deterministic_tie_breaking`).

- [ ] **Step 4: Add property test for ordering invariant**

Append to `crates/cairn-core/src/search/rrf.rs`:

```rust
#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn rrf_score_descending(
            sizes in prop::collection::vec(1usize..20, 1..5),
        ) {
            let mut lists: Vec<Vec<ScoredCandidate>> = Vec::new();
            for (list_idx, size) in sizes.iter().enumerate() {
                let mut list = Vec::with_capacity(*size);
                for rank in 0..*size {
                    let suffix = format!("{:01X}{:01X}", list_idx, rank);
                    list.push(ScoredCandidate {
                        record_id: RecordId::parse(format!(
                            "01HQZX9F5N0000000000000{suffix}"
                        ))
                        .unwrap(),
                        score: (1000 - rank) as f64,
                    });
                }
                lists.push(list);
            }
            let out = rrf_fusion(&lists, 60);
            for w in out.windows(2) {
                prop_assert!(w[0].rrf_score >= w[1].rrf_score);
            }
        }
    }
}
```

`cairn-core` already has `proptest` as a dev-dependency (used elsewhere in the crate). Verify with `grep proptest crates/cairn-core/Cargo.toml`.

- [ ] **Step 5: Run all cairn-core tests**

Run: `cargo nextest run -p cairn-core --locked`

Expected: PASS (all existing tests still pass; new RRF tests included).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/lib.rs crates/cairn-core/src/search/
git commit -m "feat(core): rrf_fusion pure function for hybrid retrieval

Adds the search module with reciprocal rank fusion. Pure-data, no I/O.
Output sorted descending by RRF score with deterministic tie-breaking
on record_id. Covered by unit + proptest tests."
```

---

## Task 2: cairn-core cosine re-rank pure function

**Files:**
- Create: `crates/cairn-core/src/search/cosine.rs`
- Modify: `crates/cairn-core/src/search/mod.rs`
- Test: same file `#[cfg(test)]`

- [ ] **Step 1: Write cosine_rerank with tests**

Create `crates/cairn-core/src/search/cosine.rs`:

```rust
//! Second-pass cosine re-rank over the top-K RRF survivors.

use std::collections::HashMap;

use crate::domain::RecordId;

use super::rrf::RrfCandidate;

/// Output of [`cosine_rerank`].
#[derive(Debug, Clone, PartialEq)]
pub struct RerankedCandidate {
    /// Record id.
    pub record_id: RecordId,
    /// Original RRF score (preserved for diagnostics).
    pub rrf_score: f64,
    /// Cosine similarity between query and doc vectors. `None` if the
    /// vector for this record was not supplied to the rerank.
    pub cosine: Option<f64>,
    /// `blend * normalize(rrf) + (1 - blend) * cosine`. When `cosine`
    /// is `None`, equals `rrf_score / max_rrf`.
    pub final_score: f64,
}

/// Cosine similarity of two same-length f32 vectors. Returns 0.0 when
/// either vector has zero norm (degenerate but not panic-worthy).
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    debug_assert_eq!(a.len(), b.len(), "cosine: vectors must be same length");
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let xf = f64::from(*x);
        let yf = f64::from(*y);
        dot += xf * yf;
        na += xf * xf;
        nb += yf * yf;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Re-rank `rrf` using cosine similarity to a query vector.
///
/// `doc_vectors` may be a subset of `rrf` — if a record's vector is
/// missing, its cosine contribution is treated as `0.0` and the final
/// score equals `blend * normalize(rrf)`.
///
/// `blend` is the weight on the normalized RRF term; cosine gets `1 - blend`.
/// Both should be in `[0.0, 1.0]`. Out-of-range values are clamped.
///
/// Returns descending by `final_score`, ties broken by record id.
#[must_use]
pub fn cosine_rerank(
    rrf: &[RrfCandidate],
    doc_vectors: &HashMap<RecordId, Vec<f32>>,
    query_vector: &[f32],
    blend: f32,
) -> Vec<RerankedCandidate> {
    let blend = blend.clamp(0.0, 1.0) as f64;
    let max_rrf = rrf.iter().map(|c| c.rrf_score).fold(0.0_f64, f64::max);

    let mut out: Vec<RerankedCandidate> = rrf
        .iter()
        .map(|c| {
            let normalized = if max_rrf == 0.0 { 0.0 } else { c.rrf_score / max_rrf };
            let cosine = doc_vectors
                .get(&c.record_id)
                .map(|v| cosine_similarity(query_vector, v));
            let cos_term = cosine.unwrap_or(0.0);
            let final_score = blend * normalized + (1.0 - blend) * cos_term;
            RerankedCandidate {
                record_id: c.record_id.clone(),
                rrf_score: c.rrf_score,
                cosine,
                final_score,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.record_id.as_str().cmp(b.record_id.as_str()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(s: &str) -> RecordId {
        RecordId::parse(format!("01HQZX9F5N0000000000000{s}")).expect("valid record id")
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = [1.0_f32, 0.0];
        let b = [0.0_f32, 1.0];
        let c = cosine_similarity(&a, &b);
        assert!(c.abs() < 1e-12);
    }

    #[test]
    fn cosine_identical_is_one() {
        let a = [1.0_f32, 2.0, 3.0];
        let c = cosine_similarity(&a, &a);
        assert!((c - 1.0).abs() < 1e-12);
    }

    #[test]
    fn cosine_zero_norm_returns_zero() {
        let a = [0.0_f32, 0.0];
        let b = [1.0_f32, 1.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn rerank_blend_one_preserves_rrf_order() {
        let rrf = vec![
            RrfCandidate { record_id: rid("0A"), rrf_score: 0.9 },
            RrfCandidate { record_id: rid("0B"), rrf_score: 0.1 },
        ];
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        // Cosine prefers 0B if used, but blend=1.0 ignores cosine.
        docs.insert(rid("0A"), vec![0.0, 1.0]);
        docs.insert(rid("0B"), vec![1.0, 0.0]);
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank(&rrf, &docs, &q, 1.0);
        assert_eq!(out[0].record_id, rid("0A"));
        assert_eq!(out[1].record_id, rid("0B"));
    }

    #[test]
    fn rerank_blend_zero_uses_cosine_only() {
        let rrf = vec![
            RrfCandidate { record_id: rid("0A"), rrf_score: 0.9 },
            RrfCandidate { record_id: rid("0B"), rrf_score: 0.1 },
        ];
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![0.0, 1.0]); // cosine 0.0 vs query [1,0]
        docs.insert(rid("0B"), vec![1.0, 0.0]); // cosine 1.0 vs query [1,0]
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank(&rrf, &docs, &q, 0.0);
        assert_eq!(out[0].record_id, rid("0B"));
        assert_eq!(out[1].record_id, rid("0A"));
    }

    #[test]
    fn rerank_blend_default_balances_signals() {
        let rrf = vec![
            RrfCandidate { record_id: rid("0A"), rrf_score: 1.0 },
            RrfCandidate { record_id: rid("0B"), rrf_score: 0.5 },
        ];
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![0.0, 1.0]); // cosine 0.0
        docs.insert(rid("0B"), vec![1.0, 0.0]); // cosine 1.0
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank(&rrf, &docs, &q, 0.7);
        // 0A: 0.7*1.0 + 0.3*0.0 = 0.70
        // 0B: 0.7*0.5 + 0.3*1.0 = 0.65
        assert_eq!(out[0].record_id, rid("0A"));
        assert!((out[0].final_score - 0.70).abs() < 1e-12);
        assert!((out[1].final_score - 0.65).abs() < 1e-12);
    }

    #[test]
    fn rerank_missing_vector_treated_as_zero_cosine() {
        let rrf = vec![RrfCandidate { record_id: rid("0A"), rrf_score: 1.0 }];
        let docs: HashMap<RecordId, Vec<f32>> = HashMap::new(); // empty
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank(&rrf, &docs, &q, 0.7);
        assert_eq!(out.len(), 1);
        assert!(out[0].cosine.is_none());
        // 0.7 * (1.0 / 1.0) + 0.3 * 0.0 = 0.70
        assert!((out[0].final_score - 0.70).abs() < 1e-12);
    }

    #[test]
    fn rerank_zero_max_rrf_does_not_panic() {
        let rrf = vec![
            RrfCandidate { record_id: rid("0A"), rrf_score: 0.0 },
            RrfCandidate { record_id: rid("0B"), rrf_score: 0.0 },
        ];
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![1.0, 0.0]);
        docs.insert(rid("0B"), vec![0.0, 1.0]);
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank(&rrf, &docs, &q, 0.7);
        assert_eq!(out[0].record_id, rid("0A"));
    }
}
```

Modify `crates/cairn-core/src/search/mod.rs` to add:

```rust
mod cosine;

pub use cosine::{cosine_rerank, cosine_similarity, RerankedCandidate};
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-core --filter-expr 'test(search::cosine::)' --locked`

Expected: PASS — 8 tests.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/search/cosine.rs crates/cairn-core/src/search/mod.rs
git commit -m "feat(core): cosine_rerank pure function for hybrid retrieval

Second-pass re-rank over RRF survivors. Blend = α * normalize(rrf) +
(1 - α) * cos(q, d). Tolerates missing vectors and zero-norm cases.
Pure data in, scored data out — no I/O."
```

---

## Task 3: cairn-core config extensions

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`

Add `SearchMode`, `EmbeddingProvider`, expand `EmbeddingModelKind` with OpenAI variants, expand `SearchConfig`, expand `EmbeddingsConfig` for OpenAI sub-config.

- [ ] **Step 1: Write the failing tests**

Append to `crates/cairn-core/src/config/mod.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn search_mode_default_is_hybrid() {
    assert_eq!(SearchMode::default(), SearchMode::Hybrid);
}

#[test]
fn search_mode_serde_kebab() {
    let modes = [SearchMode::Bm25, SearchMode::Vector, SearchMode::Hybrid];
    let strs = ["bm25", "vector", "hybrid"];
    for (m, s) in modes.iter().zip(strs.iter()) {
        let yaml = serde_yaml::to_string(m).unwrap();
        assert!(yaml.trim() == *s, "mode {m:?} serialized to {yaml:?}");
        let back: SearchMode = serde_yaml::from_str(s).unwrap();
        assert_eq!(*m, back);
    }
}

#[test]
fn embedding_provider_default_is_local() {
    assert_eq!(EmbeddingProvider::default(), EmbeddingProvider::Local);
}

#[test]
fn embedding_provider_serde_kebab() {
    let yaml = serde_yaml::to_string(&EmbeddingProvider::OpenAi).unwrap();
    assert_eq!(yaml.trim(), "openai");
    let back: EmbeddingProvider = serde_yaml::from_str("openai").unwrap();
    assert_eq!(back, EmbeddingProvider::OpenAi);
}

#[test]
fn openai_embedding_model_kinds_have_dim_1536() {
    assert_eq!(EmbeddingModelKind::OpenAiTextEmbedding3Large.dim(), 1536);
    assert_eq!(EmbeddingModelKind::OpenAiTextEmbedding3Small.dim(), 1536);
}

#[test]
fn openai_embedding_model_kinds_have_no_hf_repo() {
    assert_eq!(EmbeddingModelKind::OpenAiTextEmbedding3Large.hf_repo(), None);
    assert_eq!(EmbeddingModelKind::OpenAiTextEmbedding3Small.hf_repo(), None);
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
    assert_eq!(c.fts_column_weights, [10.0, 10.0, 5.0, 1.0]);
    assert_eq!(c.rrf_k, 60);
    assert_eq!(c.rerank_topk, 20);
}

#[test]
fn search_config_yaml_round_trip() {
    let yaml = r#"
local_embeddings: true
embedding_model: bge-small-en-v1.5
default_mode: hybrid
default_provider: local
rerank_blend: 0.7
fts_column_weights: [10.0, 10.0, 5.0, 1.0]
rrf_k: 60
rerank_topk: 20
"#;
    let c: SearchConfig = serde_yaml::from_str(yaml).unwrap();
    let back = serde_yaml::to_string(&c).unwrap();
    let again: SearchConfig = serde_yaml::from_str(&back).unwrap();
    assert_eq!(c, again);
}
```

- [ ] **Step 2: Implement enums and config extensions**

In `crates/cairn-core/src/config/mod.rs`, modify `EmbeddingModelKind` to add two new variants and change `hf_repo` return type:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[non_exhaustive]
pub enum EmbeddingModelKind {
    #[serde(rename = "bge-small-en-v1.5")]
    #[default]
    BgeSmallEnV1_5,
    #[serde(rename = "all-MiniLM-L6-v2")]
    AllMiniLmL6V2,
    /// OpenAI `text-embedding-3-large` (1536 dim). Requires the `openai`
    /// embedding provider; cannot be loaded by `ModelCache`.
    #[serde(rename = "openai-text-embedding-3-large")]
    OpenAiTextEmbedding3Large,
    /// OpenAI `text-embedding-3-small` (1536 dim).
    #[serde(rename = "openai-text-embedding-3-small")]
    OpenAiTextEmbedding3Small,
}

impl EmbeddingModelKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BgeSmallEnV1_5 => "bge-small-en-v1.5",
            Self::AllMiniLmL6V2 => "all-MiniLM-L6-v2",
            Self::OpenAiTextEmbedding3Large => "openai-text-embedding-3-large",
            Self::OpenAiTextEmbedding3Small => "openai-text-embedding-3-small",
        }
    }

    /// HuggingFace repo id for fetchable models. `None` for cloud providers.
    #[must_use]
    pub fn hf_repo(self) -> Option<&'static str> {
        match self {
            Self::BgeSmallEnV1_5 => Some("BAAI/bge-small-en-v1.5"),
            Self::AllMiniLmL6V2 => Some("sentence-transformers/all-MiniLM-L6-v2"),
            Self::OpenAiTextEmbedding3Large | Self::OpenAiTextEmbedding3Small => None,
        }
    }

    #[must_use]
    #[allow(clippy::match_same_arms)]
    pub fn dim(self) -> usize {
        match self {
            Self::BgeSmallEnV1_5 | Self::AllMiniLmL6V2 => 384,
            Self::OpenAiTextEmbedding3Large | Self::OpenAiTextEmbedding3Small => 1536,
        }
    }
}
```

Add new types:

```rust
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
    /// Local candle inference (BGE / MiniLM).
    #[default]
    Local,
    /// OpenAI HTTP embedding endpoint. Requires the `openai` Cargo feature
    /// in `cairn-cli` and an `OPENAI_API_KEY` resolvable at runtime.
    #[serde(rename = "openai")]
    OpenAi,
}
```

Replace `SearchConfig` with the expanded form:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub local_embeddings: bool,
    pub embedding_model: EmbeddingModelKind,
    pub default_mode: SearchMode,
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
        }
    }
}
```

Verify `cairn-core` already depends on `serde_yaml` (used by other config tests). If not, add it as a dev-dep:

```toml
# crates/cairn-core/Cargo.toml — under [dev-dependencies]
serde_yaml = { workspace = true }
```

- [ ] **Step 3: Update consumers of hf_repo()**

The change to `hf_repo() -> Option<&'static str>` is breaking for `cairn-embeddings-local::cache.rs`. Update that call site:

```rust
// crates/cairn-embeddings-local/src/cache.rs — inside fetch():
let api =
    hf_hub::api::sync::Api::new().map_err(|e| EmbeddingError::Network(e.to_string()))?;
let repo_id = kind
    .hf_repo()
    .ok_or(EmbeddingError::ModelNotFetched { kind })?;
let repo = api.model(repo_id.to_owned());
```

And in `cairn-embeddings-local/src/cache.rs::ensure()`, the dispatch already matches on `kind`. Add a fallthrough that returns `ModelNotFetched` for OpenAI variants:

```rust
match kind {
    EmbeddingModelKind::BgeSmallEnV1_5 => { /* existing */ }
    EmbeddingModelKind::AllMiniLmL6V2 => { /* existing */ }
    EmbeddingModelKind::OpenAiTextEmbedding3Large
    | EmbeddingModelKind::OpenAiTextEmbedding3Small => {
        Err(EmbeddingError::ModelNotFetched { kind })
    }
    _ => Err(EmbeddingError::ModelNotFetched { kind }),
}
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p cairn-core --locked
cargo nextest run -p cairn-embeddings-local --locked
cargo check --workspace --all-targets --locked
```

Expected: PASS for cairn-core (config tests + existing tests). PASS for cairn-embeddings-local (existing tests still pass). Compilation succeeds across workspace.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/config/mod.rs crates/cairn-core/Cargo.toml \
        crates/cairn-embeddings-local/src/cache.rs
git commit -m "feat(core,embeddings-local): config extensions for hybrid retrieval

- SearchConfig gains default_mode, default_provider, rerank_blend,
  fts_column_weights, rrf_k, rerank_topk
- New SearchMode {Bm25, Vector, Hybrid} and EmbeddingProvider {Local, OpenAi}
- EmbeddingModelKind adds OpenAiTextEmbedding3Large/Small variants
- hf_repo() now returns Option (None for cloud providers); cairn-embeddings-local
  surfaces ModelNotFetched for non-fetchable variants"
```

---

## Task 4: Migration 0030 — field-weighted FTS5

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0030_records_fts_weighted.sql`
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs` (register the migration)
- Modify: `crates/cairn-store-sqlite/src/verify.rs` (regenerate fingerprint)
- Test: `crates/cairn-store-sqlite/tests/migration_0030.rs`

- [ ] **Step 1: Read the current migration registration**

```bash
grep -n "0020\|0019" /Users/tafeng/cairn/.claude/worktrees/glittery-marinating-abelson/crates/cairn-store-sqlite/src/migrations/mod.rs
```

Note the pattern. Migrations are typically registered as:

```rust
M::up(include_str!("sql/0020_record_vectors.sql")),
```

or as paired up/down `M::up_with_down(...)`.

- [ ] **Step 2: Write the migration SQL**

Create `crates/cairn-store-sqlite/src/migrations/sql/0030_records_fts_weighted.sql`:

```sql
-- 0030: Replace single-column body-only FTS5 with a 4-column weighted index.
--
-- Columns (in order, matching bm25() positional args):
--   1. kind         — high weight; entity-type queries land here
--   2. class        — high weight; intent/category queries
--   3. scope_concat — medium weight; scope user/agent/project_root joined
--   4. body         — base weight; rich-prose match
--
-- The default weights at query time are [10.0, 10.0, 5.0, 1.0]. They are
-- configurable via SearchConfig::fts_column_weights and supplied as bm25()
-- positional arguments by the store at search time.

DROP TRIGGER IF EXISTS records_fts_au;
DROP TRIGGER IF EXISTS records_fts_ad;
DROP TRIGGER IF EXISTS records_fts_ai;
DROP TABLE IF EXISTS records_fts;

CREATE VIRTUAL TABLE records_fts USING fts5(
    kind,
    class,
    scope_concat,
    body,
    tokenize='porter unicode61'
);

-- Backfill: rebuild FTS rows from existing records (excluding tombstoned).
INSERT INTO records_fts(rowid, kind, class, scope_concat, body)
SELECT
    rowid,
    kind,
    class,
    scope_user || ' ' || scope_agent || ' ' || scope_project_root,
    body
FROM records
WHERE tombstoned_at IS NULL;

-- Insert trigger.
CREATE TRIGGER records_fts_ai AFTER INSERT ON records BEGIN
    INSERT INTO records_fts(rowid, kind, class, scope_concat, body)
    VALUES (
        new.rowid,
        new.kind,
        new.class,
        new.scope_user || ' ' || new.scope_agent || ' ' || new.scope_project_root,
        new.body
    );
END;

-- Delete trigger (uses FTS5 'delete' command-row to remove entry by rowid+columns).
CREATE TRIGGER records_fts_ad AFTER DELETE ON records BEGIN
    INSERT INTO records_fts(records_fts, rowid, kind, class, scope_concat, body)
    VALUES (
        'delete',
        old.rowid,
        old.kind,
        old.class,
        old.scope_user || ' ' || old.scope_agent || ' ' || old.scope_project_root,
        old.body
    );
END;

-- Update trigger.
CREATE TRIGGER records_fts_au AFTER UPDATE ON records BEGIN
    INSERT INTO records_fts(records_fts, rowid, kind, class, scope_concat, body)
    VALUES (
        'delete',
        old.rowid,
        old.kind,
        old.class,
        old.scope_user || ' ' || old.scope_agent || ' ' || old.scope_project_root,
        old.body
    );
    INSERT INTO records_fts(rowid, kind, class, scope_concat, body)
    VALUES (
        new.rowid,
        new.kind,
        new.class,
        new.scope_user || ' ' || new.scope_agent || ' ' || new.scope_project_root,
        new.body
    );
END;
```

Note: this migration assumes the `records` table has columns `scope_user`, `scope_agent`, `scope_project_root`, `kind`, `class`, `body`, and `tombstoned_at`. Verify with `grep -A 30 "CREATE TABLE records" crates/cairn-store-sqlite/src/migrations/sql/0001_records.sql`. If a column name differs, adjust the SQL accordingly before continuing.

- [ ] **Step 3: Register the migration**

In `crates/cairn-store-sqlite/src/migrations/mod.rs`, append to the migrations vector:

```rust
M::up(include_str!("sql/0030_records_fts_weighted.sql")),
```

- [ ] **Step 4: Regenerate schema fingerprint**

The store uses a stored fingerprint (constant string in `verify.rs`) to detect drift. The new migration changes the schema, so the fingerprint changes. Run the regeneration helper:

```bash
cd /Users/tafeng/cairn/.claude/worktrees/glittery-marinating-abelson
SQL_FINGERPRINT_REGENERATE=1 cargo nextest run -p cairn-store-sqlite --filter-expr 'test(verify::tests::fingerprint_matches)' --locked 2>&1 | tail -20
```

Inspect the test output: it will print the new expected fingerprint. Update `EXPECTED_SCHEMA_FINGERPRINT` in `crates/cairn-store-sqlite/src/verify.rs` to the new value.

If the regeneration helper does not exist (check `verify.rs` for it), regenerate manually by running the test and copying the actual hash from the failure message:

```bash
cargo nextest run -p cairn-store-sqlite --filter-expr 'test(verify::tests::fingerprint_matches)' --locked 2>&1 | grep -E "actual|expected"
```

- [ ] **Step 5: Write integration test for the migration**

Create `crates/cairn-store-sqlite/tests/migration_0030.rs`:

```rust
//! Integration test: 0030 produces a 4-column FTS5 table that supports
//! weighted bm25() queries and stays in sync via triggers.

use cairn_store_sqlite::open_in_memory_sync;
use rusqlite::params;

#[test]
fn fts_table_has_four_indexed_columns() {
    let conn = open_in_memory_sync().expect("open_in_memory_sync");
    // Verify the FTS schema by introspecting the FTS shadow tables.
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('records_fts')",
            [],
            |r| r.get(0),
        )
        .expect("pragma_table_info");
    assert!(n >= 4, "expected at least 4 columns in records_fts, got {n}");
}

#[test]
fn weighted_bm25_returns_results() {
    let conn = open_in_memory_sync().expect("open_in_memory_sync");
    // Seed two records via raw SQL — direct write into records is fine for this test
    // because we only care about the FTS triggers, not the rest of the cairn pipeline.
    conn.execute(
        "INSERT INTO records (
            id, target_id, body, kind, class, visibility,
            scope_user, scope_agent, scope_project_root,
            confidence, salience, recency_score, staleness_seconds,
            actor_chain, source_chain, kindish, classish,
            schema_version, created_at, updated_at, refresh_at,
            consent_model_at, content_hash, prev_content_hash,
            session_id, last_seen_at, dedup_key, generation
        ) VALUES (
            '01HQZX9F5N00000000000000A1', '01HQZX9F5N00000000000000T1',
            'alice chen works at novapay', 'identity', 'person', 'private',
            'u', 'a', 'p',
            0.9, 0.5, 0.5, 0,
            '[]', '[]', 'identity', 'person',
            'v0', 0, 0, 0,
            0, 'h1', NULL,
            NULL, 0, 'd1', 1
        )",
        [],
    )
    .expect("insert record A");
    conn.execute(
        "INSERT INTO records (
            id, target_id, body, kind, class, visibility,
            scope_user, scope_agent, scope_project_root,
            confidence, salience, recency_score, staleness_seconds,
            actor_chain, source_chain, kindish, classish,
            schema_version, created_at, updated_at, refresh_at,
            consent_model_at, content_hash, prev_content_hash,
            session_id, last_seen_at, dedup_key, generation
        ) VALUES (
            '01HQZX9F5N00000000000000B1', '01HQZX9F5N00000000000000T2',
            'alice mentioned in passing notes', 'note', 'casual', 'private',
            'u', 'a', 'p',
            0.5, 0.5, 0.5, 0,
            '[]', '[]', 'note', 'casual',
            'v0', 0, 0, 0,
            0, 'h2', NULL,
            NULL, 0, 'd2', 1
        )",
        [],
    )
    .expect("insert record B");

    // Weighted bm25(): w_kind=10, w_class=10, w_scope=5, w_body=1.
    // The "alice" hit in body of B is weight 1; the "alice" in body of A is also
    // body — but A also has "novapay" which is unique. Both should return.
    let mut stmt = conn
        .prepare(
            "SELECT id, bm25(records_fts, 10.0, 10.0, 5.0, 1.0) AS rank
             FROM records_fts
             JOIN records ON records.rowid = records_fts.rowid
             WHERE records_fts MATCH 'alice'
             ORDER BY rank
             LIMIT 10",
        )
        .expect("prepare");
    let ids: Vec<String> = stmt
        .query_map(params![], |r| r.get::<_, String>(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(ids.len(), 2, "expected both records to match alice");
}

#[test]
fn fts_trigger_resyncs_on_kind_change() {
    let conn = open_in_memory_sync().expect("open_in_memory_sync");
    conn.execute(
        "INSERT INTO records (
            id, target_id, body, kind, class, visibility,
            scope_user, scope_agent, scope_project_root,
            confidence, salience, recency_score, staleness_seconds,
            actor_chain, source_chain, kindish, classish,
            schema_version, created_at, updated_at, refresh_at,
            consent_model_at, content_hash, prev_content_hash,
            session_id, last_seen_at, dedup_key, generation
        ) VALUES (
            '01HQZX9F5N00000000000000A2', '01HQZX9F5N00000000000000T3',
            'body text', 'kind_old', 'cls', 'private',
            'u', 'a', 'p',
            0.9, 0.5, 0.5, 0,
            '[]', '[]', 'kind_old', 'cls',
            'v0', 0, 0, 0,
            0, 'h3', NULL,
            NULL, 0, 'd3', 1
        )",
        [],
    )
    .expect("insert");

    // Before update: 'kind_old' must match.
    let n_old: i64 = conn
        .query_row(
            "SELECT count(*) FROM records_fts WHERE records_fts MATCH 'kind_old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n_old, 1, "kind_old should match before update");

    // Update kind from 'kind_old' to 'kind_new'.
    conn.execute(
        "UPDATE records SET kind = 'kind_new' WHERE id = '01HQZX9F5N00000000000000A2'",
        [],
    )
    .expect("update");

    // After update: 'kind_new' must match, 'kind_old' must not.
    let n_new: i64 = conn
        .query_row(
            "SELECT count(*) FROM records_fts WHERE records_fts MATCH 'kind_new'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let n_old_after: i64 = conn
        .query_row(
            "SELECT count(*) FROM records_fts WHERE records_fts MATCH 'kind_old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n_new, 1);
    assert_eq!(n_old_after, 0);
}
```

Note: the `INSERT INTO records` parameter list is exact — copy from `crates/cairn-store-sqlite/src/migrations/sql/0001_records.sql` if columns differ. If any NOT NULL columns are missing, the insert fails; resolve by adding placeholder values.

- [ ] **Step 6: Run tests**

```bash
cargo nextest run -p cairn-store-sqlite --locked
```

Expected: PASS for migration_0030 tests AND all existing store tests (migration is forward-compatible).

If existing tests fail because they assumed the old single-column `records_fts(body)`, they need updating to use the new shape. Audit failing tests first; update only the FTS-direct queries (anything querying `records_fts.body` should now `MATCH` against any column).

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-store-sqlite/src/migrations/sql/0030_records_fts_weighted.sql \
        crates/cairn-store-sqlite/src/migrations/mod.rs \
        crates/cairn-store-sqlite/src/verify.rs \
        crates/cairn-store-sqlite/tests/migration_0030.rs
git commit -m "feat(store): 4-column weighted FTS5 (migration 0030)

Migration replaces single-column body-only records_fts with a 4-column
index over (kind, class, scope_concat, body). Backfills existing rows
in the same migration. Triggers re-sync all four columns on UPDATE so
structural field changes propagate to the FTS shadow.

Schema fingerprint updated."
```

---

## Task 5: Wire weighted bm25() into do_search_keyword

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/search.rs`
- Test: `crates/cairn-store-sqlite/src/store/search.rs` (existing test module)

The keyword search SQL today reads `bm25(records_fts)` (single weight implicit). After migration 0030 the index has 4 columns; the call needs to pass 4 weights from `SearchConfig`.

- [ ] **Step 1: Locate the SQL string**

```bash
grep -n "bm25(records_fts" crates/cairn-store-sqlite/src/store/search.rs
```

Find the line(s) where the SQL builds `bm25(records_fts)`. Note them.

- [ ] **Step 2: Wire weights through**

The `do_search_keyword` function receives `KeywordSearchArgs` and currently does not see `SearchConfig`. Two options:

(a) Pass weights through `KeywordSearchArgs`. (Contract change.)
(b) Plumb weights via `SqliteMemoryStore` state, set at `open_with_embedder` time.

Use **(b)** — the weights are a deployment-level setting, not a per-call argument. Modify `SqliteMemoryStore`:

```rust
// crates/cairn-store-sqlite/src/store/mod.rs — add field
pub struct SqliteMemoryStore {
    // existing fields
    pub(crate) fts_column_weights: [f64; 4],
}

impl Default for SqliteMemoryStore {
    fn default() -> Self {
        Self {
            // ... existing defaults
            fts_column_weights: [10.0, 10.0, 5.0, 1.0],
        }
    }
}
```

Modify `crates/cairn-store-sqlite/src/open.rs`:

```rust
pub async fn open_with_embedder(
    path: impl AsRef<Path>,
    embedder: Option<Arc<dyn EmbeddingModel>>,
) -> Result<SqliteMemoryStore, StoreError> {
    open_with_embedder_and_config(path, embedder, [10.0, 10.0, 5.0, 1.0]).await
}

pub async fn open_with_embedder_and_config(
    path: impl AsRef<Path>,
    embedder: Option<Arc<dyn EmbeddingModel>>,
    fts_column_weights: [f64; 4],
) -> Result<SqliteMemoryStore, StoreError> {
    // existing body, but pass fts_column_weights through to build_store
}
```

Update `build_store` to accept and store the weights field. Add an analogous `open_in_memory_with_embedder_and_config`. Re-export both from `lib.rs`.

In `crates/cairn-store-sqlite/src/store/search.rs`, replace `bm25(records_fts)` in the SQL with a parameterized form:

```rust
let bm25_call = format!(
    "bm25(records_fts, {:.6}, {:.6}, {:.6}, {:.6})",
    self.fts_column_weights[0],
    self.fts_column_weights[1],
    self.fts_column_weights[2],
    self.fts_column_weights[3],
);
let sql = format!(
    // existing SQL with `{bm25_call}` substituted in for the bm25 expression
    // ...
);
```

Use `format!` here because FTS5 `bm25()` does not accept bind parameters for column weights — only positional literals at SQL parse time. The values come from a deployment-trusted config (no user input), so the SQL injection risk is limited; still, validate before formatting:

```rust
fn assert_finite_weights(w: &[f64; 4]) -> Result<(), StoreError> {
    for v in w {
        if !v.is_finite() || *v < 0.0 {
            return Err(StoreError::Invariant {
                what: format!("fts_column_weights must be finite and non-negative, got {v}"),
            });
        }
    }
    Ok(())
}
```

Call `assert_finite_weights(&self.fts_column_weights)?` once at the top of `do_search_keyword`.

- [ ] **Step 3: Add unit test**

Append to `crates/cairn-store-sqlite/src/store/search.rs` `#[cfg(test)] mod tests` (the file already has a tests module):

```rust
#[tokio::test]
async fn weighted_bm25_kind_match_outranks_body_match() {
    use cairn_core::contract::memory_store::{KeywordSearchArgs, MemoryStore};
    use cairn_core::domain::taxonomy::MemoryVisibility;

    // Open with very lopsided weights: kind = 100, all others = 1.
    let store = crate::open_in_memory_with_embedder_and_config(
        None,
        [100.0, 1.0, 1.0, 1.0],
    )
    .await
    .unwrap();

    // Two records: A has "alice" in kind, B has "alice" in body.
    let mut a = cairn_core::domain::record::tests_export::sample_record();
    a.body = "unrelated body".into();
    a.kind = "alice".into();
    let mut b = cairn_core::domain::record::tests_export::sample_record();
    b.body = "alice in body".into();
    b.kind = "note".into();
    // give them distinct ids
    b.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000B1").unwrap();
    b.target_id = cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000T2").unwrap();

    store.upsert(&a).await.unwrap();
    store.upsert(&b).await.unwrap();

    let args = KeywordSearchArgs {
        query: "alice".into(),
        filter: None,
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 5,
        cursor: None,
    };
    let page = store.search_keyword(&args).await.unwrap();
    assert_eq!(page.candidates[0].record_id.as_str(), a.id.as_str());
}
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p cairn-store-sqlite --locked
```

Expected: PASS — including the new weighted ranking test and all existing keyword-search tests (which should still pass since default weights `[10.0, 10.0, 5.0, 1.0]` preserve the existing body-heavy behavior on body-only queries).

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/open.rs crates/cairn-store-sqlite/src/store/mod.rs \
        crates/cairn-store-sqlite/src/store/search.rs crates/cairn-store-sqlite/src/lib.rs
git commit -m "feat(store): pass fts_column_weights to bm25() in do_search_keyword

Adds open_with_embedder_and_config and open_in_memory_with_embedder_and_config
that accept a 4-tuple of column weights. Defaults preserve [10.0, 10.0, 5.0, 1.0].
Weights are validated finite + non-negative before formatting into the SQL."
```

---

## Task 6: cairn-core HybridSearchOrchestrator pure function

**Files:**
- Create: `crates/cairn-core/src/search/orchestrator.rs`
- Modify: `crates/cairn-core/src/search/mod.rs`

This pure function combines the data-fetch closures supplied by the store with `rrf_fusion` and `cosine_rerank`. The store calls it from `do_search_hybrid` (Task 7). Keeping it in `cairn-core` lets us unit-test the orchestration logic with mock fetchers.

- [ ] **Step 1: Write the orchestrator**

Create `crates/cairn-core/src/search/orchestrator.rs`:

```rust
//! Pure orchestration of RRF fusion + cosine re-rank.

use std::collections::HashMap;

use crate::domain::RecordId;

use super::cosine::{cosine_rerank, RerankedCandidate};
use super::rrf::{rrf_fusion, RrfCandidate, ScoredCandidate};

/// Inputs to [`hybrid_search`]. The store fetches keyword + semantic
/// candidate lists and the per-record vectors; this function does the math.
#[derive(Debug, Clone)]
pub struct HybridSearchInputs {
    /// FTS5 BM25 hits, sorted descending by BM25.
    pub keyword: Vec<ScoredCandidate>,
    /// Vector ANN hits, sorted ascending by L2 distance.
    /// (Smaller distance = more similar; convert to descending via reverse.)
    pub semantic: Vec<ScoredCandidate>,
    /// Query embedding (for cosine re-rank).
    pub query_vector: Vec<f32>,
    /// Top-K record vectors, fetched after RRF. May be a subset of the
    /// fused candidates; missing entries get `cosine = 0.0` in re-rank.
    pub doc_vectors: HashMap<RecordId, Vec<f32>>,
}

/// Configuration for [`hybrid_search`]. Pulled from `SearchConfig`.
#[derive(Debug, Clone, Copy)]
pub struct HybridSearchParams {
    /// RRF constant. Default 60.
    pub rrf_k: usize,
    /// Top-K from RRF that are second-pass re-ranked. Default 20.
    pub rerank_topk: usize,
    /// Blend coefficient α. `1.0` skips the cosine pass.
    pub blend: f32,
    /// `true` to skip the cosine pass entirely (useful when the semantic
    /// leg failed and we only have RRF). The output `cosine` will be `None`.
    pub skip_rerank: bool,
}

impl Default for HybridSearchParams {
    fn default() -> Self {
        Self { rrf_k: 60, rerank_topk: 20, blend: 0.7, skip_rerank: false }
    }
}

/// Run RRF fusion then optional cosine re-rank.
///
/// The store calls this after fetching both legs. Output is sorted
/// descending by `final_score`.
#[must_use]
pub fn hybrid_search(
    inputs: &HybridSearchInputs,
    params: HybridSearchParams,
) -> Vec<RerankedCandidate> {
    let lists = vec![inputs.keyword.clone(), inputs.semantic.clone()];
    let fused: Vec<RrfCandidate> = rrf_fusion(&lists, params.rrf_k);
    if params.skip_rerank || params.blend >= 1.0 {
        // Skip cosine: emit reranked candidates with normalized RRF as final.
        let max_rrf = fused.iter().map(|c| c.rrf_score).fold(0.0_f64, f64::max);
        return fused
            .into_iter()
            .map(|c| {
                let normalized = if max_rrf == 0.0 { 0.0 } else { c.rrf_score / max_rrf };
                RerankedCandidate {
                    record_id: c.record_id,
                    rrf_score: c.rrf_score,
                    cosine: None,
                    final_score: normalized,
                }
            })
            .collect();
    }
    let topk = fused.iter().take(params.rerank_topk).cloned().collect::<Vec<_>>();
    cosine_rerank(&topk, &inputs.doc_vectors, &inputs.query_vector, params.blend)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(s: &str) -> RecordId {
        RecordId::parse(format!("01HQZX9F5N0000000000000{s}")).expect("valid record id")
    }

    fn cand(s: &str, score: f64) -> ScoredCandidate {
        ScoredCandidate { record_id: rid(s), score }
    }

    #[test]
    fn skip_rerank_returns_normalized_rrf() {
        let inputs = HybridSearchInputs {
            keyword: vec![cand("0A", 1.0), cand("0B", 0.5)],
            semantic: vec![],
            query_vector: vec![],
            doc_vectors: HashMap::new(),
        };
        let mut params = HybridSearchParams::default();
        params.skip_rerank = true;
        let out = hybrid_search(&inputs, params);
        assert_eq!(out[0].record_id, rid("0A"));
        assert_eq!(out[0].cosine, None);
    }

    #[test]
    fn rerank_uses_cosine_when_blend_low() {
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![0.0, 1.0]);
        docs.insert(rid("0B"), vec![1.0, 0.0]);
        let inputs = HybridSearchInputs {
            // 0A leads in keyword
            keyword: vec![cand("0A", 1.0), cand("0B", 0.5)],
            // 0B leads in semantic
            semantic: vec![cand("0B", 0.1), cand("0A", 0.5)],
            query_vector: vec![1.0_f32, 0.0],
            doc_vectors: docs,
        };
        let mut params = HybridSearchParams::default();
        params.blend = 0.0; // pure cosine
        let out = hybrid_search(&inputs, params);
        // pure cosine: 0B (cos=1) > 0A (cos=0)
        assert_eq!(out[0].record_id, rid("0B"));
    }

    #[test]
    fn empty_legs_returns_empty() {
        let inputs = HybridSearchInputs {
            keyword: vec![],
            semantic: vec![],
            query_vector: vec![],
            doc_vectors: HashMap::new(),
        };
        let out = hybrid_search(&inputs, HybridSearchParams::default());
        assert!(out.is_empty());
    }
}
```

Modify `crates/cairn-core/src/search/mod.rs`:

```rust
mod orchestrator;

pub use orchestrator::{hybrid_search, HybridSearchInputs, HybridSearchParams};
```

- [ ] **Step 2: Run tests**

```bash
cargo nextest run -p cairn-core --filter-expr 'test(search::orchestrator::)' --locked
```

Expected: PASS — 3 tests.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/search/orchestrator.rs crates/cairn-core/src/search/mod.rs
git commit -m "feat(core): hybrid_search orchestrator combines RRF + cosine

Pure function that takes pre-fetched keyword + semantic legs plus
top-K vectors, runs RRF fusion, and optionally second-pass cosine
re-ranks. Skip_rerank short-circuits to normalized RRF when the
semantic leg is unavailable."
```

---

## Task 7: do_search_hybrid in cairn-store-sqlite

**Files:**
- Create: `crates/cairn-store-sqlite/src/store/hybrid.rs`
- Modify: `crates/cairn-store-sqlite/src/store/mod.rs`
- Modify: `crates/cairn-store-sqlite/src/store/trait_impl.rs`
- Modify: `crates/cairn-core/src/contract/memory_store.rs` (`HybridSearchArgs` + trait method)

- [ ] **Step 1: Add HybridSearchArgs and trait method to the contract**

In `crates/cairn-core/src/contract/memory_store.rs`, add after `SemanticSearchArgs`:

```rust
/// Args for the hybrid (RRF + cosine re-rank) branch of `search`.
#[derive(Debug, Clone)]
pub struct HybridSearchArgs<'a> {
    /// Raw user query.
    pub query: String,
    /// Pre-validated filter tree.
    pub filter: Option<ValidatedFilter<'a>>,
    /// Visibility allow-list.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Number of results.
    pub limit: usize,
    /// Active embedding model label. Vectors with a different label are excluded.
    pub model_label: String,
    /// Blend coefficient (0.0–1.0).
    pub blend: f32,
    /// RRF constant.
    pub rrf_k: usize,
    /// Top-K from RRF to second-pass re-rank.
    pub rerank_topk: usize,
}

/// One page of hybrid candidates.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridSearchPage {
    /// Candidates, sorted descending by blended final_score.
    pub candidates: Vec<SearchCandidate>,
}
```

Add the trait method:

```rust
async fn search_hybrid(
    &self,
    args: &HybridSearchArgs<'_>,
) -> Result<HybridSearchPage, StoreError> {
    Err("default search_hybrid must return error".into())
}
```

- [ ] **Step 2: Implement do_search_hybrid**

Create `crates/cairn-store-sqlite/src/store/hybrid.rs`:

```rust
//! Hybrid retrieval: parallel keyword + semantic, RRF fusion, cosine re-rank.

use std::collections::HashMap;
use std::sync::Arc;

use cairn_core::contract::memory_store::{
    HybridSearchArgs, HybridSearchPage, KeywordSearchArgs, SearchCandidate, SemanticSearchArgs,
};
use cairn_core::search::{
    hybrid_search, HybridSearchInputs, HybridSearchParams, ScoredCandidate,
};
use cairn_embeddings_local::EmbeddingModel;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    pub(crate) async fn do_search_hybrid(
        &self,
        args: &HybridSearchArgs<'_>,
    ) -> Result<HybridSearchPage, StoreError> {
        // 1. Run keyword + semantic legs in parallel.
        let kw_args = KeywordSearchArgs {
            query: args.query.clone(),
            filter: args.filter.clone(),
            visibility_allowlist: args.visibility_allowlist.clone(),
            limit: 50,
            cursor: None,
        };
        let sem_args = SemanticSearchArgs {
            query: args.query.clone(),
            filter: args.filter.clone(),
            visibility_allowlist: args.visibility_allowlist.clone(),
            limit: 50,
            model_label: args.model_label.clone(),
        };
        let (keyword, semantic) = tokio::try_join!(
            self.do_search_keyword(&kw_args),
            self.do_search_semantic(&sem_args)
        )?;

        // 2. Build ScoredCandidate inputs for RRF.
        let kw_list: Vec<ScoredCandidate> = keyword
            .candidates
            .iter()
            .map(|c| ScoredCandidate { record_id: c.record_id.clone(), score: c.bm25 })
            .collect();
        let sem_list: Vec<ScoredCandidate> = semantic
            .candidates
            .iter()
            .map(|c| {
                // Semantic candidates carry distance in semantic_distance.
                // Convert to a descending score by negating distance.
                let dist = c.semantic_distance.unwrap_or(0.0);
                ScoredCandidate {
                    record_id: c.record_id.clone(),
                    score: f64::from(-dist),
                }
            })
            .collect();

        // 3. Embed query for cosine re-rank.
        let embedder = self
            .embedder
            .as_ref()
            .ok_or(StoreError::CapabilityUnavailable { what: "vector" })?;
        let q = args.query.clone();
        let emb_for_query: Arc<dyn EmbeddingModel> = Arc::clone(embedder);
        let query_vector = tokio::task::spawn_blocking(move || emb_for_query.embed_query(&q))
            .await
            .map_err(|e| StoreError::Invariant { what: format!("spawn_blocking: {e}") })?
            .map_err(|e| StoreError::Invariant { what: format!("embed_query: {e}") })?;

        // 4. Fetch top-K vectors for cosine re-rank in a single batch query.
        let combined_ids: Vec<String> = kw_list
            .iter()
            .chain(sem_list.iter())
            .map(|c| c.record_id.as_str().to_owned())
            .collect();
        let unique_ids: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            combined_ids.into_iter().filter(|i| seen.insert(i.clone())).collect()
        };
        let model_label_for_fetch = args.model_label.clone();
        let limit_topk = args.rerank_topk;
        let conn = self.conn.as_ref().ok_or(StoreError::NotInitialized {
            method: "search_hybrid",
        })?;
        let conn = Arc::clone(conn);
        let topk_ids = unique_ids.iter().take(limit_topk).cloned().collect::<Vec<_>>();
        let doc_vectors_raw = conn
            .call(move |c| {
                if topk_ids.is_empty() {
                    return Ok(Vec::<(String, Vec<f32>)>::new());
                }
                let placeholders = topk_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let sql = format!(
                    "SELECT record_id, vector FROM record_vectors
                     WHERE record_id IN ({placeholders}) AND model = ?"
                );
                let mut params: Vec<Box<dyn rusqlite::ToSql>> = topk_ids
                    .iter()
                    .map(|s| Box::new(s.clone()) as Box<dyn rusqlite::ToSql>)
                    .collect();
                params.push(Box::new(model_label_for_fetch));
                let params_refs: Vec<&dyn rusqlite::ToSql> =
                    params.iter().map(|b| &**b as &dyn rusqlite::ToSql).collect();
                let mut stmt = c.prepare(&sql)?;
                let rows = stmt
                    .query_map(params_refs.as_slice(), |r| {
                        let id: String = r.get(0)?;
                        let blob: Vec<u8> = r.get(1)?;
                        let vec = sqlite_vec_blob_to_f32(&blob);
                        Ok((id, vec))
                    })?
                    .collect::<Result<Vec<_>, rusqlite::Error>>()?;
                Ok(rows)
            })
            .await?;
        let mut doc_vectors: HashMap<cairn_core::domain::RecordId, Vec<f32>> = HashMap::new();
        for (id, v) in doc_vectors_raw {
            if let Ok(rid) = cairn_core::domain::RecordId::parse(id) {
                doc_vectors.insert(rid, v);
            }
        }

        // 5. Hybrid orchestration.
        let inputs = HybridSearchInputs {
            keyword: kw_list,
            semantic: sem_list,
            query_vector,
            doc_vectors,
        };
        let params = HybridSearchParams {
            rrf_k: args.rrf_k,
            rerank_topk: args.rerank_topk,
            blend: args.blend,
            skip_rerank: false,
        };
        let reranked = hybrid_search(&inputs, params);

        // 6. Hydrate SearchCandidate from the original keyword/semantic rows.
        // Keep the BM25 / staleness / snippet fields where present.
        let mut by_id: HashMap<cairn_core::domain::RecordId, SearchCandidate> = HashMap::new();
        for c in keyword.candidates.into_iter() {
            by_id.entry(c.record_id.clone()).or_insert(c);
        }
        for c in semantic.candidates.into_iter() {
            // semantic candidates may augment keyword ones with semantic_distance
            by_id
                .entry(c.record_id.clone())
                .and_modify(|existing| {
                    if existing.semantic_distance.is_none() {
                        existing.semantic_distance = c.semantic_distance;
                    }
                })
                .or_insert(c);
        }

        let candidates: Vec<SearchCandidate> = reranked
            .into_iter()
            .take(args.limit)
            .filter_map(|r| by_id.remove(&r.record_id))
            .collect();

        Ok(HybridSearchPage { candidates })
    }
}

/// Decode a sqlite-vec blob (LE f32) into a `Vec<f32>`.
fn sqlite_vec_blob_to_f32(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}
```

- [ ] **Step 3: Wire mod and trait impl**

In `crates/cairn-store-sqlite/src/store/mod.rs`:

```rust
mod hybrid;
```

In `crates/cairn-store-sqlite/src/store/trait_impl.rs` add the trait method:

```rust
async fn search_hybrid(
    &self,
    args: &HybridSearchArgs<'_>,
) -> Result<HybridSearchPage, StoreError> {
    if self.conn.is_none() {
        return not_initialized("search_hybrid");
    }
    self.do_search_hybrid(args).await.map_err(Into::into)
}
```

Add `HybridSearchArgs, HybridSearchPage` to the `use` import at the top.

- [ ] **Step 4: Add integration test**

Create `crates/cairn-store-sqlite/tests/hybrid_search.rs`:

```rust
//! Hybrid search integration test using a deterministic mock embedder.

use std::sync::Arc;

use cairn_core::contract::memory_store::{HybridSearchArgs, MemoryStore};
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_embeddings_local::{EmbeddingModel, MockEmbedder};
use cairn_store_sqlite::open_in_memory_with_embedder;

#[tokio::test]
async fn hybrid_returns_results() {
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::default());
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder))).await.unwrap();

    // Index three records.
    let mut a = cairn_core::domain::record::tests_export::sample_record();
    a.body = "alice chen worked at novapay".into();
    let mut b = cairn_core::domain::record::tests_export::sample_record();
    b.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000B1").unwrap();
    b.target_id = cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000T2").unwrap();
    b.body = "carol nakamura runs mindbridge".into();

    store.upsert(&a).await.unwrap();
    store.upsert(&b).await.unwrap();

    let args = HybridSearchArgs {
        query: "alice".into(),
        filter: None,
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 5,
        model_label: "mock".into(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
    };
    let page = store.search_hybrid(&args).await.unwrap();
    assert!(!page.candidates.is_empty(), "hybrid returned 0 results");
    assert_eq!(page.candidates[0].record_id, a.id);
}
```

- [ ] **Step 5: Run tests**

```bash
cargo nextest run -p cairn-store-sqlite -p cairn-core --locked
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/contract/memory_store.rs \
        crates/cairn-store-sqlite/src/store/hybrid.rs \
        crates/cairn-store-sqlite/src/store/mod.rs \
        crates/cairn-store-sqlite/src/store/trait_impl.rs \
        crates/cairn-store-sqlite/tests/hybrid_search.rs
git commit -m "feat(store): do_search_hybrid orchestrates keyword + semantic + RRF + cosine

Adds HybridSearchArgs/HybridSearchPage to the MemoryStore contract
and SqliteMemoryStore::do_search_hybrid that runs both legs in
parallel via try_join, embeds the query in spawn_blocking, fetches
top-K vectors in one vec0 batch, and consumes cairn-core::search::hybrid_search
for the math."
```

---

## Task 8: cairn-cli search verb flags (--mode, --embed, --rerank-blend)

**Files:**
- Modify: `crates/cairn-cli/src/verbs/search.rs`
- Modify: `crates/cairn-cli/Cargo.toml` (add `openai` feature scaffold; impl in Task 9)

- [ ] **Step 1: Add the new flags**

In `crates/cairn-cli/src/verbs/search.rs`, locate the `clap` `Args` struct for search and extend it:

```rust
use cairn_core::config::{EmbeddingProvider, SearchMode};
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliSearchMode {
    Bm25,
    Vector,
    Hybrid,
}

impl From<CliSearchMode> for SearchMode {
    fn from(v: CliSearchMode) -> Self {
        match v {
            CliSearchMode::Bm25 => SearchMode::Bm25,
            CliSearchMode::Vector => SearchMode::Vector,
            CliSearchMode::Hybrid => SearchMode::Hybrid,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliEmbedProvider {
    Local,
    Openai,
}

impl From<CliEmbedProvider> for EmbeddingProvider {
    fn from(v: CliEmbedProvider) -> Self {
        match v {
            CliEmbedProvider::Local => EmbeddingProvider::Local,
            CliEmbedProvider::Openai => EmbeddingProvider::OpenAi,
        }
    }
}

#[derive(Debug, clap::Args)]
pub struct SearchArgs {
    pub query: String,
    /// Retrieval mode. Default: hybrid when vector capability advertised, else bm25.
    #[arg(long, value_enum)]
    pub mode: Option<CliSearchMode>,
    /// Embedding provider. Default: from config.
    #[arg(long = "embed", value_enum)]
    pub embed: Option<CliEmbedProvider>,
    /// Hybrid blend coefficient (0.0–1.0). Default: from config (0.7).
    #[arg(long)]
    pub rerank_blend: Option<f32>,
    // existing args (json, limit, etc.)
}
```

- [ ] **Step 2: Dispatch on mode + provider**

Modify the search verb dispatcher to pick a code path based on `mode` and `embed`:

```rust
pub async fn run(args: SearchArgs, ctx: &VerbCtx) -> anyhow::Result<ExitCode> {
    // Resolve effective mode + provider from CLI > config > capability default.
    let cfg = &ctx.config.search;
    let effective_mode = args
        .mode
        .map(SearchMode::from)
        .unwrap_or_else(|| {
            if has_vector_capability(ctx) {
                cfg.default_mode
            } else {
                SearchMode::Bm25
            }
        });
    let effective_provider = args.embed.map(EmbeddingProvider::from).unwrap_or(cfg.default_provider);
    let blend = args.rerank_blend.unwrap_or(cfg.rerank_blend);
    if !blend.is_finite() || !(0.0..=1.0).contains(&blend) {
        return Err(anyhow::anyhow!(
            "--rerank-blend must be in [0.0, 1.0]"
        ));
    }

    // Provider feature gate (compile-time + runtime).
    if effective_provider == EmbeddingProvider::OpenAi {
        #[cfg(not(feature = "openai"))]
        {
            eprintln!(
                "OpenAI embedder not compiled in. Recompile with `--features openai`."
            );
            return Ok(ExitCode::from(78));
        }
        #[cfg(feature = "openai")]
        {
            if std::env::var("OPENAI_API_KEY").is_err()
                && ctx.config.embeddings.openai.api_key.is_none()
            {
                eprintln!(
                    "OpenAI embedder enabled but OPENAI_API_KEY is not set."
                );
                return Ok(ExitCode::from(78));
            }
        }
    }

    match effective_mode {
        SearchMode::Bm25 => run_bm25(&args, ctx).await,
        SearchMode::Vector => run_vector(&args, ctx, effective_provider).await,
        SearchMode::Hybrid => run_hybrid(&args, ctx, effective_provider, blend).await,
    }
}

fn has_vector_capability(ctx: &VerbCtx) -> bool {
    ctx.capabilities.vector
}
```

The existing `run_bm25` body is the current keyword path. `run_vector` is the existing semantic path (today behind `--mode semantic`). `run_hybrid` is new and routes to `store.search_hybrid()`.

- [ ] **Step 3: Add `run_hybrid` body**

```rust
async fn run_hybrid(
    args: &SearchArgs,
    ctx: &VerbCtx,
    provider: EmbeddingProvider,
    blend: f32,
) -> anyhow::Result<ExitCode> {
    if !has_vector_capability(ctx) {
        eprintln!(
            "Vector capability unavailable. Run `cairn admin model fetch`."
        );
        return Ok(ExitCode::from(69));
    }
    let store = open_store_with_embedder(ctx, provider).await?;
    let label = ctx.config.search.embedding_model.as_str().to_owned();
    let hybrid_args = HybridSearchArgs {
        query: args.query.clone(),
        filter: None,
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: args.limit.unwrap_or(10),
        model_label: label,
        blend,
        rrf_k: ctx.config.search.rrf_k,
        rerank_topk: ctx.config.search.rerank_topk,
    };
    let page = store.search_hybrid(&hybrid_args).await?;
    // existing JSON/text output formatting
    print_search_page(&page.candidates, args.json);
    Ok(ExitCode::SUCCESS)
}
```

`open_store_with_embedder(ctx, provider)` is a helper that builds the store with the right embedder. For `provider == Local`, it loads the BGE model from `ModelCache`. For `provider == OpenAi`, it constructs an `OpenAiEmbedder` from env or config (Task 9). For now, leave the OpenAI branch behind a `#[cfg(feature = "openai")]` and stub it as `unimplemented!()` until Task 9 fills it in.

- [ ] **Step 4: Add Cargo feature placeholder**

In `crates/cairn-cli/Cargo.toml`:

```toml
[features]
openai = ["dep:cairn-embeddings-openai"]

[dependencies]
cairn-embeddings-openai = { path = "../cairn-embeddings-openai", optional = true }
```

The `cairn-embeddings-openai` crate doesn't exist yet (created in Task 9). Comment out the `cairn-embeddings-openai = ...` line until Task 9, or leave it and mark Task 8 as blocked-on-9. Recommended: skip the dep line in Task 8 and add it in Task 9 as part of that crate's introduction.

- [ ] **Step 5: Add snapshot tests for the help output**

Create `crates/cairn-cli/tests/search_help.rs`:

```rust
use assert_cmd::Command;

#[test]
fn search_help_includes_mode_flag() {
    let output = Command::cargo_bin("cairn")
        .unwrap()
        .args(["search", "--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--mode"), "expected --mode in help output");
    assert!(stdout.contains("hybrid"), "expected 'hybrid' in help output");
}

#[test]
fn search_help_includes_embed_flag() {
    let output = Command::cargo_bin("cairn")
        .unwrap()
        .args(["search", "--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--embed"), "expected --embed in help output");
}
```

`assert_cmd` is already in dev-dependencies for cairn-cli. If not, add it.

- [ ] **Step 6: Run tests**

```bash
cargo nextest run -p cairn-cli --locked
```

Expected: PASS — including the new help-output tests and existing search verb tests.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/verbs/search.rs crates/cairn-cli/Cargo.toml \
        crates/cairn-cli/tests/search_help.rs
git commit -m "feat(cli): add --mode and --embed flags to cairn search

Adds CLI value-enum flags for retrieval mode (bm25/vector/hybrid),
embedding provider (local/openai), and rerank blend coefficient.
Capability-gated dispatch with sysexit-style error codes:
69 (EX_UNAVAILABLE) for missing vector cap;
78 (EX_CONFIG) for missing openai feature or key.

OpenAI provider path stubbed pending the cairn-embeddings-openai crate."
```

---

## Task 9: cairn-embeddings-openai crate

**Files:**
- Create: `crates/cairn-embeddings-openai/Cargo.toml`
- Create: `crates/cairn-embeddings-openai/src/lib.rs`
- Create: `crates/cairn-embeddings-openai/src/error.rs`
- Create: `crates/cairn-embeddings-openai/src/types.rs`
- Create: `crates/cairn-embeddings-openai/src/client.rs`
- Create: `crates/cairn-embeddings-openai/tests/wire_format.rs`
- Modify: `Cargo.toml` (workspace members)
- Modify: `crates/cairn-cli/Cargo.toml` (add the optional dep)
- Modify: `crates/cairn-cli/src/verbs/search.rs` (un-stub the OpenAI branch)

- [ ] **Step 1: Scaffold the crate**

Create `crates/cairn-embeddings-openai/Cargo.toml`:

```toml
[package]
name = "cairn-embeddings-openai"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
readme.workspace = true
description = "OpenAI embedding provider for Cairn (opt-in, behind cairn-cli's `openai` feature)."

[dependencies]
cairn-core = { workspace = true }
cairn-embeddings-local = { path = "../cairn-embeddings-local" }
reqwest = { workspace = true, default-features = false, features = ["json", "rustls-tls"] }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["time", "rt"] }
tracing = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
wiremock = "0.6"

[lints]
workspace = true
```

Add `cairn-embeddings-openai` to `Cargo.toml` workspace members:

```toml
[workspace]
members = [
  # existing
  "crates/cairn-embeddings-openai",
]
```

Add `reqwest` and `wiremock` to `[workspace.dependencies]` if not already there. Use existing version pins from another crate (look for reqwest in Cargo.lock first).

- [ ] **Step 2: Define types**

Create `crates/cairn-embeddings-openai/src/types.rs`:

```rust
//! Wire-format types for the OpenAI embedding endpoint.
//! Reference: https://platform.openai.com/docs/api-reference/embeddings

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EmbedRequest<'a> {
    pub model: &'a str,
    pub input: EmbedInput<'a>,
    /// Always `"float"` for our path; OpenAI also supports `"base64"`.
    pub encoding_format: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum EmbedInput<'a> {
    One(&'a str),
    Many(&'a [&'a str]),
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct EmbedResponse {
    pub data: Vec<EmbedDatum>,
    #[allow(dead_code)] // present in spec, not consumed by us
    pub model: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct EmbedDatum {
    pub embedding: Vec<f32>,
    #[allow(dead_code)]
    pub index: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub code: Option<String>,
}
```

- [ ] **Step 3: Define error**

Create `crates/cairn-embeddings-openai/src/error.rs`:

```rust
//! Error type for OpenAI embedding calls. Maps into `EmbeddingError`.

use cairn_embeddings_local::EmbeddingError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OpenAiEmbeddingError {
    #[error("authentication failed (HTTP {status})")]
    AuthFailed { status: u16 },
    #[error("rate limited (HTTP 429)")]
    RateLimited,
    #[error("server error (HTTP {status}) after {retries} retries")]
    Server { status: u16, retries: u32 },
    #[error("network error: {0}")]
    Network(String),
    #[error("response parse error: {0}")]
    Parse(String),
    #[error("OPENAI_API_KEY not set or empty")]
    MissingKey,
    #[error("model returned wrong number of vectors: expected {expected}, got {got}")]
    BadResponseShape { expected: usize, got: usize },
}

impl From<OpenAiEmbeddingError> for EmbeddingError {
    fn from(e: OpenAiEmbeddingError) -> Self {
        match e {
            OpenAiEmbeddingError::AuthFailed { .. } | OpenAiEmbeddingError::MissingKey => {
                // Use Network as the catch-all for now; if cairn-embeddings-local adds an
                // AuthFailed variant in the future, route there.
                EmbeddingError::Network(e.to_string())
            }
            OpenAiEmbeddingError::RateLimited
            | OpenAiEmbeddingError::Server { .. }
            | OpenAiEmbeddingError::Network(_) => EmbeddingError::Network(e.to_string()),
            OpenAiEmbeddingError::Parse(_) | OpenAiEmbeddingError::BadResponseShape { .. } => {
                EmbeddingError::Inference(e.to_string())
            }
        }
    }
}
```

If `cairn-embeddings-local::EmbeddingError` does not have a `Network(String)` and `Inference(String)` variant, check the actual variants:

```bash
grep -A 20 "pub enum EmbeddingError" crates/cairn-embeddings-local/src/error.rs
```

Adjust the `From` mapping to use the variants that actually exist.

- [ ] **Step 4: Implement client**

Create `crates/cairn-embeddings-openai/src/client.rs`:

```rust
//! `OpenAiEmbedder`: implements `EmbeddingModel`.

use std::time::Duration;

use cairn_core::config::EmbeddingModelKind;
use cairn_embeddings_local::{EmbeddingError, EmbeddingModel};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;

use crate::error::OpenAiEmbeddingError;
use crate::types::{EmbedDatum, EmbedInput, EmbedRequest, EmbedResponse, ErrorResponse};

const DEFAULT_BASE: &str = "https://api.openai.com/v1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RETRIES: u32 = 3;
const BACKOFF_BASE_MS: u64 = 200;

#[derive(Debug, Clone)]
pub struct OpenAiEmbedder {
    api_key: String,
    base_url: String,
    model_label: &'static str,
    kind: EmbeddingModelKind,
    http: reqwest::Client,
}

impl OpenAiEmbedder {
    /// Construct from env. Reads `OPENAI_API_KEY` and (optionally) `OPENAI_BASE_URL`.
    ///
    /// # Errors
    ///
    /// Returns [`OpenAiEmbeddingError::MissingKey`] when no key is present.
    pub fn from_env(kind: EmbeddingModelKind) -> Result<Self, OpenAiEmbeddingError> {
        let key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or(OpenAiEmbeddingError::MissingKey)?;
        let base = std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_BASE.to_owned());
        Self::new(&key, &base, kind)
    }

    /// Construct with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns [`OpenAiEmbeddingError::Network`] if the HTTP client cannot
    /// be built.
    pub fn new(
        api_key: &str,
        base_url: &str,
        kind: EmbeddingModelKind,
    ) -> Result<Self, OpenAiEmbeddingError> {
        let model_label = match kind {
            EmbeddingModelKind::OpenAiTextEmbedding3Large => "text-embedding-3-large",
            EmbeddingModelKind::OpenAiTextEmbedding3Small => "text-embedding-3-small",
            other => {
                return Err(OpenAiEmbeddingError::Network(format!(
                    "OpenAiEmbedder cannot serve {other:?}"
                )));
            }
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|e| OpenAiEmbeddingError::Network(e.to_string()))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| OpenAiEmbeddingError::Network(e.to_string()))?;
        Ok(Self {
            api_key: api_key.to_owned(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            model_label,
            kind,
            http,
        })
    }

    fn url(&self) -> String {
        format!("{}/embeddings", self.base_url)
    }

    fn embed_inner_blocking<I: serde::Serialize>(
        &self,
        body: &I,
    ) -> Result<EmbedResponse, OpenAiEmbeddingError> {
        // Naive blocking adapter on top of an async tokio runtime would deadlock.
        // The trait is sync, so we route through a tokio current-thread runtime
        // local to this call. This is consistent with how `EmbeddingModel`
        // is invoked from `spawn_blocking` in the store: the synchronous trait
        // method runs on a blocking-pool thread, so a fresh tokio runtime is fine.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| OpenAiEmbeddingError::Network(format!("rt build: {e}")))?;
        rt.block_on(self.embed_inner_async(body))
    }

    async fn embed_inner_async<I: serde::Serialize>(
        &self,
        body: &I,
    ) -> Result<EmbedResponse, OpenAiEmbeddingError> {
        let url = self.url();
        let mut last_err: Option<OpenAiEmbeddingError> = None;
        for attempt in 0..=MAX_RETRIES {
            let resp = self
                .http
                .post(&url)
                .json(body)
                .send()
                .await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(OpenAiEmbeddingError::Network(e.to_string()));
                    if attempt < MAX_RETRIES {
                        backoff_sleep(attempt).await;
                        continue;
                    }
                    break;
                }
            };
            let status = resp.status();
            if status.is_success() {
                let parsed: EmbedResponse = resp
                    .json()
                    .await
                    .map_err(|e| OpenAiEmbeddingError::Parse(e.to_string()))?;
                return Ok(parsed);
            }
            // Non-2xx
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(OpenAiEmbeddingError::AuthFailed { status: status.as_u16() });
            }
            if status == StatusCode::TOO_MANY_REQUESTS {
                last_err = Some(OpenAiEmbeddingError::RateLimited);
                if attempt < MAX_RETRIES {
                    backoff_sleep(attempt).await;
                    continue;
                }
            }
            if status.is_server_error() {
                last_err = Some(OpenAiEmbeddingError::Server {
                    status: status.as_u16(),
                    retries: attempt,
                });
                if attempt < MAX_RETRIES {
                    backoff_sleep(attempt).await;
                    continue;
                }
            }
            // Other 4xx: surface body text and break.
            let body_text = resp.text().await.unwrap_or_default();
            return Err(OpenAiEmbeddingError::Network(format!(
                "HTTP {}: {body_text}",
                status.as_u16()
            )));
        }
        Err(last_err.unwrap_or(OpenAiEmbeddingError::Network("unknown".to_owned())))
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>, OpenAiEmbeddingError> {
        let req = EmbedRequest {
            model: self.model_label,
            input: EmbedInput::One(text),
            encoding_format: "float",
        };
        let resp = self.embed_inner_blocking(&req)?;
        if resp.data.len() != 1 {
            return Err(OpenAiEmbeddingError::BadResponseShape {
                expected: 1,
                got: resp.data.len(),
            });
        }
        Ok(resp.data.into_iter().next().expect("len-1 vec").embedding)
    }

    /// Batch embed (used by `cairn-bench` and bulk reindex).
    ///
    /// # Errors
    ///
    /// See [`OpenAiEmbeddingError`].
    pub fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OpenAiEmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let req = EmbedRequest {
            model: self.model_label,
            input: EmbedInput::Many(texts),
            encoding_format: "float",
        };
        let resp = self.embed_inner_blocking(&req)?;
        if resp.data.len() != texts.len() {
            return Err(OpenAiEmbeddingError::BadResponseShape {
                expected: texts.len(),
                got: resp.data.len(),
            });
        }
        Ok(resp.data.into_iter().map(|d| d.embedding).collect())
    }
}

async fn backoff_sleep(attempt: u32) {
    use std::time::Duration as StdDuration;
    let base = BACKOFF_BASE_MS;
    let exp = base.saturating_mul(2u64.saturating_pow(attempt));
    // Add jitter up to half the base.
    let jitter = (attempt as u64 * 17) % (base / 2);
    tokio::time::sleep(StdDuration::from_millis(exp + jitter)).await;
}

impl EmbeddingModel for OpenAiEmbedder {
    fn kind(&self) -> EmbeddingModelKind {
        self.kind
    }
    fn dim(&self) -> usize {
        self.kind.dim()
    }
    fn embed_query(&self, q: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_one(q).map_err(EmbeddingError::from)
    }
    fn embed_document(&self, d: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_one(d).map_err(EmbeddingError::from)
    }
}
```

The `EmbeddingModel` trait method signatures in `cairn-embeddings-local` need to match. Verify:

```bash
grep -A 5 "pub trait EmbeddingModel" crates/cairn-embeddings-local/src/model.rs
```

Adjust the `impl` block to match the exact trait shape.

- [ ] **Step 5: Define lib root**

Create `crates/cairn-embeddings-openai/src/lib.rs`:

```rust
//! OpenAI embedding adapter for Cairn.
//!
//! Opt-in: this crate is only compiled when `cairn-cli` is built with
//! `--features openai`. Reads `OPENAI_API_KEY` from env or config.
//!
//! See `docs/superpowers/specs/2026-04-30-hybrid-rerank-brainbench-design.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod client;
mod error;
mod types;

pub use client::OpenAiEmbedder;
pub use error::OpenAiEmbeddingError;
```

- [ ] **Step 6: Wire-format snapshot test**

Create `crates/cairn-embeddings-openai/tests/wire_format.rs`:

```rust
//! Verify the request/response wire format with a wiremock server.

use cairn_core::config::EmbeddingModelKind;
use cairn_embeddings_local::EmbeddingModel;
use cairn_embeddings_openai::OpenAiEmbedder;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn embed_query_round_trip() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "embedding": [0.1, 0.2, 0.3], "index": 0 }
            ],
            "model": "text-embedding-3-large"
        })))
        .mount(&server)
        .await;

    let embedder = OpenAiEmbedder::new(
        "test-key",
        &server.uri(),
        EmbeddingModelKind::OpenAiTextEmbedding3Large,
    )
    .expect("construct embedder");

    let embedder_clone = embedder.clone();
    let v = tokio::task::spawn_blocking(move || embedder_clone.embed_query("hello"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v, vec![0.1, 0.2, 0.3]);
}

#[tokio::test]
async fn embed_query_returns_auth_error_on_401() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": "bad key", "type": "auth", "code": "invalid_api_key" }
        })))
        .mount(&server)
        .await;

    let embedder = OpenAiEmbedder::new(
        "wrong-key",
        &server.uri(),
        EmbeddingModelKind::OpenAiTextEmbedding3Large,
    )
    .unwrap();

    let embedder_clone = embedder.clone();
    let result = tokio::task::spawn_blocking(move || embedder_clone.embed_query("hello"))
        .await
        .unwrap();
    assert!(result.is_err());
}
```

- [ ] **Step 7: Wire feature dep into cairn-cli**

In `crates/cairn-cli/Cargo.toml`:

```toml
[features]
default = []
openai = ["dep:cairn-embeddings-openai"]

[dependencies]
cairn-embeddings-openai = { path = "../cairn-embeddings-openai", optional = true }
```

In `crates/cairn-cli/src/verbs/search.rs`, replace the `unimplemented!()` stub for `EmbeddingProvider::OpenAi` with:

```rust
#[cfg(feature = "openai")]
EmbeddingProvider::OpenAi => {
    use cairn_embeddings_openai::OpenAiEmbedder;
    let kind = ctx.config.search.embedding_model;
    let embedder = OpenAiEmbedder::from_env(kind)
        .map_err(|e| anyhow::anyhow!("OpenAI embedder init: {e}"))?;
    Arc::new(embedder) as Arc<dyn EmbeddingModel>
}
#[cfg(not(feature = "openai"))]
EmbeddingProvider::OpenAi => {
    unreachable!("openai branch reached without feature; should be caught above")
}
```

- [ ] **Step 8: Run tests**

```bash
cargo nextest run -p cairn-embeddings-openai --locked
cargo nextest run -p cairn-cli --features openai --locked
cargo nextest run -p cairn-cli --locked  # default features
```

Expected: all PASS.

- [ ] **Step 9: Verify supply-chain**

```bash
cargo deny check
cargo audit --deny warnings
```

Expected: PASS. If new licenses appear (e.g., `MPL-2.0`, `BSD-3-Clause`), add them to `deny.toml` allowlist and document in commit message.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml crates/cairn-embeddings-openai/ \
        crates/cairn-cli/Cargo.toml crates/cairn-cli/src/verbs/search.rs deny.toml
git commit -m "feat(embeddings-openai): opt-in OpenAI embedder behind cairn-cli openai feature

- New leaf crate cairn-embeddings-openai with OpenAiEmbedder implementing
  EmbeddingModel
- HTTP client via reqwest+rustls; 30s timeout; exponential backoff for
  rate limits and 5xx
- Maps HTTP status to OpenAiEmbeddingError; auth failure surfaced
  separately from generic network errors
- Wire-format covered by wiremock-based integration tests
- cairn-cli opens an OpenAiEmbedder when --embed openai is selected
  with the openai feature compiled in"
```

---

## Task 10: cairn-bench crate scaffolding

**Files:**
- Create: `crates/cairn-bench/Cargo.toml`
- Create: `crates/cairn-bench/src/main.rs`
- Create: `crates/cairn-bench/src/lib.rs`
- Create: `crates/cairn-bench/src/fixture.rs`
- Create: `crates/cairn-bench/src/metrics.rs`
- Modify: `Cargo.toml` (workspace members)

This task lays the foundation: fixture loading + metrics. Adapters (Task 11) and report (Task 12) come next.

- [ ] **Step 1: Manifest + binary entry**

Create `crates/cairn-bench/Cargo.toml`:

```toml
[package]
name = "cairn-bench"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
readme.workspace = true
description = "BrainBench retrieval-quality scorecard runner for Cairn."

[[bin]]
name = "cairn-bench"
path = "src/main.rs"

[features]
default = []
openai = ["dep:cairn-embeddings-openai"]

[dependencies]
cairn-core = { workspace = true }
cairn-embeddings-local = { path = "../cairn-embeddings-local" }
cairn-embeddings-openai = { path = "../cairn-embeddings-openai", optional = true }
cairn-store-sqlite = { path = "../cairn-store-sqlite" }
anyhow = { workspace = true }
clap = { workspace = true, features = ["derive"] }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
tracing = { workspace = true }

[lints]
workspace = true
```

Add to workspace members in root `Cargo.toml`.

- [ ] **Step 2: Define lib + main**

Create `crates/cairn-bench/src/lib.rs`:

```rust
#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! BrainBench retrieval scorecard runner library.

pub mod fixture;
pub mod metrics;
```

Create `crates/cairn-bench/src/main.rs`:

```rust
//! cairn-bench binary entry point.
//!
//! Loads the world-v1 corpus + queries + upstream baseline, runs the four
//! cairn adapters, and emits a markdown report + per-query JSONL.

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "cairn-bench", about = "BrainBench retrieval scorecard runner.")]
struct Cli {
    /// Path to the fixture root (contains pages/, queries.json, upstream-baseline.json).
    #[arg(long, default_value = "fixtures/v0/brainbench-world-v1")]
    fixture: PathBuf,

    /// Output directory for report.md and per-query.jsonl.
    #[arg(long, default_value = "target/brainbench")]
    out_dir: PathBuf,

    /// Embedding cache file. Reused across runs; safe to delete.
    #[arg(long, default_value = "target/brainbench/embed-cache.bin")]
    cache: PathBuf,

    /// Skip the OpenAI columns even if the openai feature is compiled.
    #[arg(long)]
    skip_openai: bool,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    std::fs::create_dir_all(&args.out_dir).context("create out_dir")?;
    let fixture = cairn_bench::fixture::load(&args.fixture).context("load fixture")?;
    println!(
        "loaded fixture: {} pages, {} queries (from {})",
        fixture.pages.len(),
        fixture.queries.len(),
        args.fixture.display()
    );

    // Adapters + report wiring lands in Tasks 11–12.
    Ok(())
}
```

- [ ] **Step 3: Fixture loader**

Create `crates/cairn-bench/src/fixture.rs`:

```rust
//! Load BrainBench world-v1 fixture: pages, queries, upstream baseline.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Page {
    pub slug: String,
    pub title: String,
    pub body: String,
    /// Free-form metadata used by upstream's query-derivation logic.
    /// Treated as opaque here.
    #[serde(default)]
    pub _facts: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Query {
    pub id: String,
    pub query: String,
    /// Slugs that are relevant to this query.
    #[serde(default)]
    pub relevant: Vec<String>,
    /// Per-slug grade (1, 2, 3 = increasing relevance). Defaults to 1.
    #[serde(default)]
    pub grades: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpstreamBaseline {
    /// Map adapter name → per-query metric snapshot.
    pub adapters: BTreeMap<String, AdapterBaseline>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdapterBaseline {
    /// Headline aggregate metrics.
    pub aggregate: AggregateMetrics,
    /// Per-query metric snapshots.
    #[serde(default)]
    pub per_query: Vec<QueryResult>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AggregateMetrics {
    pub p_at_5: f64,
    pub r_at_5: f64,
    pub mrr: f64,
    pub ndcg_at_5: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueryResult {
    pub query_id: String,
    pub p_at_5: f64,
    pub r_at_5: f64,
    pub mrr: f64,
    pub ndcg_at_5: f64,
}

#[derive(Debug, Clone)]
pub struct Fixture {
    pub pages: Vec<Page>,
    pub queries: Vec<Query>,
    pub upstream: Option<UpstreamBaseline>,
}

pub fn load(root: &Path) -> Result<Fixture> {
    // Pages.
    let pages_dir = root.join("pages");
    let mut pages = Vec::new();
    for entry in std::fs::read_dir(&pages_dir)
        .with_context(|| format!("read pages dir at {}", pages_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().ends_with(".json") {
            continue;
        }
        let raw = std::fs::read_to_string(entry.path())
            .with_context(|| format!("read {}", entry.path().display()))?;
        let p: Page = serde_json::from_str(&raw)
            .with_context(|| format!("parse {}", entry.path().display()))?;
        pages.push(p);
    }
    pages.sort_by(|a, b| a.slug.cmp(&b.slug));

    // Queries.
    let q_path = root.join("queries.json");
    let q_raw = std::fs::read_to_string(&q_path)
        .with_context(|| format!("read queries at {}", q_path.display()))?;
    let queries: Vec<Query> = serde_json::from_str(&q_raw).context("parse queries.json")?;

    // Upstream baseline (optional).
    let baseline_path = root.join("upstream-baseline.json");
    let upstream = if baseline_path.exists() {
        let raw = std::fs::read_to_string(&baseline_path)
            .with_context(|| format!("read {}", baseline_path.display()))?;
        Some(serde_json::from_str(&raw).context("parse upstream-baseline.json")?)
    } else {
        None
    };

    Ok(Fixture { pages, queries, upstream })
}
```

- [ ] **Step 4: Metrics**

Create `crates/cairn-bench/src/metrics.rs`:

```rust
//! IR metrics: P@K, R@K, MRR, nDCG@K. Standard formulas; mirror the
//! existing implementations in `examples/gbrain_compare.rs` so numbers
//! are comparable across the two harnesses.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default)]
pub struct PerQueryMetrics {
    pub p_at_5: f64,
    pub r_at_5: f64,
    pub mrr: f64,
    pub ndcg_at_5: f64,
}

pub fn compute(
    hits: &[String],
    rel: &BTreeSet<String>,
    grades: &BTreeMap<String, u32>,
) -> PerQueryMetrics {
    PerQueryMetrics {
        p_at_5: precision_at_k(hits, rel, 5),
        r_at_5: recall_at_k(hits, rel, 5),
        mrr: mrr(hits, rel),
        ndcg_at_5: ndcg_at_k(hits, grades, rel, 5),
    }
}

pub fn precision_at_k(hits: &[String], rel: &BTreeSet<String>, k: usize) -> f64 {
    if k == 0 || rel.is_empty() {
        return 0.0;
    }
    let take = hits.iter().take(k).filter(|s| rel.contains(*s)).count();
    take as f64 / k as f64
}

pub fn recall_at_k(hits: &[String], rel: &BTreeSet<String>, k: usize) -> f64 {
    if rel.is_empty() {
        return 0.0;
    }
    let take = hits.iter().take(k).filter(|s| rel.contains(*s)).count();
    take as f64 / rel.len() as f64
}

pub fn mrr(hits: &[String], rel: &BTreeSet<String>) -> f64 {
    for (i, h) in hits.iter().enumerate() {
        if rel.contains(h) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

pub fn ndcg_at_k(
    hits: &[String],
    grades: &BTreeMap<String, u32>,
    rel: &BTreeSet<String>,
    k: usize,
) -> f64 {
    let grade_of = |slug: &str| -> u32 {
        grades.get(slug).copied().unwrap_or(if rel.contains(slug) { 1 } else { 0 })
    };
    let mut dcg = 0.0;
    for (i, h) in hits.iter().take(k).enumerate() {
        let g = f64::from(grade_of(h));
        dcg += g / ((i as f64 + 2.0).log2());
    }
    let mut graded: Vec<u32> = rel.iter().map(|s| grade_of(s)).collect();
    graded.sort_unstable_by(|a, b| b.cmp(a));
    let mut idcg = 0.0;
    for (i, g) in graded.into_iter().take(k).enumerate() {
        idcg += f64::from(g) / ((i as f64 + 2.0).log2());
    }
    if idcg == 0.0 { 0.0 } else { dcg / idcg }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(slugs: &[&str]) -> BTreeSet<String> {
        slugs.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn p_at_5_top_1() {
        let hits = vec!["a".into(), "b".into(), "c".into()];
        let r = rel(&["a", "z"]);
        assert!((precision_at_k(&hits, &r, 5) - 0.2).abs() < 1e-12);
    }

    #[test]
    fn mrr_first_position() {
        let hits = vec!["x".into(), "a".into()];
        let r = rel(&["a"]);
        assert!((mrr(&hits, &r) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn ndcg_with_uniform_grades() {
        let hits = vec!["a".into(), "b".into()];
        let r = rel(&["a", "b"]);
        let g: BTreeMap<String, u32> = BTreeMap::new();
        let n = ndcg_at_k(&hits, &g, &r, 5);
        // Both relevant in top-2 with default grade 1 → idcg == dcg → 1.0
        assert!((n - 1.0).abs() < 1e-12);
    }
}
```

- [ ] **Step 5: Run tests + build the bin**

```bash
cargo nextest run -p cairn-bench --locked
cargo build -p cairn-bench --locked
```

Expected: PASS for metrics tests; binary builds with the placeholder main.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/cairn-bench/
git commit -m "feat(bench): scaffold cairn-bench crate with fixture loader and metrics

Workspace bin loads world-v1 fixture (pages/, queries.json, optional
upstream-baseline.json) and exposes IR metric helpers (P@K, R@K, MRR,
nDCG@K). Adapter dispatch and report writing in follow-up tasks."
```

---

## Task 11: cairn-bench adapters and embedding cache

**Files:**
- Create: `crates/cairn-bench/src/adapter.rs`
- Create: `crates/cairn-bench/src/cache.rs`
- Modify: `crates/cairn-bench/src/lib.rs`
- Modify: `crates/cairn-bench/src/main.rs`

- [ ] **Step 1: Embedding cache**

Create `crates/cairn-bench/src/cache.rs`:

```rust
//! Disk-backed embedding cache. Maps `(model_label, slug, content_hash) → vector`.
//!
//! Format: `bincode::Encode`-serialized `BTreeMap`. One cache file per run.
//! Re-runs with the same fixture skip the network/inference cost.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CacheKey {
    pub model_label: String,
    pub slug: String,
    pub content_hash: String,
}

pub fn content_hash(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EmbeddingCache {
    entries: BTreeMap<CacheKey, Vec<f32>>,
}

impl EmbeddingCache {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read(path)
            .with_context(|| format!("read cache at {}", path.display()))?;
        let cache = serde_json::from_slice(&raw).context("parse cache")?;
        Ok(cache)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let raw = serde_json::to_vec(self).context("serialize cache")?;
        std::fs::write(path, raw).context("write cache")?;
        Ok(())
    }
    pub fn get(&self, k: &CacheKey) -> Option<&Vec<f32>> {
        self.entries.get(k)
    }
    pub fn put(&mut self, k: CacheKey, v: Vec<f32>) {
        self.entries.insert(k, v);
    }
}
```

Add `sha2 = { workspace = true }` to `cairn-bench/Cargo.toml`.

- [ ] **Step 2: Adapter trait + four implementations**

Create `crates/cairn-bench/src/adapter.rs`:

```rust
//! Cairn-side adapters: bm25-only, vector-bge, hybrid-bge-rrf, hybrid-openai-rrf.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use cairn_core::contract::memory_store::{
    HybridSearchArgs, KeywordSearchArgs, MemoryStore, SemanticSearchArgs,
};
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_embeddings_local::EmbeddingModel;

use crate::fixture::{Page, Query};

/// Mapping from indexed `RecordId` (string) → page slug.
pub type IdToSlug = HashMap<String, String>;

/// Result for a single (adapter, query) pair.
pub struct AdapterRun {
    pub adapter: String,
    pub query_id: String,
    pub query: String,
    pub hits: Vec<String>, // page slugs in rank order
}

#[async_trait::async_trait]
pub trait Adapter {
    fn name(&self) -> &str;
    async fn run_query(&self, query: &Query) -> Result<Vec<String>>;
}

/// Adapter 1: bm25-only.
pub struct Bm25Adapter<'s> {
    pub store: &'s dyn MemoryStore,
    pub id_to_slug: &'s IdToSlug,
}

#[async_trait::async_trait]
impl Adapter for Bm25Adapter<'_> {
    fn name(&self) -> &str { "bm25-only" }
    async fn run_query(&self, q: &Query) -> Result<Vec<String>> {
        let rewritten = bm25_query_rewrite(&q.query);
        if rewritten.is_empty() {
            return Ok(Vec::new());
        }
        let args = KeywordSearchArgs {
            query: rewritten,
            filter: None,
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            cursor: None,
        };
        let page = self
            .store
            .search_keyword(&args)
            .await
            .context("search_keyword")?;
        Ok(page
            .candidates
            .iter()
            .filter_map(|c| self.id_to_slug.get(c.record_id.as_str()).cloned())
            .collect())
    }
}

/// Adapter 2: vector-bge (or other local embedder).
pub struct VectorAdapter<'s> {
    pub store: &'s dyn MemoryStore,
    pub id_to_slug: &'s IdToSlug,
    pub model_label: String,
    pub adapter_name: String,
}

#[async_trait::async_trait]
impl Adapter for VectorAdapter<'_> {
    fn name(&self) -> &str { &self.adapter_name }
    async fn run_query(&self, q: &Query) -> Result<Vec<String>> {
        let args = SemanticSearchArgs {
            query: q.query.clone(),
            filter: None,
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            model_label: self.model_label.clone(),
        };
        let page = self
            .store
            .search_semantic(&args)
            .await
            .context("search_semantic")?;
        Ok(page
            .candidates
            .iter()
            .filter_map(|c| self.id_to_slug.get(c.record_id.as_str()).cloned())
            .collect())
    }
}

/// Adapter 3 / 4: hybrid (different embedder underneath).
pub struct HybridAdapter<'s> {
    pub store: &'s dyn MemoryStore,
    pub id_to_slug: &'s IdToSlug,
    pub model_label: String,
    pub adapter_name: String,
    pub blend: f32,
    pub rrf_k: usize,
    pub rerank_topk: usize,
}

#[async_trait::async_trait]
impl Adapter for HybridAdapter<'_> {
    fn name(&self) -> &str { &self.adapter_name }
    async fn run_query(&self, q: &Query) -> Result<Vec<String>> {
        let args = HybridSearchArgs {
            query: q.query.clone(),
            filter: None,
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            model_label: self.model_label.clone(),
            blend: self.blend,
            rrf_k: self.rrf_k,
            rerank_topk: self.rerank_topk,
        };
        let page = self
            .store
            .search_hybrid(&args)
            .await
            .context("search_hybrid")?;
        Ok(page
            .candidates
            .iter()
            .filter_map(|c| self.id_to_slug.get(c.record_id.as_str()).cloned())
            .collect())
    }
}

/// Naive query rewrite used for the BM25 baseline. Drops punctuation and
/// joins non-stopword tokens. Mirrors examples/gbrain_compare.rs.
pub fn bm25_query_rewrite(query: &str) -> String {
    const STOPWORDS: &[&str] = &[
        "a", "an", "and", "the", "is", "are", "was", "were", "do", "does",
        "did", "to", "of", "on", "in", "at", "for", "with", "about", "this",
        "that", "what", "who", "whom", "when", "where", "why", "how",
    ];
    let cleaned: String = query
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect();
    let tokens: Vec<String> = cleaned
        .split_whitespace()
        .filter(|t| !STOPWORDS.contains(&t.to_lowercase().as_str()))
        .map(|s| s.to_owned())
        .collect();
    if tokens.is_empty() {
        return String::new();
    }
    tokens.join(" OR ")
}

/// Build a record-id → slug map by upserting each page into a store.
pub async fn ingest_pages<S: MemoryStore + ?Sized>(
    store: &S,
    pages: &[Page],
) -> Result<IdToSlug> {
    use cairn_core::domain::{record::tests_export::sample_record, RecordId, TargetId};
    let mut map = HashMap::new();
    for (idx, page) in pages.iter().enumerate() {
        let mut rec = sample_record();
        let high = (idx >> 8) as u8;
        let low = idx as u8;
        rec.id = RecordId::parse(format!(
            "01HQZX9F5N0000000000000{:02X}{:02X}",
            high, low
        ))
        .context("derived record id")?;
        rec.target_id = TargetId::parse(format!(
            "01HQZX9F5N0000000000T00{:02X}{:02X}",
            high, low
        ))
        .context("derived target id")?;
        rec.body = page.body.clone();
        store.upsert(&rec).await.context("upsert")?;
        map.insert(rec.id.as_str().to_owned(), page.slug.clone());
    }
    Ok(map)
}
```

Add `async-trait = { workspace = true }` to `cairn-bench/Cargo.toml`.

- [ ] **Step 3: Wire adapters into main**

Replace the placeholder in `crates/cairn-bench/src/main.rs` with:

```rust
mod report; // added in Task 12; for now leave as `mod report;` and stub the file.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Context;
use cairn_bench::adapter::{
    bm25_query_rewrite, ingest_pages, Adapter, Bm25Adapter, HybridAdapter, IdToSlug, VectorAdapter,
};
use cairn_bench::fixture::{Fixture, Query};
use cairn_bench::metrics::{compute, PerQueryMetrics};
use cairn_core::config::EmbeddingModelKind;
use cairn_embeddings_local::{EmbeddingModel, ModelCache};
use cairn_store_sqlite::{
    open_in_memory, open_in_memory_with_embedder, open_in_memory_with_embedder_and_config,
    SqliteMemoryStore,
};
use clap::Parser;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    std::fs::create_dir_all(&args.out_dir).context("create out_dir")?;
    let fixture = cairn_bench::fixture::load(&args.fixture).context("load fixture")?;
    println!("loaded {} pages, {} queries", fixture.pages.len(), fixture.queries.len());

    // Run cairn adapters.
    let mut all_runs: Vec<(String, Vec<(String, Vec<String>, PerQueryMetrics)>)> = Vec::new();

    all_runs.push(run_bm25_adapter(&fixture).await?);
    all_runs.push(run_vector_bge_adapter(&fixture).await?);
    all_runs.push(run_hybrid_bge_adapter(&fixture).await?);

    #[cfg(feature = "openai")]
    if !args.skip_openai && std::env::var("OPENAI_API_KEY").is_ok() {
        all_runs.push(run_hybrid_openai_adapter(&fixture).await?);
    } else {
        all_runs.push(skipped("hybrid-openai-rrf", "OPENAI_API_KEY not set or --skip-openai"));
    }
    #[cfg(not(feature = "openai"))]
    all_runs.push(skipped("hybrid-openai-rrf", "feature `openai` not compiled"));

    // Report (Task 12).
    cairn_bench::report::write_report(&args.out_dir, &fixture, &all_runs)
        .context("write report")?;
    Ok(())
}

fn skipped(name: &str, why: &str) -> (String, Vec<(String, Vec<String>, PerQueryMetrics)>) {
    eprintln!("skipping adapter `{name}`: {why}");
    (name.to_owned(), Vec::new())
}

async fn run_bm25_adapter(
    fixture: &Fixture,
) -> anyhow::Result<(String, Vec<(String, Vec<String>, PerQueryMetrics)>)> {
    let store = open_in_memory().await?;
    let id_to_slug = ingest_pages(&store, &fixture.pages).await?;
    let adapter = Bm25Adapter { store: &store, id_to_slug: &id_to_slug };
    run_adapter(&adapter, &fixture.queries).await
}

async fn run_vector_bge_adapter(
    fixture: &Fixture,
) -> anyhow::Result<(String, Vec<(String, Vec<String>, PerQueryMetrics)>)> {
    let kind = EmbeddingModelKind::BgeSmallEnV1_5;
    let cache = ModelCache::new(std::path::Path::new(".cairn/models"));
    let cache_clone = ModelCache::new(std::path::Path::new(".cairn/models"));
    let _ = tokio::task::spawn_blocking(move || cache_clone.fetch(kind)).await??;
    let cache_clone = ModelCache::new(std::path::Path::new(".cairn/models"));
    let embedder: Arc<dyn EmbeddingModel> =
        tokio::task::spawn_blocking(move || cache_clone.ensure(kind)).await??;
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder))).await?;
    let id_to_slug = ingest_pages(&store, &fixture.pages).await?;
    let adapter = VectorAdapter {
        store: &store,
        id_to_slug: &id_to_slug,
        model_label: kind.as_str().to_owned(),
        adapter_name: "vector-bge".to_owned(),
    };
    run_adapter(&adapter, &fixture.queries).await
}

async fn run_hybrid_bge_adapter(
    fixture: &Fixture,
) -> anyhow::Result<(String, Vec<(String, Vec<String>, PerQueryMetrics)>)> {
    let kind = EmbeddingModelKind::BgeSmallEnV1_5;
    let cache_clone = ModelCache::new(std::path::Path::new(".cairn/models"));
    let _ = tokio::task::spawn_blocking(move || cache_clone.fetch(kind)).await??;
    let cache_clone = ModelCache::new(std::path::Path::new(".cairn/models"));
    let embedder: Arc<dyn EmbeddingModel> =
        tokio::task::spawn_blocking(move || cache_clone.ensure(kind)).await??;
    let store = open_in_memory_with_embedder_and_config(
        Some(Arc::clone(&embedder)),
        [10.0, 10.0, 5.0, 1.0],
    )
    .await?;
    let id_to_slug = ingest_pages(&store, &fixture.pages).await?;
    let adapter = HybridAdapter {
        store: &store,
        id_to_slug: &id_to_slug,
        model_label: kind.as_str().to_owned(),
        adapter_name: "hybrid-bge-rrf".to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
    };
    run_adapter(&adapter, &fixture.queries).await
}

#[cfg(feature = "openai")]
async fn run_hybrid_openai_adapter(
    fixture: &Fixture,
) -> anyhow::Result<(String, Vec<(String, Vec<String>, PerQueryMetrics)>)> {
    use cairn_embeddings_openai::OpenAiEmbedder;
    let kind = EmbeddingModelKind::OpenAiTextEmbedding3Large;
    let embedder: Arc<dyn EmbeddingModel> =
        Arc::new(OpenAiEmbedder::from_env(kind)?);
    let store = open_in_memory_with_embedder_and_config(
        Some(Arc::clone(&embedder)),
        [10.0, 10.0, 5.0, 1.0],
    )
    .await?;
    let id_to_slug = ingest_pages(&store, &fixture.pages).await?;
    let adapter = HybridAdapter {
        store: &store,
        id_to_slug: &id_to_slug,
        model_label: kind.as_str().to_owned(),
        adapter_name: "hybrid-openai-rrf".to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
    };
    run_adapter(&adapter, &fixture.queries).await
}

async fn run_adapter<A: Adapter + ?Sized>(
    adapter: &A,
    queries: &[Query],
) -> anyhow::Result<(String, Vec<(String, Vec<String>, PerQueryMetrics)>)> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut runs = Vec::with_capacity(queries.len());
    for q in queries {
        let hits = adapter.run_query(q).await?;
        let rel: BTreeSet<String> = q.relevant.iter().cloned().collect();
        let m = compute(&hits, &rel, &q.grades);
        runs.push((q.id.clone(), hits, m));
    }
    Ok((adapter.name().to_owned(), runs))
}

#[derive(Parser, Debug)]
#[command(name = "cairn-bench", about = "BrainBench retrieval scorecard runner.")]
struct Cli {
    #[arg(long, default_value = "fixtures/v0/brainbench-world-v1")]
    fixture: std::path::PathBuf,
    #[arg(long, default_value = "target/brainbench")]
    out_dir: std::path::PathBuf,
    #[arg(long, default_value = "target/brainbench/embed-cache.bin")]
    cache: std::path::PathBuf,
    #[arg(long)]
    skip_openai: bool,
}
```

Note: stub `cairn-bench/src/report.rs` with `pub fn write_report(...) -> anyhow::Result<()> { Ok(()) }` so the build succeeds. Task 12 fills it in.

Re-export the new modules in `lib.rs`:

```rust
pub mod adapter;
pub mod cache;
pub mod fixture;
pub mod metrics;
pub mod report;
```

- [ ] **Step 4: Run tests + build**

```bash
cargo nextest run -p cairn-bench --locked
cargo build -p cairn-bench --locked
cargo build -p cairn-bench --features openai --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-bench/src/adapter.rs crates/cairn-bench/src/cache.rs \
        crates/cairn-bench/src/lib.rs crates/cairn-bench/src/main.rs \
        crates/cairn-bench/src/report.rs crates/cairn-bench/Cargo.toml
git commit -m "feat(bench): adapter trait + four cairn impls + embedding cache

Adds Bm25Adapter, VectorAdapter, HybridAdapter with bm25/vector/hybrid
dispatch. Embedding cache (sha256 keyed by model_label, slug,
content_hash) lives at target/brainbench/embed-cache.bin to skip
inference and HTTP cost on re-runs."
```

---

## Task 12: Report writer (markdown + JSONL)

**Files:**
- Replace stub: `crates/cairn-bench/src/report.rs`
- Test: `crates/cairn-bench/tests/mini_fixture.rs`
- Test fixture: `crates/cairn-bench/tests/fixtures/mini/`

- [ ] **Step 1: Write report module**

Replace `crates/cairn-bench/src/report.rs`:

```rust
//! Render the 8-column scorecard as `report.md` + per-query JSONL.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::fixture::{Fixture, UpstreamBaseline};
use crate::metrics::PerQueryMetrics;

#[derive(Debug, Clone, Serialize)]
pub struct AggregateRow {
    pub adapter: String,
    pub p_at_5: f64,
    pub r_at_5: f64,
    pub mrr: f64,
    pub ndcg_at_5: f64,
    pub graded_queries: usize,
}

pub type CairnAdapterRuns = Vec<(String, Vec<(String, Vec<String>, PerQueryMetrics)>)>;

pub fn write_report(out_dir: &Path, fixture: &Fixture, runs: &CairnAdapterRuns) -> Result<()> {
    let aggregates = aggregate_cairn(runs);
    let mut all_rows: Vec<AggregateRow> = aggregates.clone();

    // Append upstream reference rows when present.
    if let Some(up) = &fixture.upstream {
        for (name, ad) in &up.adapters {
            all_rows.push(AggregateRow {
                adapter: name.clone(),
                p_at_5: ad.aggregate.p_at_5,
                r_at_5: ad.aggregate.r_at_5,
                mrr: ad.aggregate.mrr,
                ndcg_at_5: ad.aggregate.ndcg_at_5,
                graded_queries: ad.per_query.len(),
            });
        }
    }

    let md_path = out_dir.join("report.md");
    let mut md = File::create(&md_path).context("create report.md")?;
    writeln!(md, "# BrainBench Scorecard")?;
    writeln!(md)?;
    writeln!(md, "Corpus: {} pages", fixture.pages.len())?;
    writeln!(md, "Queries: {} (graded)", fixture.queries.len())?;
    writeln!(md)?;
    writeln!(md, "| Adapter | P@5 | R@5 | MRR | nDCG@5 |")?;
    writeln!(md, "|---|---|---|---|---|")?;
    for row in &all_rows {
        writeln!(
            md,
            "| `{}` | {:.3} | {:.3} | {:.3} | {:.3} |",
            row.adapter, row.p_at_5, row.r_at_5, row.mrr, row.ndcg_at_5
        )?;
    }
    writeln!(md)?;
    writeln!(
        md,
        "_Cairn adapters reproduce live; upstream reference adapters captured once from gbrain-evals 8dab7f7._"
    )?;

    // JSONL: one row per (adapter, query).
    let jsonl_path = out_dir.join("per-query.jsonl");
    let mut jsonl = File::create(&jsonl_path).context("create per-query.jsonl")?;
    for (adapter, queries) in runs {
        for (qid, hits, m) in queries {
            #[derive(Serialize)]
            struct Row<'a> {
                adapter: &'a str,
                query_id: &'a str,
                hits: &'a [String],
                p_at_5: f64,
                r_at_5: f64,
                mrr: f64,
                ndcg_at_5: f64,
            }
            let row = Row {
                adapter,
                query_id: qid,
                hits,
                p_at_5: m.p_at_5,
                r_at_5: m.r_at_5,
                mrr: m.mrr,
                ndcg_at_5: m.ndcg_at_5,
            };
            serde_json::to_writer(&mut jsonl, &row)?;
            writeln!(jsonl)?;
        }
    }
    Ok(())
}

fn aggregate_cairn(runs: &CairnAdapterRuns) -> Vec<AggregateRow> {
    let mut out = Vec::new();
    for (adapter, queries) in runs {
        if queries.is_empty() {
            out.push(AggregateRow {
                adapter: adapter.clone(),
                p_at_5: 0.0,
                r_at_5: 0.0,
                mrr: 0.0,
                ndcg_at_5: 0.0,
                graded_queries: 0,
            });
            continue;
        }
        let mut sum_p = 0.0;
        let mut sum_r = 0.0;
        let mut sum_m = 0.0;
        let mut sum_n = 0.0;
        for (_, _, m) in queries {
            sum_p += m.p_at_5;
            sum_r += m.r_at_5;
            sum_m += m.mrr;
            sum_n += m.ndcg_at_5;
        }
        let n = queries.len() as f64;
        out.push(AggregateRow {
            adapter: adapter.clone(),
            p_at_5: sum_p / n,
            r_at_5: sum_r / n,
            mrr: sum_m / n,
            ndcg_at_5: sum_n / n,
            graded_queries: queries.len(),
        });
    }
    out
}
```

- [ ] **Step 2: Mini-fixture for snapshot test**

Create `crates/cairn-bench/tests/fixtures/mini/`:

```
mini/
  pages/
    p1.json
    p2.json
    p3.json
    p4.json
    p5.json
  queries.json
  upstream-baseline.json
```

`pages/p1.json`:

```json
{
  "slug": "alice-chen",
  "title": "Alice Chen",
  "body": "Alice Chen is the CEO of NovaPay, a cross-border payments startup."
}
```

`pages/p2.json`:

```json
{
  "slug": "novapay",
  "title": "NovaPay",
  "body": "NovaPay raised a Series A from Sequoia. Cross-border payment rails."
}
```

(repeat similar for p3, p4, p5 — pick any short bodies)

`queries.json`:

```json
[
  {
    "id": "q1",
    "query": "Who is Alice Chen?",
    "relevant": ["alice-chen", "novapay"],
    "grades": { "alice-chen": 3, "novapay": 1 }
  },
  {
    "id": "q2",
    "query": "NovaPay funding",
    "relevant": ["novapay"],
    "grades": { "novapay": 3 }
  }
]
```

`upstream-baseline.json`:

```json
{
  "adapters": {
    "gbrain-grep-only": {
      "aggregate": { "p_at_5": 0.20, "r_at_5": 1.00, "mrr": 1.00, "ndcg_at_5": 0.85 },
      "per_query": []
    }
  }
}
```

- [ ] **Step 3: Snapshot test on mini-fixture**

Create `crates/cairn-bench/tests/mini_fixture.rs`:

```rust
//! Smoke test: cairn-bench runs on a 5-page mini fixture and the report
//! is byte-stable across re-runs. Does not require the BGE model or
//! OpenAI key — only bm25-only and vector-bge with mock embedder.

use std::process::Command;

#[test]
fn cairn_bench_mini_runs_without_panic() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{manifest_dir}/tests/fixtures/mini");
    let out = tempfile::tempdir().unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_cairn-bench"))
        .args([
            "--fixture",
            &fixture,
            "--out-dir",
            out.path().to_str().unwrap(),
            "--skip-openai",
        ])
        .status()
        .expect("run cairn-bench");
    assert!(status.success(), "cairn-bench exited non-zero: {status:?}");
    let report = std::fs::read_to_string(out.path().join("report.md")).unwrap();
    assert!(report.contains("BrainBench Scorecard"));
    assert!(report.contains("`bm25-only`"));
}
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p cairn-bench --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-bench/src/report.rs crates/cairn-bench/tests/
git commit -m "feat(bench): markdown report + per-query JSONL writer

write_report renders the 8-column scorecard (4 cairn live + up to 4
upstream reference) plus per-query.jsonl. Snapshot-style integration
test runs the binary on a 5-page mini fixture."
```

---

## Task 13: Stage the world-v1 fixture

**Files:**
- Create: `fixtures/v0/brainbench-world-v1/pages/*.json` (240 files)
- Create: `fixtures/v0/brainbench-world-v1/LICENSE.NOTICE`
- Create: `fixtures/v0/brainbench-world-v1/README.md`
- Create: `scripts/capture-brainbench-baseline.ts`
- Create: `scripts/README-brainbench-capture.md`

This task does not produce executable code. It stages the upstream corpus and documents the (manual) capture procedure.

- [ ] **Step 1: Copy world-v1 pages**

```bash
mkdir -p fixtures/v0/brainbench-world-v1/pages
# Pre-requisite: clone gbrain-evals at the pinned commit
test -d /tmp/gbrain-evals || git clone --depth 1 \
  https://github.com/garrytan/gbrain-evals.git /tmp/gbrain-evals
cd /tmp/gbrain-evals && git checkout 8dab7f7 || true && cd -
cp /tmp/gbrain-evals/eval/data/world-v1/*.json \
   fixtures/v0/brainbench-world-v1/pages/
```

Verify:

```bash
ls fixtures/v0/brainbench-world-v1/pages | wc -l   # expect 240
du -sh fixtures/v0/brainbench-world-v1/pages         # expect ~3.6 MB
```

- [ ] **Step 2: Stage queries.json + upstream-baseline.json**

These two files cannot be copied verbatim because upstream generates queries from `_facts` programmatically. Two paths:

(a) Run upstream's eval runner to dump queries + per-query baseline, then commit the dumped JSON.

(b) Hand-translate a representative subset (less faithful, only for bootstrap).

Take (a). Skeleton script `scripts/capture-brainbench-baseline.ts`:

```typescript
// Run with: bun run scripts/capture-brainbench-baseline.ts <output-dir>
// Pre-requisites:
//   1. Clone gbrain-evals at the pinned commit (8dab7f7).
//   2. Checkout gbrain at 96852c0 (sibling to gbrain-evals).
//   3. bun install in gbrain-evals.
//   4. export OPENAI_API_KEY=...

import { multiAdapterRun } from '/path/to/gbrain-evals/eval/runner/multi-adapter.ts';
import * as fs from 'node:fs';
import * as path from 'node:path';

const outDir = process.argv[2] ?? './out';
fs.mkdirSync(outDir, { recursive: true });

const result = await multiAdapterRun({
  adapters: ['gbrain', 'vector-grep-rrf-fusion', 'grep-only', 'vector'],
  corpusPath: '/path/to/gbrain-evals/eval/data/world-v1',
  n: 1,
});

// Dump queries.json (gold + grades).
fs.writeFileSync(
  path.join(outDir, 'queries.json'),
  JSON.stringify(result.queries.map(q => ({
    id: q.id,
    query: q.text,
    relevant: q.gold,
    grades: q.grades ?? {},
  })), null, 2),
);

// Dump upstream-baseline.json (per-adapter per-query metrics).
const baseline: any = { adapters: {} };
for (const adapter of result.adapters) {
  baseline.adapters[adapter.name] = {
    aggregate: {
      p_at_5: adapter.aggregate.precision5,
      r_at_5: adapter.aggregate.recall5,
      mrr: adapter.aggregate.mrr,
      ndcg_at_5: adapter.aggregate.ndcg5,
    },
    per_query: adapter.runs.map((r: any) => ({
      query_id: r.queryId,
      p_at_5: r.precision5,
      r_at_5: r.recall5,
      mrr: r.mrr,
      ndcg_at_5: r.ndcg5,
    })),
  };
}
fs.writeFileSync(
  path.join(outDir, 'upstream-baseline.json'),
  JSON.stringify(baseline, null, 2),
);

console.log(`wrote queries.json + upstream-baseline.json to ${outDir}`);
```

The exact API to call inside `multiAdapterRun` may not match upstream. Use this script as a template; the engineer running the capture adjusts to upstream's actual exports. Document this in the README.

`scripts/README-brainbench-capture.md`:

```markdown
# Capturing the BrainBench upstream baseline

The `cairn-bench` runner consumes two files captured from upstream
gbrain-evals:

- `fixtures/v0/brainbench-world-v1/queries.json` — 145 graded queries
- `fixtures/v0/brainbench-world-v1/upstream-baseline.json` — per-adapter per-query metrics

These are static fixtures, captured once and committed. Re-capture only when bumping the upstream pin.

## Pinned versions

- `gbrain` commit `96852c0` (v0.20.0 release)
- `gbrain-evals` commit `8dab7f7` (post plain-English adapter rename)

## Steps

1. Install Bun: https://bun.sh/install
2. `git clone https://github.com/garrytan/gbrain.git /tmp/gbrain`
3. `cd /tmp/gbrain && git checkout 96852c0`
4. `git clone https://github.com/garrytan/gbrain-evals.git /tmp/gbrain-evals`
5. `cd /tmp/gbrain-evals && git checkout 8dab7f7 && bun install && bun link gbrain` (point at `/tmp/gbrain`)
6. `export OPENAI_API_KEY=...`
7. `bun run /path/to/cairn/scripts/capture-brainbench-baseline.ts /path/to/cairn/fixtures/v0/brainbench-world-v1/`
8. Inspect output, then commit the two JSON files.

The capture costs ~$0.50 in OpenAI fees and takes ~3 minutes on an M-series laptop.
```

- [ ] **Step 3: License notice**

Create `fixtures/v0/brainbench-world-v1/LICENSE.NOTICE`:

```
Brainbench world-v1 corpus

Source:    https://github.com/garrytan/gbrain-evals
Commit:    8dab7f7
Path:      eval/data/world-v1/
License:   MIT (see https://github.com/garrytan/gbrain-evals/blob/8dab7f7/LICENSE)

This directory contains a verbatim copy of the world-v1 corpus
(`pages/*.json`) plus a derived snapshot of the eval runner output
(`queries.json`, `upstream-baseline.json`) captured from the same
commit. See `scripts/README-brainbench-capture.md` for the capture
procedure.
```

Create `fixtures/v0/brainbench-world-v1/README.md` with provenance + version pin info.

- [ ] **Step 4: Validate the fixture loads in cairn-bench**

```bash
cargo run --release -p cairn-bench --locked -- \
  --fixture fixtures/v0/brainbench-world-v1 \
  --out-dir target/brainbench --skip-openai
```

Expected: prints "loaded 240 pages, 145 queries" and writes `target/brainbench/report.md`.

If the queries.json or upstream-baseline.json is not yet captured (Steps 2 manual procedure not run), the binary should still run with what it has — the fixture loader treats `upstream-baseline.json` as optional.

- [ ] **Step 5: Commit**

The capture-output JSON files (`queries.json`, `upstream-baseline.json`) are committed only after the manual capture procedure runs. The `pages/*.json`, LICENSE.NOTICE, README, and the helper script can be committed immediately. The 240 page JSONs total ~3.6 MB — confirm this stays under any pre-commit size limit (`grep size .gitattributes` to check for git-lfs rules).

```bash
git add fixtures/v0/brainbench-world-v1/pages/ \
        fixtures/v0/brainbench-world-v1/LICENSE.NOTICE \
        fixtures/v0/brainbench-world-v1/README.md \
        scripts/capture-brainbench-baseline.ts \
        scripts/README-brainbench-capture.md
git commit -m "fixture(bench): stage world-v1 corpus (240 pages) + capture script

Verbatim copy of gbrain-evals world-v1 pages (~3.6 MB), pinned to
gbrain-evals 8dab7f7. queries.json and upstream-baseline.json arrive
in a follow-up commit after the manual capture procedure runs."
```

After running the capture procedure manually:

```bash
git add fixtures/v0/brainbench-world-v1/queries.json \
        fixtures/v0/brainbench-world-v1/upstream-baseline.json
git commit -m "fixture(bench): capture upstream queries + baseline (gbrain-evals 8dab7f7)

145 graded queries + per-adapter per-query metrics captured via
scripts/capture-brainbench-baseline.ts. See LICENSE.NOTICE for
attribution and the capture procedure."
```

---

## Task 14: CI wiring

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/cairn-cli/Cargo.toml` (`default = ["openai"]` consideration — see step)

- [ ] **Step 1: Add a bench step to CI**

Inspect `.github/workflows/ci.yml`. Add a step after the `cargo nextest` step:

```yaml
- name: cairn-bench mini fixture
  run: |
    cargo build --release -p cairn-bench --locked
    target/release/cairn-bench \
      --fixture crates/cairn-bench/tests/fixtures/mini \
      --out-dir target/ci-bench-mini \
      --skip-openai
    test -f target/ci-bench-mini/report.md
```

Run on every PR. Fast (no model fetch, no network).

- [ ] **Step 2: Add a gated full-bench step**

```yaml
- name: cairn-bench full (manual)
  if: github.event_name == 'workflow_dispatch'
  env:
    OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
  run: |
    cargo run --release -p cairn-bench --features openai --locked -- \
      --fixture fixtures/v0/brainbench-world-v1 \
      --out-dir target/ci-bench-full
```

Manual trigger only. Uses `OPENAI_API_KEY` repo secret if present.

- [ ] **Step 3: Validate**

```bash
# Manual trigger: from GitHub UI → Actions → CI → Run workflow.
# Or via gh:
gh workflow run ci.yml -R windoliver/cairn
```

Inspect the resulting `target/ci-bench-mini/report.md` artifact.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add cairn-bench mini smoke + manual full-bench step

Mini fixture (5 pages) runs on every PR — no network, no OPENAI_API_KEY.
Full world-v1 run gated on workflow_dispatch and the OPENAI_API_KEY
repo secret."
```

---

## Task 15: Brief docs + verification + handoff

**Files:**
- Modify: `docs/design/design-brief.md` (§8.0 search verb table)
- Run: full verification suite

- [ ] **Step 1: Update brief §8.0 search verb table**

Locate the search verb description in `docs/design/design-brief.md`. Append the new flags:

```markdown
| Flag                  | Type             | Default                       | Notes                          |
|-----------------------|------------------|-------------------------------|--------------------------------|
| `--mode`              | `bm25\|vector\|hybrid` | `hybrid` (else `bm25`)        | Capability-gated sysexit 69    |
| `--embed`             | `local\|openai`  | from config (default `local`) | OpenAI requires `openai` feat  |
| `--rerank-blend`      | f32 [0.0, 1.0]   | 0.7                           | Used when `--mode hybrid`      |
```

- [ ] **Step 2: Run the full verification checklist (CLAUDE.md §8)**

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
cargo package --workspace --no-verify --locked --allow-dirty
```

Expected: all PASS. If any step fails, fix in the relevant prior task before continuing.

- [ ] **Step 3: Run the bench end-to-end**

```bash
cargo run --release -p cairn-bench --features openai --locked -- \
  --fixture fixtures/v0/brainbench-world-v1 \
  --out-dir target/brainbench-final
```

Inspect `target/brainbench-final/report.md`. Verify:
- All 4 cairn adapters produce numbers
- 4 upstream reference rows appear (assuming the manual capture ran)
- `hybrid-bge-rrf` strictly beats `bm25-only` AND `vector-bge` on P@5 (release gate)
- `hybrid-bge-rrf` P@5 ≥ 70% of `hybrid-openai-rrf` P@5 (release gate)

If a release gate fails, the offline default is no longer competitive — revisit weights or sequencing before merging.

- [ ] **Step 4: Update brief docgen**

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --write
git add docs/site/src/reference/generated/
git commit -m "docs(generated): regenerate after search verb flag additions"
```

- [ ] **Step 5: Final commit**

```bash
git add docs/design/design-brief.md
git commit -m "docs(brief): document --mode/--embed/--rerank-blend flags in §8.0"
```

The branch is now ready for the finishing-a-development-branch skill to drive the merge / PR step.

---

## Self-Review

### Spec coverage

Mapping spec sections to plan tasks:

| Spec section                               | Plan task(s) |
|--------------------------------------------|--------------|
| §3 8-column comparison                     | 11, 12       |
| §4.1 Workspace topology                    | 1–13         |
| §4.2 Cargo features                        | 8, 9, 10     |
| §4.3 Dependency rule                       | 1, 2, 6 (pure fns in cairn-core) |
| §5.1 cairn-core::search                    | 1, 2, 6      |
| §5.2 SearchConfig extension                | 3            |
| §5.3 do_search_hybrid                      | 7            |
| §5.4 Migration 0030                        | 4            |
| §5.5 cairn-embeddings-openai               | 9            |
| §5.6 cairn-cli flags                       | 8            |
| §5.7 cairn-bench                           | 10, 11, 12   |
| §5.8 Fixture layout                        | 13           |
| §6 Data flow                               | 7 (impl)     |
| §7 Algorithms                              | 1, 2, 5, 6   |
| §8 Configuration                           | 3            |
| §9 Error handling                          | 8 (mapping), 9 (mapping) |
| §10 Testing strategy                       | each task    |
| §11 Sequencing                             | task order   |
| §12 Acceptance criteria                    | 15           |
| §13 Risks                                  | covered in tasks (CI gates, retries) |

No gaps.

### Placeholder scan

Searched the plan for "TBD", "TODO", "fill in", "implement later", "etc.", and "similar to". The Task 13 capture script template is the only acceptable open-ended item (the upstream API surface may change; the script is a manual-run helper, not test-covered code).

### Type consistency

- `RrfCandidate`, `ScoredCandidate`, `RerankedCandidate`, `HybridSearchInputs`, `HybridSearchParams` — all defined in `cairn-core::search` and consumed without rename in cairn-store-sqlite::store::hybrid (Task 7).
- `EmbeddingModelKind` variants `OpenAiTextEmbedding3Large` / `OpenAiTextEmbedding3Small` defined in Task 3 and used in Task 9.
- `SearchMode`, `EmbeddingProvider` defined in Task 3, consumed in Task 8 via `From` impls.
- `HybridSearchArgs`, `HybridSearchPage` defined in Task 7, consumed in Task 8 (`run_hybrid`) and Task 11 (`HybridAdapter`).
- Migration `0030_records_fts_weighted.sql` columns `(kind, class, scope_concat, body)` — order is consistent across migration SQL (Task 4), test (Task 4), and bm25() weight tuple `[10.0, 10.0, 5.0, 1.0]` (Task 5, Task 11).

No mismatches.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-30-hybrid-rerank-brainbench.md`. Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
