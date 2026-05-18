# Issue #191 — RRF Hybrid Search w/ 1-hop Graph Expansion: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a third retrieval leg (1-hop entity-graph neighborhood expansion) to the existing 2-leg RRF hybrid search, with confidence-aware ranking, full bitemporal + auth + supersession correctness, and a two-tier connection pool with statement-scoped cancellation.

**Architecture:** Phased rollout across 6 PRs. Pure core types ship first (no I/O); contract bump (auth_scope) ships next behind a `0.4.0 → 0.5.0` major-version event; connection pool + cancellation primitives third; graph SQL fourth; orchestrator/verb/CLI/SDK fifth; bench gates last.

**Tech Stack:** Rust 1.95.0, tokio, rusqlite (with `carray` + `interrupt` + `progress_handler`), tokio_rusqlite, sqlite-vec, FTS5, schemars, bon (builders), insta (snapshots), criterion (bench), proptest, rstest.

**Spec:** `docs/superpowers/specs/2026-05-05-issue-191-rrf-hybrid-graph-design.md`

---

## Phase Decomposition

Each phase = one PR, independently testable, mergeable into `main`.

| # | Phase | Crates touched | Why ship-able alone |
|---|---|---|---|
| 1 | Pure types + RRF/cosine extensions | `cairn-core` | Adds `GraphCandidate`, `RankedCandidate`, `DegradedLeg`, `rrf_fusion_weighted`, graph-only cosine path. Pure functions, unit-testable. No callers yet — dead code with `#[allow(dead_code)]` until phase 5. |
| 2 | Contract bump: `auth_scope` + `MemoryStoreCapabilities::graph_search` + trait method | `cairn-core`, all in-tree adapters | Major version bump `0.4.0 → 0.5.0`. Threads `auth_scope` through Keyword/Semantic/Hybrid args. `search_graph_neighbors` returns `CapabilityUnavailable` until phase 4. Existing tests keep passing because legacy auth via `filter` is preserved as the only path used by SQLite store. |
| 3 | Two-tier connection pool + cancellation | `cairn-store-sqlite` | New `HybridConnectionPool` with Tier A (mandatory) + Tier B (graph). `interrupt()` + `progress_handler` per-checkout lifecycle. Existing keyword/semantic queries migrate onto Tier A. Capability probe extended. |
| 4 | Graph SQL: 1-hop traversal + supersession + bitemporal + hydration | `cairn-store-sqlite` | Implements `search_graph_neighbors`. Capability probe gates on entity_graph schema. Tests: hidden-alias, orphan-edge, supersession, bitemporal, rank-rescue. |
| 5 | Orchestrator: 4-leg parallel + auth-only seed retrieval + degraded_legs | `cairn-store-sqlite::store::hybrid`, `cairn-core::verbs::search`, `cairn-cli`, `cairn-sdk`, `cairn-mcp`, `cairn-idl` | Wires all the previous phases together. Adds `--confidence-min` CLI flag. Threads `degraded_legs` through SearchOutcome → CLI/MCP/SDK. IDL update + codegen. |
| 6 | Bench + load gates | `cairn-bench` | Baseline p99, broad-query p99, saturation p99, deadline-circuit, Tier A acquire-timeout gates. |

**Order matters.** Phase N depends on N-1 except 3↔4 (3 can land before 4; 4 cannot land before 3 because graph SQL needs the cancellable pool).

---

## Phase 1: Pure types + RRF/cosine extensions

**Goal:** Land all pure-function building blocks in `cairn-core::search` so phases 2-5 can compose them. No `MemoryStore` changes, no I/O, no callers — all new code is `#[allow(dead_code)]` until phase 5 wires it.

**Files:**
- Modify: `crates/cairn-core/src/search/rrf.rs` — add `RankedCandidate`, `Leg`, `rrf_fusion_weighted`.
- Modify: `crates/cairn-core/src/search/cosine.rs` — add graph-only path to `cosine_rerank`.
- Create: `crates/cairn-core/src/search/graph.rs` — define `GraphCandidate`.
- Create: `crates/cairn-core/src/search/degraded.rs` — `DegradedLeg`, `DegradationReason`, `GraphSource` enums.
- Modify: `crates/cairn-core/src/search/orchestrator.rs` — extend `HybridSearchInputs`/`Params` with new fields; extend `hybrid_search`.
- Modify: `crates/cairn-core/src/search/mod.rs` — re-export new public types.

### Task 1.1: Add `GraphCandidate` type

**Files:**
- Create: `crates/cairn-core/src/search/graph.rs`
- Test: inline `#[cfg(test)] mod tests` in same file.

- [ ] **Step 1: Write the failing test**

```rust
// crates/cairn-core/src/search/graph.rs
//! One graph-leg candidate.

use crate::domain::RecordId;

/// One graph-leg candidate: a record reached via a neighbor entity edge.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphCandidate {
    pub record_id: RecordId,
    /// Confidence score of the *connecting edge* in `[0.0, 1.0]`.
    pub edge_confidence_score: f32,
    /// 1-based rank in the graph leg's SQL output order. Carried
    /// explicitly so RRF fusion does not infer rank from list position
    /// after hydration shuffles ids. See spec §5.2.1.
    pub graph_rank: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(suffix: &str) -> RecordId {
        RecordId::parse(format!("01HQZX9F5N00000000000000{suffix}")).expect("valid id")
    }

    #[test]
    fn construct_and_clone() {
        let c = GraphCandidate {
            record_id: rid("0A"),
            edge_confidence_score: 0.85,
            graph_rank: 3,
        };
        let c2 = c.clone();
        assert_eq!(c, c2);
    }
}
```

- [ ] **Step 2: Wire into `mod.rs` and run**

Edit `crates/cairn-core/src/search/mod.rs`: add `pub mod graph;` and `pub use graph::GraphCandidate;`.

Run: `cargo nextest run -p cairn-core search::graph`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/search/graph.rs crates/cairn-core/src/search/mod.rs
git commit -m "feat(core/search): GraphCandidate type for graph-leg results (issue #191)"
```

### Task 1.2: Add `DegradedLeg` typed enum

**Files:**
- Create: `crates/cairn-core/src/search/degraded.rs`
- Modify: `crates/cairn-core/src/search/mod.rs`

- [ ] **Step 1: Write the type + tests**

```rust
// crates/cairn-core/src/search/degraded.rs
//! Typed degradation signal for hybrid search responses.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DegradationReason {
    CapabilityUnavailable,
    DeadlineExceeded,
    SqlError,
    WorkerPanic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphSource {
    All,
    AuthKeywordSeed,
    AuthSemanticSeed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DegradedLeg {
    Semantic { reason: DegradationReason },
    Graph    { reason: DegradationReason, source: GraphSource },
}

impl DegradedLeg {
    pub fn graph_capability_unavailable() -> Self {
        Self::Graph { reason: DegradationReason::CapabilityUnavailable, source: GraphSource::All }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graph_constructor_helper() {
        let d = DegradedLeg::graph_capability_unavailable();
        assert!(matches!(d, DegradedLeg::Graph { reason: DegradationReason::CapabilityUnavailable, source: GraphSource::All }));
    }
}
```

- [ ] **Step 2: Wire + run**

Edit `mod.rs`: `pub mod degraded;` + re-export `DegradedLeg`, `DegradationReason`, `GraphSource`.

Run: `cargo nextest run -p cairn-core search::degraded`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/search/degraded.rs crates/cairn-core/src/search/mod.rs
git commit -m "feat(core/search): DegradedLeg typed enum for partial-result signaling"
```

### Task 1.3: Add `rrf_fusion_weighted` with `Leg` shape variants

**Files:**
- Modify: `crates/cairn-core/src/search/rrf.rs`

- [ ] **Step 1: Write tests for the new function**

Add to `rrf.rs`:

```rust
/// One element of an explicit-rank input list to RRF.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedCandidate {
    pub record_id: RecordId,
    /// 1-based rank.
    pub rank: usize,
    /// Confidence weight in `[0.0, 1.0]`. Used to compute
    /// `effective_rank = rank / max(weight, floor)`.
    pub weight: f32,
}

/// One leg of [`rrf_fusion_weighted`].
#[derive(Debug, Clone)]
pub enum Leg {
    /// Rank inferred from slice index. Equivalent to today's `rrf_fusion`.
    ListPosition(Vec<ScoredCandidate>),
    /// Rank carried per-candidate; confidence penalty applied with `floor`.
    Explicit(Vec<RankedCandidate>, f32),
}

#[must_use]
pub fn rrf_fusion_weighted(legs: &[Leg], k: usize) -> Vec<RrfCandidate> {
    use std::collections::HashMap;
    let mut acc: HashMap<RecordId, f64> = HashMap::new();
    #[allow(clippy::cast_precision_loss)]
    let kf = k as f64;
    for leg in legs {
        match leg {
            Leg::ListPosition(list) => {
                for (i, c) in list.iter().enumerate() {
                    #[allow(clippy::cast_precision_loss)]
                    let r = (i + 1) as f64;
                    *acc.entry(c.record_id.clone()).or_insert(0.0) += 1.0 / (kf + r);
                }
            }
            Leg::Explicit(list, floor) => {
                let f = (*floor).max(1e-6) as f64;
                for c in list {
                    #[allow(clippy::cast_precision_loss)]
                    let raw_rank = c.rank as f64;
                    let weight = (c.weight as f64).max(f);
                    let effective = raw_rank / weight;
                    *acc.entry(c.record_id.clone()).or_insert(0.0) += 1.0 / (kf + effective);
                }
            }
        }
    }
    let mut out: Vec<RrfCandidate> = acc.into_iter()
        .map(|(record_id, rrf_score)| RrfCandidate { record_id, rrf_score })
        .collect();
    out.sort_by(|a, b| b.rrf_score.partial_cmp(&a.rrf_score).unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.record_id.as_str().cmp(b.record_id.as_str())));
    out
}
```

Add tests:

```rust
#[test]
fn weighted_extracted_outranks_inferred_at_same_rank() {
    let extracted = vec![RankedCandidate { record_id: rid("0A"), rank: 1, weight: 1.0 }];
    let inferred  = vec![RankedCandidate { record_id: rid("0B"), rank: 1, weight: 0.6 }];
    let out = rrf_fusion_weighted(&[
        Leg::Explicit(extracted, 1e-3),
        Leg::Explicit(inferred,  1e-3),
    ], 60);
    assert_eq!(out[0].record_id, rid("0A"));
}

#[test]
fn list_position_matches_legacy_fusion() {
    let list = vec![
        ScoredCandidate { record_id: rid("0A"), score: 1.0 },
        ScoredCandidate { record_id: rid("0B"), score: 0.5 },
    ];
    let legacy  = rrf_fusion(&[list.clone()], 60);
    let weighted = rrf_fusion_weighted(&[Leg::ListPosition(list)], 60);
    assert_eq!(legacy, weighted);
}

#[test]
fn confidence_floor_prevents_div_by_zero() {
    let zero_conf = vec![RankedCandidate { record_id: rid("0A"), rank: 1, weight: 0.0 }];
    let out = rrf_fusion_weighted(&[Leg::Explicit(zero_conf, 1e-3)], 60);
    assert!(out[0].rrf_score.is_finite());
}
```

- [ ] **Step 2: Run**

Run: `cargo nextest run -p cairn-core search::rrf`
Expected: PASS (existing + new tests).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/search/rrf.rs
git commit -m "feat(core/search): rrf_fusion_weighted with confidence-penalty leg"
```

### Task 1.4: Extend `cosine_rerank` for graph-only candidates

**Files:**
- Modify: `crates/cairn-core/src/search/cosine.rs`

- [ ] **Step 1: Add `CandidateOrigin` and tagged candidate type**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateOrigin {
    Lexical,
    GraphOnly,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OriginTaggedCandidate {
    pub inner: super::rrf::RrfCandidate,
    pub origin: CandidateOrigin,
}
```

- [ ] **Step 2: Add `cosine_rerank_tagged` (new function — keep existing `cosine_rerank` for backwards compat)**

```rust
#[must_use]
pub fn cosine_rerank_tagged(
    candidates: &[OriginTaggedCandidate],
    doc_vectors: &HashMap<RecordId, Vec<f32>>,
    query_vector: &[f32],
    blend: f32,
) -> Vec<RerankedCandidate> {
    let max_rrf = candidates.iter().map(|c| c.inner.rrf_score).fold(0.0_f64, f64::max);
    let mut out: Vec<RerankedCandidate> = candidates.iter().map(|tc| {
        let rrf_norm = if max_rrf < f64::EPSILON { 0.0 } else { tc.inner.rrf_score / max_rrf };
        let final_score = match tc.origin {
            CandidateOrigin::Lexical => {
                let cos_norm = doc_vectors
                    .get(&tc.inner.record_id)
                    .map(|v| (cosine_similarity(query_vector, v) + 1.0) / 2.0)
                    .unwrap_or(0.5) as f64;
                f64::from(blend) * rrf_norm + (1.0 - f64::from(blend)) * cos_norm
            }
            CandidateOrigin::GraphOnly => f64::from(blend) * rrf_norm,
        };
        RerankedCandidate {
            record_id: tc.inner.record_id.clone(),
            rrf_score: tc.inner.rrf_score,
            cosine: matches!(tc.origin, CandidateOrigin::Lexical)
                .then(|| doc_vectors.get(&tc.inner.record_id).map(|v| cosine_similarity(query_vector, v)).unwrap_or(0.0)),
            final_score,
        }
    }).collect();
    out.sort_by(|a, b| b.final_score.partial_cmp(&a.final_score).unwrap_or(std::cmp::Ordering::Equal));
    out
}
```

- [ ] **Step 3: Add tests**

```rust
#[test]
fn graph_only_ties_lexical_at_zero_cosine_equal_rrf() {
    let mut docs = HashMap::new();
    docs.insert(rid("0A"), vec![0.0_f32, 1.0]);          // lexical doc, cosine_raw = -1 vs query
    let cands = vec![
        OriginTaggedCandidate {
            inner: RrfCandidate { record_id: rid("0A"), rrf_score: 0.5 },
            origin: CandidateOrigin::Lexical,
        },
        OriginTaggedCandidate {
            inner: RrfCandidate { record_id: rid("0B"), rrf_score: 0.5 },
            origin: CandidateOrigin::GraphOnly,
        },
    ];
    let q = vec![0.0_f32, -1.0]; // produces cosine_raw=-1, cosine_norm=0 for 0A
    let out = cosine_rerank_tagged(&cands, &docs, &q, 0.7);
    assert!((out[0].final_score - out[1].final_score).abs() < 1e-9, "lexical-cos-0 must tie graph-only");
}

#[test]
fn graph_only_loses_to_strong_lexical_at_equal_rrf() {
    let mut docs = HashMap::new();
    docs.insert(rid("0A"), vec![1.0_f32, 0.0]);
    let cands = vec![
        OriginTaggedCandidate {
            inner: RrfCandidate { record_id: rid("0A"), rrf_score: 0.5 },
            origin: CandidateOrigin::Lexical,
        },
        OriginTaggedCandidate {
            inner: RrfCandidate { record_id: rid("0B"), rrf_score: 0.5 },
            origin: CandidateOrigin::GraphOnly,
        },
    ];
    let q = vec![1.0_f32, 0.0];
    let out = cosine_rerank_tagged(&cands, &docs, &q, 0.7);
    assert_eq!(out[0].record_id, rid("0A"));
}
```

- [ ] **Step 4: Run + commit**

Run: `cargo nextest run -p cairn-core search::cosine`
Expected: PASS.

```bash
git add crates/cairn-core/src/search/cosine.rs
git commit -m "feat(core/search): graph-only candidate path in cosine rerank"
```

### Task 1.5: Extend `HybridSearchInputs` / `HybridSearchParams` and update `hybrid_search`

**Files:**
- Modify: `crates/cairn-core/src/search/orchestrator.rs`

Add to `HybridSearchInputs`:
```rust
pub graph: Vec<GraphCandidate>,
```
Add to `HybridSearchParams`:
```rust
pub confidence_floor: f32,   // default 1e-3
```
And `Default for HybridSearchParams` sets `confidence_floor: 1e-3`.

Then refactor `hybrid_search` body to:
1. Build legs: `Leg::ListPosition(keyword)`, `Leg::ListPosition(semantic)`, `Leg::Explicit(graph_as_ranked, params.confidence_floor)`.
2. Call `rrf_fusion_weighted`.
3. Tag each fused candidate as `Lexical` or `GraphOnly` based on whether it appears in the graph leg AND not in the keyword/semantic leg.
4. Call `cosine_rerank_tagged`.

Add tests:
- Three-leg integration: keyword + semantic + graph with mixed confidence neighbors → ordering matches expected.
- Graph-leg empty → output equals legacy 2-leg behavior (parity test).
- Graph-only candidate present → tagged correctly and survives rerank.

Commit:
```bash
git add crates/cairn-core/src/search/orchestrator.rs
git commit -m "feat(core/search): 3-leg hybrid_search with graph candidates and tagged rerank"
```

### Task 1.6: Phase 1 final verification

- [ ] Run `cargo fmt --all --check`
- [ ] Run `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] Run `cargo nextest run --workspace`
- [ ] Run `./scripts/check-core-boundary.sh`
- [ ] Open PR titled `feat(core/search): pure types + 3-leg RRF + graph-only rerank (issue #191 phase 1)`. Body cites spec §5, §6, §6.1.

---

## Phase 2: Contract bump — auth_scope + capability + trait method

**Goal:** Bump `CONTRACT_VERSION` to `0.5.0`, add `auth_scope: ScopeTuple` to all search args, add `MemoryStoreCapabilities::graph_search`, add the `search_graph_neighbors` method. Existing search behavior unchanged (the SQLite store still authorizes via `filter` for keyword/semantic, just like today; only graph-aware code paths added in phase 4 will read `auth_scope`). The default trait impl of `search_graph_neighbors` returns `CapabilityUnavailable`.

**Files:**
- Modify: `crates/cairn-core/src/contract/memory_store.rs` — `CONTRACT_VERSION`, all `*SearchArgs`, `MemoryStoreCapabilities`, new `GraphNeighborsArgs`, trait method.
- Modify: `crates/cairn-store-sqlite/src/lib.rs` — `ACCEPTED_RANGE`.
- Modify: `crates/cairn-core/src/search/orchestrator.rs` — extend args definitions if any in core mod.
- Modify: `crates/cairn-store-sqlite/src/store/search.rs`, `crates/cairn-store-sqlite/src/store/hybrid.rs`, `crates/cairn-store-sqlite/src/lib.rs` — accept new field; pass through unchanged.
- Modify: every test that constructs `KeywordSearchArgs` / `SemanticSearchArgs` / `HybridSearchArgs` — populate `auth_scope`. Use `ScopeTuple::EMPTY` in legacy tests where no scope was previously used.
- Modify: `crates/cairn-core/src/contract/memory_store.rs` test stubs.
- Modify: `crates/cairn-store-sqlite/tests/capabilities_unchanged.rs` — regenerate snapshot.
- Add `#[non_exhaustive]` to: `KeywordSearchArgs`, `SemanticSearchArgs`, `HybridSearchArgs`, `HybridSearchInputs`, `HybridSearchParams`, `HybridSearchPage`, `KeywordSearchPage`, `SemanticSearchPage`, `ScoreExplain`, `MemoryStoreCapabilities`, `SearchOutcome`.
- Add `bon::Builder` derive to all `*SearchArgs` structs and `GraphNeighborsArgs`. Imports: add `bon` to `[workspace.dependencies]` if not already present (check first).

### Tasks (high-level — see spec §12.1)

1. **Pre-work: add `#[non_exhaustive]` to existing public structs.** This itself is a breaking change for external struct literals; bundle with the contract bump.
2. **Add `auth_scope: ScopeTuple`** as a public required field on the args structs. Update `Default` impls to use `ScopeTuple::EMPTY` only inside the test-fixtures crate's helpers; production callers must populate.
3. **Add `MemoryStoreCapabilities::graph_search: bool`** with `Default = false`.
4. **Add `GraphNeighborsArgs`** struct (see spec §4.3).
5. **Add `search_graph_neighbors`** method to `MemoryStore` trait with default impl returning `Err(StoreError::CapabilityUnavailable { what: "graph_search" })`.
6. **Bump `CONTRACT_VERSION`** to `ContractVersion::new(0, 5, 0)`.
7. **Bump `ACCEPTED_RANGE`** in `cairn-store-sqlite`, `cairn-mcp`, `cairn-sdk` to `[0.5.0, 0.6.0)`.
8. **Add `bon::Builder` derive** + write a basic builder usage test for each args struct.
9. **Update every callsite** in the workspace. Mechanical fan-out.
10. **Regenerate `capabilities_unchanged.rs` snapshot.**

Verification: workspace builds; `cargo nextest run --workspace` passes; the existing keyword/semantic search tests are unchanged in behavior.

PR title: `feat(core): contract 0.5.0 — auth_scope + graph_search capability + search_graph_neighbors method (issue #191 phase 2)`

---

## Phase 3: Two-tier connection pool + cancellation lifecycle

**Goal:** Introduce `HybridConnectionPool` (Tier A + Tier B) replacing the current single-connection model for hybrid search. Wire `interrupt()` + `progress_handler` per connection. Migrate `do_search_keyword` and `do_search_semantic` onto Tier A. Add per-checkout reset/discard lifecycle.

**Files:**
- Create: `crates/cairn-store-sqlite/src/pool.rs`
- Modify: `crates/cairn-store-sqlite/src/open.rs` — `open_connection` factory becomes the single source of truth for per-connection setup; pool init goes through it.
- Modify: `crates/cairn-store-sqlite/src/store/mod.rs` — `SqliteMemoryStore` gains `hybrid_pool: Arc<HybridConnectionPool>`.
- Modify: `crates/cairn-store-sqlite/src/store/search.rs` — keyword + semantic now acquire from Tier A.
- Modify: `crates/cairn-store-sqlite/src/store/hybrid.rs` — orchestrator acquires per-leg connections from the pool.
- Add: `crates/cairn-store-sqlite/tests/connection_pool.rs` — integration tests for cross-request reuse, sibling-completion isolation, init-failure → graph_search false.

**Key implementation points (see spec §5.3):**

- Pool size: `filtered_pool_size = num_cpus()` for Tier A, `graph_pool_size = 2 * num_cpus()` for Tier B.
- Tier A acquire: `tokio::time::timeout(filtered_acquire_timeout_ms, semaphore.acquire())`. Default 200ms. On timeout return `StoreError::Timeout` with reason `tier_a_acquire_timeout`.
- Tier B acquire: same shape with `pool_acquire_timeout_ms` (= leg deadline).
- Per-checkout lifecycle:
  - Borrow → install fresh `CancellationToken`, re-arm `progress_handler` against it, run `SELECT 1` smoke probe; if probe fails, discard + rebuild via factory.
  - Run → leg's SQL.
  - Release → reset token, deregister callback, run `sqlite3_db_release_memory`. If reset errors, discard + rebuild.
- Watchdog: when deadline elapses, call `Connection::interrupt()` AND fire the cancellation token.
- Capability probe extension: every pool connection must pass §8 probes individually before `caps.graph_search = true`.

Tests:
- Cross-request reuse after timeout: assert request N+1 succeeds on the same pool slot that request N timed out on.
- Sibling-completion isolation: two parallel legs, one times out; assert the other completes normally on its own connection.
- Pool-init failure: inject a `carray` load failure on one pool connection; assert `caps.graph_search = false` at startup.
- Tier A acquire timeout: saturate Tier A; new request fails with timeout in `filtered_acquire_timeout_ms` ± 10ms.

PR title: `feat(store): two-tier connection pool + statement-scoped cancellation (issue #191 phase 3)`

---

## Phase 4: Graph SQL — 1-hop + supersession + bitemporal + hydration

**Goal:** Implement `search_graph_neighbors` end-to-end. The pure orchestrator + connection pool from previous phases compose with the new SQL.

**Files:**
- Create: `crates/cairn-store-sqlite/src/store/graph_search.rs` — implements `do_search_graph_neighbors` with the §5.1 SQL (CTE form).
- Create: `crates/cairn-store-sqlite/src/store/graph_hydrate.rs` — implements `hydrate_graph_only` (§5.2.1 SQL + Rust-side rank preservation).
- Modify: `crates/cairn-store-sqlite/src/store/trait_impl.rs` — wire trait method to `do_search_graph_neighbors`.
- Modify: `crates/cairn-store-sqlite/src/open.rs` — extend §8 capability probe to cover `valid_at`, `created_at`, `expired_at`, `invalid_at` on `entity_edges`; `episode_id` on `entity_episodes`; tombstoned/active/scope on `records`.
- Add: `crates/cairn-store-sqlite/tests/graph_search.rs` — full §10.2 test suite.

**Key SQL (see spec §5.1):**

The CTE has the shape (literal bindings):
```sql
WITH RECURSIVE
  visible_query_records(id) AS (SELECT value FROM carray(?1)),    -- ≤ 400 ids
  ranked_query_records(id)  AS (SELECT value FROM carray(?5)),    -- ≤ 100 ids
  seeds(id) AS (
    SELECT DISTINCT ep.entity_node_id
      FROM entity_episodes ep
     WHERE ep.episode_id IN (SELECT id FROM visible_query_records)
  ),
  neighbors(neighbor_id, conf) AS (
    SELECT neighbor_id, MAX(confidence_score)
      FROM (
        SELECT
          CASE WHEN e.source_id IN (SELECT id FROM seeds) THEN e.target_id ELSE e.source_id END AS neighbor_id,
          e.confidence_score
        FROM entity_edges e
        JOIN records pr ON pr.record_id = e.source_record_id
        WHERE (e.source_id IN (SELECT id FROM seeds) OR e.target_id IN (SELECT id FROM seeds))
          AND e.invalid_at IS NULL AND e.expired_at IS NULL
          AND e.valid_at <= ?6 AND e.created_at <= ?7
          AND e.source_record_id IS NOT NULL
          AND e.confidence_score >= ?2
          AND pr.tombstoned = 0 AND pr.active = 1
          AND <auth_scope predicate ON pr>
          AND <visibility predicate ON pr>
      )
      WHERE neighbor_id NOT IN (SELECT id FROM seeds)
      GROUP BY neighbor_id
  )
SELECT r.record_id, MAX(n.conf) AS conf
  FROM neighbors n
  JOIN entity_episodes ep ON ep.entity_node_id = n.neighbor_id
  JOIN records r ON r.record_id = ep.episode_id
 WHERE r.tombstoned = 0 AND r.active = 1
   AND <supersession predicate ON r>
   AND <auth_scope predicate ON r>
   AND <visibility predicate ON r>
   AND <filter predicate ON r>
   AND r.record_id NOT IN (SELECT id FROM ranked_query_records)
 GROUP BY r.record_id
 ORDER BY conf DESC, r.updated_at DESC
 LIMIT ?3;
```

`<auth_scope predicate>`, `<visibility predicate>`, `<filter predicate>`, `<supersession predicate>` are built by the same shared helper that `do_search_keyword` uses (extract the helper to a shared module if not already shared).

Capture each row with its 1-based ordinal as `graph_rank`; pass through to hydration.

**Hydration:** §5.2.1 SQL — fetch by id via `IN carray(?1)`, re-apply auth + visibility + filter + supersession. Re-sort the hydrated rows by `graph_rank` in Rust before returning.

**Tests (§10.2 + §10.3 partial):**
- Discovery property (neighbor-only record surfaces).
- Hidden alias does not seed graph.
- Cross-scope provenance excluded.
- Orphan edge (NULL source_record_id) excluded.
- Expired edge / invalid edge excluded.
- Future-dated edge (valid_at > now or created_at > now) excluded.
- Stale-seed exclusion (superseded source).
- Stale-neighbor exclusion (superseded neighbor).
- Self-exclusion (seed cannot be its own "neighbor").
- Rank-rescue (record at keyword rank-75 returns via graph).
- Hydration auth re-check (cache replay defense-in-depth).
- N+1 verifier: prepared-statement count ≤ 3 across the whole graph search.
- Capability skew: missing `expired_at` column → `graph_search=false`.
- Capability skew: missing `valid_at` column → `graph_search=false`.

PR title: `feat(store): graph 1-hop SQL + hydration + capability probe (issue #191 phase 4)`

---

## Phase 5: Orchestrator — 4-leg parallel + degraded_legs propagation

**Goal:** Wire all the phases together. The hybrid orchestrator (`store/hybrid.rs::do_search_hybrid`) now runs four parallel legs via `tokio::task::JoinSet`, applies the per-leg timeout table, builds `degraded_legs`, and threads it through `SearchOutcome` → CLI / MCP / SDK.

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/hybrid.rs` — JoinSet-based 4-leg fan-out; auth-only seed query (id-only, no filter, supersession kept); deadline + degradation handling; pool acquisition; graph-leg dispatch; merging into `HybridSearchPage` with `degraded_legs`.
- Create: `crates/cairn-store-sqlite/src/store/seed_query.rs` — id-only auth+supersession-only retrieval for keyword and semantic.
- Modify: `crates/cairn-core/src/contract/memory_store.rs` — `HybridSearchPage` gains `degraded_legs: Vec<DegradedLeg>`.
- Modify: `crates/cairn-core/src/verbs/search.rs` — `SearchOutcome` gains `degraded_legs: Vec<DegradedLeg>`; dispatcher copies it through.
- Modify: `crates/cairn-cli/src/verbs/search.rs` — `--confidence-min <f32>` flag; `--json` includes `degraded_legs`; TTY emits `warning:` to stderr when non-empty.
- Modify: `crates/cairn-mcp/src/...` — schemars derive picks up `degraded_legs` automatically; assert via snapshot.
- Modify: `crates/cairn-sdk/src/transport.rs` — extend `SearchData` mapping to include `degraded_legs`.
- Modify: `crates/cairn-idl/...` — extend search-response IDL with `degraded_legs: Vec<DegradedLeg>` (`#[serde(default)]`); `auth_scope: ScopeTuple` required (no default). Re-run `cairn-codegen`; commit generated diff.
- Modify: `crates/cairn-core/src/config/mod.rs` — `SearchConfig` adds `confidence_min`, `confidence_floor`, `graph_seed_overfetch`, `graph_leg_deadline_ms`, `filtered_acquire_timeout_ms`, `pool_acquire_timeout_ms`, `filtered_pool_size`, `graph_pool_size`.

**Tests:**
- End-to-end hybrid w/ all 3 legs against a real seeded vault.
- `--json` snapshot stable (insta).
- Graph-only candidate hydrated correctly through verb → CLI.
- `degraded_legs` survives dispatcher → CLI `--json`.
- `degraded_legs` survives SDK transport round-trip.
- Per-leg timeout table: each row tested with synthetic delay injection (use a wrapper `MemoryStore` impl that delays specific legs).
- 0.4.x-shaped request (no `auth_scope`) → `SearchError::MissingField`.

PR title: `feat(verbs/cli/sdk/mcp): wire 3-leg hybrid search end-to-end with degraded_legs (issue #191 phase 5)`

---

## Phase 6: Bench + load gates

**Goal:** Add bench cases that gate the latency/load claims in §10.5. CI-runnable.

**Files:**
- Modify: `crates/cairn-bench/...` — Criterion benches or custom harness.

**Gates:**
- p99 < 100ms on 10k-record vault, hybrid search.
- p99 < 100ms on 50k-record vault, broad query (>200 keyword hits).
- p99 < 150ms under 32-way concurrent load on 50k-record vault; assert adaptive-overfetch reduction engages at least once.
- Deadline-circuit: synthetic delay → request returns within deadline + 10ms with correct `degraded_legs`.
- Tier A acquire-timeout: saturate Tier A → fast failure within `filtered_acquire_timeout_ms` ± 10ms.

PR title: `bench: hybrid search load + deadline gates (issue #191 phase 6)`

---

## Self-Review

**Spec coverage:** Every §-section in the spec is mapped to a phase. §3 algorithm = phases 1+4. §4 contracts = phase 2. §5.1 SQL = phase 4. §5.2.1 hydration = phase 4. §5.3 orchestrator = phase 5. §5.4 verb envelope = phase 5. §6 fusion = phase 1. §6.1 cosine graph-only = phase 1. §7 verb + CLI = phase 5. §8 capability = phases 2+3+4. §10 tests distributed across phases (each phase ships its own tests). §10.5 bench = phase 6. §11 AC mapping covered. §12.1 contract version = phase 2.

**Placeholder scan:** No "TBD" / "TODO" / "implement later" in the plan. Predicate placeholders (`<auth_scope predicate>`, etc.) are intentional — they reference an existing helper that the implementer will reuse from `do_search_keyword`. Phase 4 task should locate and extract that helper as an early sub-task if it's still inline today.

**Type consistency:** `GraphCandidate { record_id, edge_confidence_score, graph_rank }` is the same in phase 1 and phase 4. `RankedCandidate { record_id, rank, weight }` ditto. `DegradedLeg { Semantic { reason }, Graph { reason, source } }` ditto. `auth_scope: ScopeTuple` consistent.

**Honest limitation:** Phases 4 and 5 are large — phase 4 has ~15 integration tests, phase 5 has the most cross-crate fan-out. Subagent execution is strongly recommended for those phases (see executing-plans skill); inline batch execution will be slow.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-05-issue-191-rrf-hybrid-graph.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — Dispatch a fresh subagent per phase (or per major task within phase 4/5). Two-stage review between phases.

2. **Inline Execution** — Execute phases sequentially in this session. Long; will likely need batch checkpoints.

**Suggested next step:** start with phase 1 (fully self-contained, pure functions, ~6 tasks, ~1-2 hours of subagent time) so the type vocabulary is locked in before the contract bump in phase 2.

Which approach?
