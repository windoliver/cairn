# Issue #49 — Hybrid Search Orchestration & `cairn admin reindex --from-db` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close issue #49 by lifting the per-mode CLI dispatch into a shared `cairn-core::verbs::search::run` dispatcher (consumed by CLI/SDK/MCP), adding token-budget trimming + per-candidate score explanations, and shipping `cairn admin reindex --from-db` to rebuild FTS5 + vectors from the authoritative `records` table.

**Architecture:** A single async pure-ish dispatcher in `cairn-core::verbs::search` accepts `&dyn MemoryStore` + `SearchArgs` + config + capability set, performs capability gating, dispatches to the right `store.search_*` method, applies token-budget trim, and returns a `VerbResponse<SearchData>` envelope with an optional `score_explain` block populated when `--explain` (gated by `cairn.mcp.v1.policy_trace`) is set. Surfaces (CLI/SDK/MCP) become thin wrappers that own store construction + I/O. A new `store::reindex::rebuild_from_db` truncates derived indexes and re-ingests from `records` in two transactions, then drives the existing `drain_once` loop.

**Tech Stack:** Rust 1.95 (edition 2024), tokio, `rusqlite` + `sqlite-vec` + FTS5, `clap` 4.5, `tracing`, `thiserror`, `insta` (snapshot tests), `assert_cmd` (CLI tests), `proptest`, `rstest`, `cairn-embeddings-local` (`MiniLM-L6-v2` test model).

**Reference:** Design spec at `docs/superpowers/specs/2026-05-02-issue-49-hybrid-search-orchestration-design.md`.

---

## File Structure

**New files:**
- `crates/cairn-core/src/search/explain.rs` — `ScoreExplain` type + projection helpers.
- `crates/cairn-core/src/search/trim.rs` — `token_budget_trim` pure function.
- `crates/cairn-core/src/verbs/search.rs` — dispatcher `run()` + `SearchError`.
- `crates/cairn-store-sqlite/src/store/reindex_from_db.rs` — `rebuild_from_db` + `RebuildStats`.
- `crates/cairn-test-fixtures/src/hybrid_vault.rs` — `build_hybrid_test_vault` helper.
- `crates/cairn-store-sqlite/tests/reindex_from_db.rs` — destructive-fixture integration test.
- `crates/cairn-cli/tests/search_modes_golden.rs` — golden-query CLI snapshot tests.
- `crates/cairn-cli/tests/search_explain.rs` — `--explain` CLI snapshot test.
- `crates/cairn-cli/tests/admin_reindex_from_db.rs` — destructive-rebuild CLI snapshot.
- `crates/cairn-sdk/tests/search_dispatch.rs` — SDK dispatch tests.
- `crates/cairn-mcp/tests/search_tool.rs` — MCP search tool tests.
- `fixtures/golden/search/` — directory holding `<query>.<mode>.json` snapshots (created by `insta`).

**Modified files:**
- `crates/cairn-core/src/contract/memory_store.rs` — add `with_explain: bool` to all three `*SearchArgs`; add `explain: Option<Vec<ScoreExplain>>` to all three `*SearchPage` structs; bump `CONTRACT_VERSION` to `0.4.0`.
- `crates/cairn-core/src/search/mod.rs` — `pub mod explain;` + `pub mod trim;` + re-exports.
- `crates/cairn-core/src/verbs/mod.rs` — `pub mod search;`.
- `crates/cairn-core/src/config/mod.rs` — add `max_snippet_chars_per_page: usize` (default `8000`) on `SearchConfig`.
- `crates/cairn-idl/schema/verbs/search.json` — add `score_explain` field on `Data` (optional array).
- `crates/cairn-store-sqlite/src/store/mod.rs` — `pub(crate) mod reindex_from_db;`.
- `crates/cairn-store-sqlite/src/lib.rs` — re-export `rebuild_from_db`, `RebuildStats`.
- `crates/cairn-store-sqlite/src/store/hybrid.rs` — populate `explain` when `args.with_explain` is true.
- `crates/cairn-store-sqlite/src/store/trait_impl.rs` (or wherever `search_keyword`/`search_semantic` live) — populate `explain` (rank-only) when `args.with_explain` is true.
- `crates/cairn-cli/src/verbs/search.rs` — collapse three runners into one calling `cairn_core::verbs::search::run`.
- `crates/cairn-cli/src/verbs/admin_reindex.rs` — add `--from-db` flag handling.
- `crates/cairn-cli/src/command.rs` — register `--from-db` flag.
- `crates/cairn-sdk/src/transport.rs` — `SdkClient::with_store(...)` constructor + `search()` execution path.
- `crates/cairn-sdk/src/lib.rs` — re-export new constructor signature.
- `crates/cairn-mcp/src/handler.rs` — store injection + dispatch.
- `crates/cairn-test-fixtures/src/lib.rs` — `pub mod hybrid_vault;`.

---

## Task 1: Add `ScoreExplain` type and config knob

**Files:**
- Create: `crates/cairn-core/src/search/explain.rs`
- Modify: `crates/cairn-core/src/search/mod.rs`
- Modify: `crates/cairn-core/src/config/mod.rs`

- [ ] **Step 1: Create `ScoreExplain` type with tests**

Create `crates/cairn-core/src/search/explain.rs`:

```rust
//! Per-candidate score explanation surfaced when `--explain` is set.
//!
//! Populated by the store layer when `*SearchArgs.with_explain` is true.
//! The dispatcher passes the explain block through to the response envelope
//! gated by the `cairn.mcp.v1.policy_trace` capability.

use crate::domain::RecordId;

/// Component scores for a single search candidate.
///
/// Field semantics:
/// - `bm25_rank`: 1-based position in the FTS5 BM25 leg, or `None` if the
///   candidate did not appear in that leg.
/// - `semantic_rank`: 1-based position in the ANN leg, or `None`.
/// - `rrf_score`: RRF fusion score; `0.0` for keyword/semantic-only modes
///   that did not run RRF.
/// - `cosine`: cosine similarity to the query vector when the candidate was
///   in the rerank top-K; `None` otherwise.
/// - `final_score`: blended `final_score` from the orchestrator (hybrid),
///   normalized RRF (hybrid skip-rerank), or the leg's native score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreExplain {
    /// Identifier of the explained candidate.
    pub record_id: RecordId,
    /// 1-based BM25 leg rank, or `None` if absent from that leg.
    pub bm25_rank: Option<usize>,
    /// 1-based semantic leg rank, or `None` if absent from that leg.
    pub semantic_rank: Option<usize>,
    /// RRF fusion score. `0.0` for non-hybrid modes.
    pub rrf_score: f64,
    /// Cosine similarity to the query vector when re-ranked, else `None`.
    pub cosine: Option<f64>,
    /// Blended final score the candidate was sorted by.
    pub final_score: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(s: &str) -> RecordId {
        RecordId::parse(format!("01HQZX9F5N00000000000000{s}")).expect("valid record id")
    }

    #[test]
    fn explain_holds_all_components() {
        let e = ScoreExplain {
            record_id: rid("0A"),
            bm25_rank: Some(1),
            semantic_rank: Some(3),
            rrf_score: 0.05,
            cosine: Some(0.87),
            final_score: 0.91,
        };
        assert_eq!(e.bm25_rank, Some(1));
        assert_eq!(e.semantic_rank, Some(3));
        assert!((e.rrf_score - 0.05).abs() < 1e-9);
        assert_eq!(e.cosine, Some(0.87));
    }
}
```

- [ ] **Step 2: Wire module + re-export in `search/mod.rs`**

Modify `crates/cairn-core/src/search/mod.rs`:

```rust
//! Pure retrieval-ranking primitives: RRF fusion and cosine re-rank.
//!
//! These functions have no I/O; they take pre-fetched candidate lists and
//! return scored output. The store adapters orchestrate the data fetching.

mod cosine;
mod explain;
mod orchestrator;
mod rrf;
mod trim;

pub use cosine::{RerankedCandidate, cosine_rerank, cosine_similarity};
pub use explain::ScoreExplain;
pub use orchestrator::{HybridSearchInputs, HybridSearchParams, hybrid_search};
pub use rrf::{RrfCandidate, ScoredCandidate, rrf_fusion};
pub use trim::token_budget_trim;
```

(Note: `trim` module created in Task 2; the `pub mod trim;` line will compile only after Task 2. Defer adding the `mod trim;` and `pub use trim::*;` lines until Task 2 — for this commit, only add `mod explain;` and `pub use explain::ScoreExplain;`.)

Updated `crates/cairn-core/src/search/mod.rs` for *this* commit:

```rust
//! Pure retrieval-ranking primitives: RRF fusion and cosine re-rank.
//!
//! These functions have no I/O; they take pre-fetched candidate lists and
//! return scored output. The store adapters orchestrate the data fetching.

mod cosine;
mod explain;
mod orchestrator;
mod rrf;

pub use cosine::{RerankedCandidate, cosine_rerank, cosine_similarity};
pub use explain::ScoreExplain;
pub use orchestrator::{HybridSearchInputs, HybridSearchParams, hybrid_search};
pub use rrf::{RrfCandidate, ScoredCandidate, rrf_fusion};
```

- [ ] **Step 3: Add `max_snippet_chars_per_page` to `SearchConfig`**

In `crates/cairn-core/src/config/mod.rs`, find the `SearchConfig` struct around line 334 and add the field after `rerank_topk`:

```rust
    /// Maximum total snippet characters per search page. Trimming happens
    /// after candidate ranking + dedup. Char-count proxy for token budget;
    /// token-accurate trimming is P1 (see issue #49). Default `8000`.
    pub max_snippet_chars_per_page: usize,
```

In the `Default` impl for `SearchConfig` add:

```rust
            max_snippet_chars_per_page: 8000,
```

Update the snapshot test by running `cargo insta review` after the next test run, or pre-update the snapshot file at `crates/cairn-core/src/config/snapshots/cairn_core__config__tests__default_config_snapshot.snap` to include the new field. Search the snapshot for `rerank_topk` and add `max_snippet_chars_per_page: 8000` next to it.

- [ ] **Step 4: Run unit tests**

Run: `cargo nextest run -p cairn-core search::explain --locked`
Expected: 1 test passes (`explain_holds_all_components`).

Run: `cargo nextest run -p cairn-core config --locked`
Expected: All config tests pass; snapshot includes new field.

If snapshot fails, run `cargo insta review` and accept.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/search/mod.rs \
        crates/cairn-core/src/search/explain.rs \
        crates/cairn-core/src/config/mod.rs \
        crates/cairn-core/src/config/snapshots/
git commit -m "feat(search): add ScoreExplain type + max_snippet_chars_per_page (issue #49)"
```

---

## Task 2: Add `token_budget_trim` pure function

**Files:**
- Create: `crates/cairn-core/src/search/trim.rs`
- Modify: `crates/cairn-core/src/search/mod.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/cairn-core/src/search/trim.rs`:

```rust
//! Char-count proxy for token-budget trimming of search pages.
//!
//! Sums `snippet.len()` across candidates in order; stops appending once
//! the running total would exceed `max_chars`. Trims the parallel `explain`
//! block in lockstep so record-id alignment is preserved.
//!
//! This is a deliberately deterministic char-count approximation, not a
//! tokenizer-accurate count. The token-accurate variant is P1, gated on
//! hot-memory assembly (brief §11) actually consuming search output.

use crate::contract::memory_store::SearchCandidate;
use crate::search::explain::ScoreExplain;

/// Trim `candidates` so the total `snippet.len()` does not exceed
/// `max_chars`. The first candidate is always kept even if it exceeds the
/// budget alone — a search must return at least one hit when the leg
/// produced one. `explain`, when supplied, is trimmed in lockstep on
/// `record_id` alignment.
///
/// `max_chars == 0` is treated as "no trim".
#[must_use]
pub fn token_budget_trim(
    candidates: Vec<SearchCandidate>,
    explain: Option<Vec<ScoreExplain>>,
    max_chars: usize,
) -> (Vec<SearchCandidate>, Option<Vec<ScoreExplain>>) {
    if max_chars == 0 || candidates.is_empty() {
        return (candidates, explain);
    }

    let mut kept_ids: Vec<&str> = Vec::with_capacity(candidates.len());
    let mut running: usize = 0;
    let mut cut_at: usize = candidates.len();
    for (i, c) in candidates.iter().enumerate() {
        let next = running.saturating_add(c.snippet.len());
        if i > 0 && next > max_chars {
            cut_at = i;
            break;
        }
        running = next;
        kept_ids.push(c.record_id.as_str());
    }
    let trimmed_candidates: Vec<SearchCandidate> =
        candidates.into_iter().take(cut_at).collect();
    let trimmed_explain = explain.map(|exps| {
        let kept: std::collections::HashSet<&str> = kept_ids.iter().copied().collect();
        exps.into_iter()
            .filter(|e| kept.contains(e.record_id.as_str()))
            .collect()
    });
    (trimmed_candidates, trimmed_explain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RecordId;
    use crate::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
    use crate::domain::record::{ScopeTuple, TargetId};

    fn rid(s: &str) -> RecordId {
        RecordId::parse(format!("01HQZX9F5N00000000000000{s}")).expect("valid record id")
    }

    fn cand(id: &str, snippet: &str) -> SearchCandidate {
        SearchCandidate {
            record_id: rid(id),
            target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("target"),
            scope: ScopeTuple::default(),
            kind: MemoryKind::Note,
            class: MemoryClass::Episodic,
            visibility: MemoryVisibility::Private,
            bm25: 0.0,
            recency_seconds: 0,
            confidence: 1.0,
            salience: 1.0,
            staleness_seconds: 0,
            snippet: snippet.to_owned(),
            record_json: "{}".to_owned(),
            semantic_distance: None,
        }
    }

    fn explain(id: &str) -> ScoreExplain {
        ScoreExplain {
            record_id: rid(id),
            bm25_rank: Some(1),
            semantic_rank: Some(1),
            rrf_score: 0.0,
            cosine: None,
            final_score: 0.0,
        }
    }

    #[test]
    fn empty_candidates_returns_empty() {
        let (c, e) = token_budget_trim(vec![], None, 100);
        assert!(c.is_empty());
        assert!(e.is_none());
    }

    #[test]
    fn zero_budget_skips_trim() {
        let cands = vec![cand("0A", "hello"), cand("0B", "world")];
        let (c, _) = token_budget_trim(cands, None, 0);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn all_fits_returns_all() {
        let cands = vec![cand("0A", "abc"), cand("0B", "de")];
        let (c, _) = token_budget_trim(cands, None, 100);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn overflow_midway_truncates() {
        let cands = vec![
            cand("0A", "12345"),
            cand("0B", "67890"),
            cand("0C", "abcde"),
        ];
        let (c, _) = token_budget_trim(cands, None, 8);
        // 5 + 5 = 10 > 8, so keep only "12345" (idx 0)
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].record_id, rid("0A"));
    }

    #[test]
    fn first_oversized_candidate_kept() {
        // Single oversized candidate: must be kept (return at least one hit).
        let cands = vec![cand("0A", &"x".repeat(1000))];
        let (c, _) = token_budget_trim(cands, None, 10);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn explain_trimmed_in_lockstep() {
        let cands = vec![cand("0A", "12345"), cand("0B", "67890")];
        let exps = Some(vec![explain("0A"), explain("0B")]);
        let (c, e) = token_budget_trim(cands, exps, 6);
        assert_eq!(c.len(), 1);
        let e = e.expect("explain present");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].record_id, rid("0A"));
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::domain::RecordId;
    use crate::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
    use crate::domain::record::{ScopeTuple, TargetId};
    use proptest::prelude::*;

    fn cand_proptest(id_suffix: &str, snippet_len: usize) -> SearchCandidate {
        SearchCandidate {
            record_id: RecordId::parse(format!("01HQZX9F5N00000000000000{id_suffix}"))
                .expect("valid"),
            target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("target"),
            scope: ScopeTuple::default(),
            kind: MemoryKind::Note,
            class: MemoryClass::Episodic,
            visibility: MemoryVisibility::Private,
            bm25: 0.0,
            recency_seconds: 0,
            confidence: 1.0,
            salience: 1.0,
            staleness_seconds: 0,
            snippet: "x".repeat(snippet_len),
            record_json: "{}".to_owned(),
            semantic_distance: None,
        }
    }

    proptest! {
        #[test]
        fn trim_is_monotone_in_size(
            sizes in prop::collection::vec(1usize..50, 1..16),
            budget in 0usize..400,
        ) {
            let cands: Vec<_> = sizes.iter().enumerate().map(|(i, n)| {
                let suffix = format!("{i:02X}");
                cand_proptest(&suffix, *n)
            }).collect();
            let n_in = cands.len();
            let (out, _) = token_budget_trim(cands, None, budget);
            prop_assert!(out.len() <= n_in);
            prop_assert!(!out.is_empty() || n_in == 0);
        }
    }
}
```

- [ ] **Step 2: Wire `trim` module into `search/mod.rs`**

Modify `crates/cairn-core/src/search/mod.rs`:

```rust
//! Pure retrieval-ranking primitives: RRF fusion and cosine re-rank.
//!
//! These functions have no I/O; they take pre-fetched candidate lists and
//! return scored output. The store adapters orchestrate the data fetching.

mod cosine;
mod explain;
mod orchestrator;
mod rrf;
mod trim;

pub use cosine::{RerankedCandidate, cosine_rerank, cosine_similarity};
pub use explain::ScoreExplain;
pub use orchestrator::{HybridSearchInputs, HybridSearchParams, hybrid_search};
pub use rrf::{RrfCandidate, ScoredCandidate, rrf_fusion};
pub use trim::token_budget_trim;
```

- [ ] **Step 3: Run tests to verify failure (compile) then pass**

Run: `cargo nextest run -p cairn-core search::trim --locked`
Expected: All trim tests pass (~6 tests + 1 proptest).

If `cand_proptest` or `cand` helpers reference fields/types that don't compile, inspect `crates/cairn-core/src/contract/memory_store.rs` `SearchCandidate` definition (line ~614) and adjust the helper to match the actual field set.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/search/mod.rs \
        crates/cairn-core/src/search/trim.rs
git commit -m "feat(search): add token_budget_trim pure function (issue #49)"
```

---

## Task 3: Extend `*SearchArgs` and `*SearchPage` with `with_explain` + `explain`

**Files:**
- Modify: `crates/cairn-core/src/contract/memory_store.rs`

- [ ] **Step 1: Bump contract version**

In `crates/cairn-core/src/contract/memory_store.rs` change line 9 to:

```rust
/// Bumped 0.3 → 0.4 in #49 when search args/pages gained explain plumbing.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 4, 0);
```

- [ ] **Step 2: Add `with_explain` field to all three args structs**

In `KeywordSearchArgs<'a>` (around line 511) add after `cursor`:

```rust
    /// When true, the store populates the page's `explain` block.
    /// Callers are expected to set this only when `--explain` was requested
    /// (and the `policy_trace` capability is advertised — gating happens
    /// in the verb dispatcher, not the store).
    pub with_explain: bool,
```

In `SemanticSearchArgs<'a>` (around line 553) add after `model_label`:

```rust
    /// When true, the store populates the page's `explain` block. See
    /// [`KeywordSearchArgs::with_explain`].
    pub with_explain: bool,
```

In `HybridSearchArgs<'a>` (around line 583) add after `rerank_topk`:

```rust
    /// When true, the store populates the page's `explain` block. See
    /// [`KeywordSearchArgs::with_explain`].
    pub with_explain: bool,
```

- [ ] **Step 3: Add `explain` field to all three page structs**

Add a top-of-file import:

```rust
use crate::search::explain::ScoreExplain;
```

In `KeywordSearchPage` add after `next_cursor`:

```rust
    /// Optional per-candidate score-component explanations. Present only
    /// when the matching args' `with_explain` was true. For the keyword
    /// page, only `bm25_rank` is populated.
    pub explain: Option<Vec<ScoreExplain>>,
```

In `SemanticSearchPage` add after `candidates`:

```rust
    /// Optional per-candidate score-component explanations. Present only
    /// when the matching args' `with_explain` was true. For the semantic
    /// page, only `semantic_rank` is populated.
    pub explain: Option<Vec<ScoreExplain>>,
```

In `HybridSearchPage` add after `candidates`:

```rust
    /// Optional per-candidate score-component explanations. Present only
    /// when the matching args' `with_explain` was true. For the hybrid
    /// page, all fields are populated where applicable.
    pub explain: Option<Vec<ScoreExplain>>,
```

- [ ] **Step 4: Fix `StubStore` test impl**

In the `tests` module of the same file, find the `StubStore` impl. The test stub currently constructs `KeywordSearchPage { candidates: vec![], next_cursor: None }` (or similar). Add `explain: None,` to every page-struct instantiation. Same for `SemanticSearchPage` and `HybridSearchPage`.

- [ ] **Step 5: Run tests**

Run: `cargo check -p cairn-core --locked`
Expected: Clean compile.

Run: `cargo nextest run -p cairn-core --locked`
Expected: All tests pass (with stub updates from Step 4).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/contract/memory_store.rs
git commit -m "feat(store): add with_explain/explain plumbing to search args+pages (issue #49)"
```

---

## Task 4: Wire FTS leg in store to populate `explain`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/trait_impl.rs` (or the file holding `do_search_keyword` / `do_search_semantic`)

- [ ] **Step 1: Locate the leg implementations**

Run: `grep -rn "fn do_search_keyword\|fn do_search_semantic" crates/cairn-store-sqlite/src/`

Note the file paths. (Likely `store/trait_impl.rs` or `store/keyword.rs` / `store/semantic.rs`.)

- [ ] **Step 2: Update `do_search_keyword` to emit `explain` when requested**

At the end of `do_search_keyword` (just before constructing `KeywordSearchPage`), add:

```rust
let explain = if args.with_explain {
    Some(
        candidates
            .iter()
            .enumerate()
            .map(|(i, c)| cairn_core::search::ScoreExplain {
                record_id: c.record_id.clone(),
                bm25_rank: Some(i + 1),
                semantic_rank: None,
                rrf_score: 0.0,
                cosine: None,
                final_score: c.bm25,
            })
            .collect(),
    )
} else {
    None
};
```

Update the page construction to include `explain`:

```rust
Ok(KeywordSearchPage {
    candidates,
    next_cursor,
    explain,
})
```

- [ ] **Step 3: Update `do_search_semantic` similarly**

At the end of `do_search_semantic`, before constructing the page:

```rust
let explain = if args.with_explain {
    Some(
        candidates
            .iter()
            .enumerate()
            .map(|(i, c)| cairn_core::search::ScoreExplain {
                record_id: c.record_id.clone(),
                bm25_rank: None,
                semantic_rank: Some(i + 1),
                rrf_score: 0.0,
                cosine: None,
                final_score: f64::from(-c.semantic_distance.unwrap_or(0.0)),
            })
            .collect(),
    )
} else {
    None
};

Ok(SemanticSearchPage { candidates, explain })
```

- [ ] **Step 4: Run tests**

Run: `cargo check -p cairn-store-sqlite --locked`
Run: `cargo nextest run -p cairn-store-sqlite --locked --no-fail-fast`
Expected: Existing keyword/semantic integration tests still pass; no change in default behavior (explain stays `None` when `with_explain` defaults to `false`). Any test that constructs args literals must add `with_explain: false` — fix in place.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/
git commit -m "feat(store): populate keyword/semantic explain blocks when requested (issue #49)"
```

---

## Task 5: Wire hybrid leg to populate full `explain`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/hybrid.rs`

- [ ] **Step 1: Capture per-leg ranks in `do_search_hybrid`**

In `crates/cairn-store-sqlite/src/store/hybrid.rs`, inside `do_search_hybrid` after the `let kw_list = scored_from_keyword(&keyword.candidates);` and `let sem_list = scored_from_semantic(&semantic.candidates);` lines, build rank lookup maps:

```rust
use std::collections::HashMap;

let kw_ranks: HashMap<RecordId, usize> = kw_list
    .iter()
    .enumerate()
    .map(|(i, c)| (c.record_id.clone(), i + 1))
    .collect();
let sem_ranks: HashMap<RecordId, usize> = sem_list
    .iter()
    .enumerate()
    .map(|(i, c)| (c.record_id.clone(), i + 1))
    .collect();
```

- [ ] **Step 2: Build explain block from `reranked` and rank maps**

After the existing `let reranked = hybrid_search(...);` call, before the candidate hydration loop, add:

```rust
let explain = if args.with_explain {
    Some(
        reranked
            .iter()
            .map(|r| cairn_core::search::ScoreExplain {
                record_id: r.record_id.clone(),
                bm25_rank: kw_ranks.get(&r.record_id).copied(),
                semantic_rank: sem_ranks.get(&r.record_id).copied(),
                rrf_score: r.rrf_score,
                cosine: r.cosine,
                final_score: r.final_score,
            })
            .collect(),
    )
} else {
    None
};
```

- [ ] **Step 3: Trim explain to the `args.limit` window in lockstep**

After the `candidates` collection that already trims to `args.limit`, also trim `explain`:

```rust
let explain = explain.map(|exps| exps.into_iter().take(args.limit).collect());
```

Then update the `Ok(...)` return:

```rust
Ok(HybridSearchPage { candidates, explain })
```

- [ ] **Step 4: Add an inline test for `do_search_hybrid` explain emission**

At the bottom of `crates/cairn-store-sqlite/src/store/hybrid.rs` `tests` module add a `#[tokio::test]` that:
1. Opens an in-memory store (`open_in_memory_with_embedder` or equivalent — check `crates/cairn-store-sqlite/src/open.rs` for the test helper).
2. Inserts 3 records with distinct snippets.
3. Calls `do_search_hybrid` with `with_explain: true`.
4. Asserts `page.explain.is_some()`, `page.explain.as_ref().unwrap().len() == page.candidates.len()`, and that record_ids align by index.

If a test helper for in-memory store + embedder doesn't exist yet, defer this test to Task 13 (integration tests) and add only a unit-level shape assertion here:

```rust
#[test]
fn explain_field_default_is_none() {
    // Constructing a page literal with explain: None should still compile.
    let page = HybridSearchPage {
        candidates: vec![],
        explain: None,
    };
    assert!(page.explain.is_none());
}
```

- [ ] **Step 5: Run tests**

Run: `cargo check -p cairn-store-sqlite --locked`
Run: `cargo nextest run -p cairn-store-sqlite hybrid --locked`
Expected: New explain test passes; existing hybrid tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/hybrid.rs
git commit -m "feat(store): populate hybrid explain block from reranker output (issue #49)"
```

---

## Task 6: Add `score_explain` field to IDL `SearchData`

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/search.json`

- [ ] **Step 1: Add the schema field**

In `crates/cairn-idl/schema/verbs/search.json`, locate the `Data` definition (around line 124). After the `excluded` property, add:

```json
        "score_explain": {
          "type": "array",
          "description": "Optional per-candidate score-component explanations. Present only when args.explain is true (which itself requires the cairn.mcp.v1.policy_trace capability).",
          "items": { "$ref": "#/$defs/ScoreExplain" }
        }
```

Then in the top-level `$defs` object, add a `ScoreExplain` entry (alongside `Hit`, `filter`, etc.):

```json
    "ScoreExplain": {
      "type": "object",
      "additionalProperties": false,
      "required": ["record_id", "rrf_score", "final_score"],
      "properties": {
        "record_id": { "$ref": "../common/primitives.json#/$defs/Ulid" },
        "bm25_rank": { "type": "integer", "minimum": 1 },
        "semantic_rank": { "type": "integer", "minimum": 1 },
        "rrf_score": { "type": "number" },
        "cosine": { "type": "number" },
        "final_score": { "type": "number" }
      }
    }
```

- [ ] **Step 2: Regenerate code**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked`
Expected: Files under `crates/cairn-core/src/generated/` and `crates/cairn-mcp/src/generated/` updated with the new field.

- [ ] **Step 3: Verify codegen check passes**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
Expected: Exit 0 (no diff).

- [ ] **Step 4: Run all tests**

Run: `cargo nextest run --workspace --locked --no-fail-fast`
Expected: Existing tests pass; new generated `SearchDataScoreExplain` (or whatever cairn-codegen names it) compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-idl/schema/verbs/search.json \
        crates/cairn-core/src/generated/ \
        crates/cairn-mcp/src/generated/
git commit -m "feat(idl): add SearchData.score_explain schema field (issue #49)"
```

---

## Task 7: Build `cairn-core::verbs::search::run` dispatcher

**Files:**
- Create: `crates/cairn-core/src/verbs/search.rs`
- Modify: `crates/cairn-core/src/verbs/mod.rs`

- [ ] **Step 1: Wire module**

Modify `crates/cairn-core/src/verbs/mod.rs`:

```rust
//! Verb implementations. See brief §8.0.
//!
//! Each verb is a pure function (or a small, pure-function tree) over a
//! typed input snapshot. Adapters live outside this module; bridging
//! adapters → verb inputs is the job of `cairn-cli`.

pub mod lint;
pub mod search;
```

- [ ] **Step 2: Write `SearchError` enum + dispatcher skeleton**

Create `crates/cairn-core/src/verbs/search.rs`:

```rust
//! `search` verb dispatcher.
//!
//! Single entry point used by every surface (CLI, SDK, MCP). Performs
//! capability gating, dispatches to the matching `MemoryStore` leg
//! (`search_keyword` / `search_semantic` / `search_hybrid`), applies
//! token-budget trimming, and packages the result + optional explain
//! block into the response envelope.
//!
//! No I/O beyond the `store.*` calls — keeps `cairn-core`'s adapter-free
//! invariant (CLAUDE.md §3).

use crate::config::{CairnConfig, CapabilitySet};
use crate::contract::memory_store::{
    HybridSearchArgs, KeywordSearchArgs, MemoryStore, SearchCandidate, SemanticSearchArgs,
};
use crate::domain::taxonomy::MemoryVisibility;
use crate::search::{ScoreExplain, token_budget_trim};

/// Mode requested by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchMode {
    /// FTS5 keyword leg only.
    Keyword,
    /// ANN vector leg only.
    Semantic,
    /// RRF fusion + cosine re-rank.
    Hybrid,
}

/// Inputs to [`run`].
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// Free-text query.
    pub query: String,
    /// Selected mode.
    pub mode: SearchMode,
    /// Page size.
    pub limit: usize,
    /// Visibility allowlist; empty = no narrowing.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Active embedding model label (for semantic + hybrid).
    pub model_label: String,
    /// `true` → request explain block from the store and surface it.
    /// Caller must have already verified the `policy_trace` capability.
    pub explain: bool,
}

/// Result of [`run`]: the trimmed candidate page plus optional explain.
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    /// Trimmed candidate page.
    pub candidates: Vec<SearchCandidate>,
    /// Per-candidate score-component explanations, in lockstep with
    /// `candidates`. Populated iff `request.explain` was true and the
    /// `policy_trace` capability was advertised.
    pub explain: Option<Vec<ScoreExplain>>,
}

/// Errors raised by the dispatcher.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SearchError {
    /// Required capability is not advertised by `status` in this incarnation.
    #[error("capability unavailable: {capability}")]
    CapabilityUnavailable {
        /// The capability identifier (e.g. `cairn.mcp.v1.search.hybrid`).
        capability: &'static str,
    },
    /// Args failed validation before dispatch.
    #[error("invalid args: {reason}")]
    InvalidArgs {
        /// Human-readable reason.
        reason: String,
    },
    /// Store impl raised an error.
    #[error(transparent)]
    Store(#[from] crate::contract::memory_store::StoreError),
}

const POLICY_TRACE_CAP: &str = "cairn.mcp.v1.policy_trace";

/// Fail-closed capability gate for `request.mode`.
fn gate_mode(mode: SearchMode, caps: &CapabilitySet) -> Result<(), SearchError> {
    let (ok, name) = match mode {
        SearchMode::Keyword => (caps.keyword_search, "cairn.mcp.v1.search.keyword"),
        SearchMode::Semantic => (caps.semantic_search, "cairn.mcp.v1.search.semantic"),
        SearchMode::Hybrid => (caps.hybrid_search, "cairn.mcp.v1.search.hybrid"),
    };
    if ok {
        Ok(())
    } else {
        Err(SearchError::CapabilityUnavailable { capability: name })
    }
}

/// Run the dispatcher.
///
/// Order of operations:
/// 1. Mode capability gate (fail closed).
/// 2. `--explain` capability gate (`policy_trace`).
/// 3. Build mode-specific `*SearchArgs` with `with_explain` set.
/// 4. Dispatch to `store.search_*`.
/// 5. Trim candidates + explain in lockstep using
///    `config.search.max_snippet_chars_per_page`.
///
/// # Errors
///
/// - [`SearchError::CapabilityUnavailable`] for missing mode or
///   `policy_trace` capability.
/// - [`SearchError::InvalidArgs`] when the query is empty.
/// - [`SearchError::Store`] propagated from the store impl.
pub async fn run(
    store: &dyn MemoryStore,
    config: &CairnConfig,
    caps: &CapabilitySet,
    request: SearchRequest,
) -> Result<SearchOutcome, SearchError> {
    if request.query.trim().is_empty() {
        return Err(SearchError::InvalidArgs {
            reason: "query is empty".to_owned(),
        });
    }
    gate_mode(request.mode, caps)?;
    if request.explain && !caps_advertises_policy_trace(caps) {
        return Err(SearchError::CapabilityUnavailable {
            capability: POLICY_TRACE_CAP,
        });
    }

    let visibility = if request.visibility_allowlist.is_empty() {
        vec![
            MemoryVisibility::Private,
            MemoryVisibility::Session,
            MemoryVisibility::Project,
            MemoryVisibility::Team,
            MemoryVisibility::Org,
            MemoryVisibility::Public,
        ]
    } else {
        request.visibility_allowlist.clone()
    };

    let (candidates, explain) = match request.mode {
        SearchMode::Keyword => {
            let args = KeywordSearchArgs {
                query: request.query.clone(),
                filter: None,
                visibility_allowlist: visibility,
                limit: request.limit,
                cursor: None,
                with_explain: request.explain,
            };
            let page = store.search_keyword(&args).await?;
            (page.candidates, page.explain)
        }
        SearchMode::Semantic => {
            let args = SemanticSearchArgs {
                query: request.query.clone(),
                filter: None,
                visibility_allowlist: visibility,
                limit: request.limit,
                model_label: request.model_label.clone(),
                with_explain: request.explain,
            };
            let page = store.search_semantic(&args).await?;
            (page.candidates, page.explain)
        }
        SearchMode::Hybrid => {
            let args = HybridSearchArgs {
                query: request.query.clone(),
                filter: None,
                visibility_allowlist: visibility,
                limit: request.limit,
                model_label: request.model_label.clone(),
                blend: config.search.rerank_blend,
                rrf_k: config.search.rrf_k,
                rerank_topk: config.search.rerank_topk,
                with_explain: request.explain,
            };
            let page = store.search_hybrid(&args).await?;
            (page.candidates, page.explain)
        }
    };

    let (candidates, explain) =
        token_budget_trim(candidates, explain, config.search.max_snippet_chars_per_page);

    Ok(SearchOutcome {
        candidates,
        explain,
    })
}

/// Whether the active capability set advertises `policy_trace`.
///
/// `CapabilitySet` does not currently carry a `policy_trace` flag; the
/// capability is advertised by `status` directly. For the dispatcher we
/// approximate by inspecting `caps.llm_extract` placeholder semantics —
/// **TODO(#49 follow-up)**: thread `policy_trace` through `CapabilitySet`
/// and derive from config.
///
/// For now: rely on the status-layer wiring. The CLI calls
/// `status::p0_capabilities_advertises("cairn.mcp.v1.policy_trace")` before
/// invoking the dispatcher and rejects there. Inside the dispatcher, treat
/// the field as advertised whenever `request.explain` is set — since the
/// caller is required to have gated already.
#[allow(clippy::needless_pass_by_value)]
fn caps_advertises_policy_trace(_caps: &CapabilitySet) -> bool {
    // Caller (CLI/SDK/MCP) is responsible for the policy_trace gate against
    // the IDL-level capability list. The dispatcher trusts that gate; this
    // function is a stub kept for future tightening when CapabilitySet
    // grows a policy_trace flag.
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{
        HybridSearchPage, KeywordSearchPage, MemoryStoreCapabilities, SemanticSearchPage,
        StoreError, UpsertOutcome,
    };
    use crate::contract::version::VersionRange;
    use crate::domain::record::MemoryRecord;
    use std::sync::Mutex;

    /// Stub store that records which leg was called.
    struct CallRecorder {
        calls: Mutex<Vec<&'static str>>,
        capabilities: MemoryStoreCapabilities,
    }

    #[async_trait::async_trait]
    impl MemoryStore for CallRecorder {
        fn name(&self) -> &str { "recorder" }
        fn capabilities(&self) -> &MemoryStoreCapabilities { &self.capabilities }
        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::exact(super::super::super::contract::memory_store::CONTRACT_VERSION)
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            unimplemented!()
        }
        async fn get(&self, _id: &crate::domain::RecordId)
            -> Result<Option<MemoryRecord>, StoreError> { Ok(None) }
        async fn search_keyword(&self, _args: &KeywordSearchArgs<'_>)
            -> Result<KeywordSearchPage, StoreError> {
            self.calls.lock().unwrap().push("keyword");
            Ok(KeywordSearchPage { candidates: vec![], next_cursor: None, explain: None })
        }
        async fn search_semantic(&self, _args: &SemanticSearchArgs<'_>)
            -> Result<SemanticSearchPage, StoreError> {
            self.calls.lock().unwrap().push("semantic");
            Ok(SemanticSearchPage { candidates: vec![], explain: None })
        }
        async fn search_hybrid(&self, _args: &HybridSearchArgs<'_>)
            -> Result<HybridSearchPage, StoreError> {
            self.calls.lock().unwrap().push("hybrid");
            Ok(HybridSearchPage { candidates: vec![], explain: None })
        }
    }

    fn caps(keyword: bool, semantic: bool, hybrid: bool) -> CapabilitySet {
        CapabilitySet {
            keyword_search: keyword,
            semantic_search: semantic,
            hybrid_search: hybrid,
            ..Default::default()
        }
    }

    fn req(mode: SearchMode) -> SearchRequest {
        SearchRequest {
            query: "hello".to_owned(),
            mode,
            limit: 10,
            visibility_allowlist: vec![],
            model_label: "MiniLM-L6-v2".to_owned(),
            explain: false,
        }
    }

    #[tokio::test]
    async fn keyword_routes_to_keyword_leg() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities { fts: true, vector: false, graph_edges: false, transactions: true },
        };
        let config = CairnConfig::default();
        run(&store, &config, &caps(true, false, false), req(SearchMode::Keyword))
            .await
            .expect("ok");
        assert_eq!(store.calls.lock().unwrap().as_slice(), &["keyword"]);
    }

    #[tokio::test]
    async fn semantic_rejected_when_capability_absent() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities::default(),
        };
        let config = CairnConfig::default();
        let err = run(&store, &config, &caps(true, false, false), req(SearchMode::Semantic))
            .await
            .err()
            .expect("expected error");
        match err {
            SearchError::CapabilityUnavailable { capability } => {
                assert_eq!(capability, "cairn.mcp.v1.search.semantic");
            }
            other => panic!("wrong error: {other:?}"),
        }
        assert!(store.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn hybrid_routes_when_capability_set() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities { fts: true, vector: true, graph_edges: false, transactions: true },
        };
        let config = CairnConfig::default();
        run(&store, &config, &caps(true, true, true), req(SearchMode::Hybrid))
            .await
            .expect("ok");
        assert_eq!(store.calls.lock().unwrap().as_slice(), &["hybrid"]);
    }

    #[tokio::test]
    async fn empty_query_rejected() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities::default(),
        };
        let config = CairnConfig::default();
        let mut request = req(SearchMode::Keyword);
        request.query = "  ".to_owned();
        let err = run(&store, &config, &caps(true, false, false), request).await.err().unwrap();
        assert!(matches!(err, SearchError::InvalidArgs { .. }));
    }
}
```

> **Note on `caps_advertises_policy_trace`:** the design spec calls for the dispatcher to gate `--explain` on `policy_trace`. `CapabilitySet` doesn't currently carry that flag (status-layer responsibility). For Task 7 we trust the caller's gate and document the follow-up. If reviewers prefer a stricter gate, add `policy_trace: bool` to `CapabilitySet` here and source it from `config.capabilities()`.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-core verbs::search --locked`
Expected: 4 tests pass.

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: Clean.

Run: `./scripts/check-core-boundary.sh`
Expected: Clean (cairn-core stays adapter-free).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verbs/mod.rs \
        crates/cairn-core/src/verbs/search.rs
git commit -m "feat(verbs): add cairn-core::verbs::search::run dispatcher (issue #49)"
```

---

## Task 8: Add `rebuild_from_db` to the store

**Files:**
- Create: `crates/cairn-store-sqlite/src/store/reindex_from_db.rs`
- Modify: `crates/cairn-store-sqlite/src/store/mod.rs`
- Modify: `crates/cairn-store-sqlite/src/lib.rs`

- [ ] **Step 1: Wire module**

Modify `crates/cairn-store-sqlite/src/store/mod.rs`:

```rust
pub(crate) mod reindex_from_db;
```

(Add alongside existing `pub(crate) mod reindex;`.)

- [ ] **Step 2: Implement `rebuild_from_db`**

Create `crates/cairn-store-sqlite/src/store/reindex_from_db.rs`:

```rust
//! `cairn admin reindex --from-db`: rebuild FTS5 + vector indexes from the
//! authoritative `records` table.
//!
//! Two transactions:
//!   TX1 — `DELETE FROM records_fts;` + repopulate from `records`
//!         (active = 1 AND tombstoned = 0).
//!   TX2 — `DELETE FROM record_vectors;` + enqueue all active records
//!         into `pending_embeddings` with `reason = 'rebuild_from_db'`.
//!
//! The caller drives the existing `drain_once` loop afterwards. vec0 +
//! FTS5 transaction interaction is unverified at the time of writing —
//! splitting into two transactions sidesteps the question; if a single
//! transaction is shown safe in a follow-up, fold them.
//!
//! Idempotent: re-running succeeds with the same final counts.

use std::sync::Arc;

use tracing::instrument;

use crate::error::StoreError;

/// Counts emitted by [`rebuild_from_db`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebuildStats {
    /// Rows inserted into `records_fts`.
    pub fts_rebuilt: u64,
    /// Rows enqueued into `pending_embeddings`.
    pub enqueued: u64,
}

/// Rebuild FTS5 + vector indexes from the authoritative `records` table.
///
/// # Errors
///
/// - [`StoreError`] for any underlying SQLite failure.
#[instrument(skip(conn), err)]
pub async fn rebuild_from_db(
    conn: Arc<tokio_rusqlite::Connection>,
) -> Result<RebuildStats, StoreError> {
    // TX1 — FTS5 mirror.
    let fts_rebuilt: u64 = conn
        .call(|c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM records_fts", [])?;
            // Column list mirrors the `records_fts` schema (see
            // crates/cairn-store-sqlite/src/migrations/sql/0030_records_fts_weighted.sql).
            // Keep this string in lockstep when the FTS column set changes.
            let inserted = tx.execute(
                "INSERT INTO records_fts (rowid, body, snippet, scope, kind)
                   SELECT rowid, body, snippet, scope, kind
                     FROM records
                    WHERE active = 1 AND tombstoned = 0",
                [],
            )?;
            tx.commit()?;
            Ok::<_, tokio_rusqlite::Error>(inserted as u64)
        })
        .await?;

    // TX2 — vector enqueue.
    let enqueued: u64 = conn
        .call(|c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM record_vectors", [])?;
            let inserted = tx.execute(
                "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
                   SELECT record_id, 'rebuild_from_db', 0, strftime('%s','now')
                     FROM records
                    WHERE active = 1 AND tombstoned = 0
                  ON CONFLICT(record_id) DO UPDATE
                    SET reason = 'rebuild_from_db', attempt_count = 0",
                [],
            )?;
            tx.commit()?;
            Ok::<_, tokio_rusqlite::Error>(inserted as u64)
        })
        .await?;

    Ok(RebuildStats {
        fts_rebuilt,
        enqueued,
    })
}
```

> **Note on column list:** the exact `records_fts` and `records` column lists must match the active schema. Before committing, run `grep -n "CREATE VIRTUAL TABLE records_fts\|records_fts(" crates/cairn-store-sqlite/src/migrations/sql/*.sql` and verify the `INSERT INTO records_fts (...)` column tuple matches. If `records_fts` uses different column names (e.g. `searchable_text` instead of `body`), update the SQL.

- [ ] **Step 3: Re-export from `lib.rs`**

In `crates/cairn-store-sqlite/src/lib.rs`, add to the re-exports near the existing `pub use store::reindex::{DrainStats, drain_once};`:

```rust
pub use store::reindex_from_db::{RebuildStats, rebuild_from_db};
```

- [ ] **Step 4: Run check + tests**

Run: `cargo check -p cairn-store-sqlite --locked`
Expected: Clean. If the SQL column list is wrong, fix it (Step 2 note).

Run: `cargo nextest run -p cairn-store-sqlite reindex --locked`
Expected: Existing reindex tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/mod.rs \
        crates/cairn-store-sqlite/src/store/reindex_from_db.rs \
        crates/cairn-store-sqlite/src/lib.rs
git commit -m "feat(store): add rebuild_from_db for FTS + vector reindex (issue #49)"
```

---

## Task 9: Destructive-fixture integration test for `rebuild_from_db`

**Files:**
- Create: `crates/cairn-store-sqlite/tests/reindex_from_db.rs`

- [ ] **Step 1: Inspect existing test helpers**

Run: `ls crates/cairn-store-sqlite/tests/ && grep -l "open_with_embedder\|open_in_memory" crates/cairn-store-sqlite/tests/*.rs`

Note an existing test that opens a store with the test embedder. Pattern its harness for our new test.

- [ ] **Step 2: Write the destructive-rebuild test**

Create `crates/cairn-store-sqlite/tests/reindex_from_db.rs`:

```rust
//! Destructive-fixture test: nuke records_fts + record_vectors, call
//! `rebuild_from_db`, verify both indexes are restored from `records`.

use std::sync::Arc;

use cairn_embeddings_local::{EmbeddingModelKind, ModelCache};
use cairn_store_sqlite::{rebuild_from_db, drain_once, open_with_embedder};
use tempfile::TempDir;

/// Build an empty vault directory with `.cairn/cairn.db` parent created.
fn vault() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let cairn = dir.path().join(".cairn");
    std::fs::create_dir_all(&cairn).expect("mkdir");
    let db = cairn.join("cairn.db");
    (dir, db)
}

/// Load the test embedding model (cache-aware).
fn embedder(vault_root: &std::path::Path) -> Arc<dyn cairn_embeddings_local::EmbeddingModel> {
    let models_root = vault_root.join(".cairn").join("models");
    let cache = ModelCache::new(&models_root);
    cache
        .ensure(EmbeddingModelKind::default())
        .expect("model load — make sure the test model is staged in the vault")
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_from_db_restores_both_indexes() {
    let (dir, db_path) = vault();
    let embedder = embedder(dir.path());
    let store = open_with_embedder(&db_path, Some(Arc::clone(&embedder)))
        .await
        .expect("open store");

    // Seed three records using cairn-test-fixtures.
    // (Use whatever helper the existing tests use; e.g. `seed_records(&store, 3).await`.)
    cairn_test_fixtures::seed_simple_records(&store, 3).await;

    let conn = store
        .raw_conn_for_admin()
        .expect("admin conn");

    // Sanity: indexes populated.
    let fts_before: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM records_fts", [], |r| r.get(0))
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .unwrap();
    assert_eq!(fts_before, 3);

    let vec_before: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM record_vectors", [], |r| r.get(0))
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .unwrap();
    assert!(vec_before > 0, "background drain should have populated vectors");

    // Destruction: drop both indexes.
    conn.call(|c| {
        c.execute("DELETE FROM records_fts", [])?;
        c.execute("DELETE FROM record_vectors", [])?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .unwrap();

    // Rebuild.
    let stats = rebuild_from_db(Arc::clone(conn))
        .await
        .expect("rebuild");
    assert_eq!(stats.fts_rebuilt, 3);
    assert_eq!(stats.enqueued, 3);

    // Drive the drain loop until empty.
    for _ in 0..1000 {
        let s = drain_once(Arc::clone(conn), Arc::clone(&embedder))
            .await
            .expect("drain_once");
        if s.remaining == 0 {
            break;
        }
    }

    // Assertions: FTS5 restored, vectors restored.
    let fts_after: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM records_fts", [], |r| r.get(0))
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .unwrap();
    assert_eq!(fts_after, 3);

    let vec_after: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM record_vectors", [], |r| r.get(0))
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .unwrap();
    assert!(vec_after > 0, "vectors should be re-embedded after drain");
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_from_db_is_idempotent() {
    let (dir, db_path) = vault();
    let embedder = embedder(dir.path());
    let store = open_with_embedder(&db_path, Some(Arc::clone(&embedder)))
        .await
        .expect("open store");
    cairn_test_fixtures::seed_simple_records(&store, 3).await;
    let conn = store.raw_conn_for_admin().expect("admin conn");
    let s1 = rebuild_from_db(Arc::clone(conn)).await.expect("rebuild 1");
    let s2 = rebuild_from_db(Arc::clone(conn)).await.expect("rebuild 2");
    assert_eq!(s1, s2);
}
```

> **Note on `seed_simple_records`:** if `cairn-test-fixtures` doesn't already export a helper that seeds N records into a store, add one in `crates/cairn-test-fixtures/src/lib.rs` (function signature `pub async fn seed_simple_records(store: &impl MemoryStore, n: usize)`) before this test compiles. If the existing fixtures have something close, use that name instead.

- [ ] **Step 3: Run the test**

Run: `cargo nextest run -p cairn-store-sqlite --test reindex_from_db --locked`
Expected: Both tests pass. If the test model is missing, the tests will fail at `embedder.ensure(...)`; document with `#[ignore]` and a comment if the test model is gated behind a feature, or run `cairn admin model fetch` once.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-store-sqlite/tests/reindex_from_db.rs \
        crates/cairn-test-fixtures/  # if seed helper added
git commit -m "test(store): destructive-fixture rebuild_from_db integration (issue #49)"
```

---

## Task 10: Add `--from-db` flag to `cairn admin reindex`

**Files:**
- Modify: `crates/cairn-cli/src/command.rs`
- Modify: `crates/cairn-cli/src/verbs/admin_reindex.rs`

- [ ] **Step 1: Register the flag in clap**

In `crates/cairn-cli/src/command.rs`, find the `reindex` subcommand definition (around line 268). Add the `--from-db` flag after `--all`:

```rust
                .arg(
                    clap::Arg::new("from-db")
                        .long("from-db")
                        .action(clap::ArgAction::SetTrue)
                        .help(
                            "Rebuild FTS5 + vector indexes from the authoritative records table \
                             (use after derived indexes are deleted or corrupted).",
                        ),
                )
```

- [ ] **Step 2: Update handler to dispatch `--from-db`**

In `crates/cairn-cli/src/verbs/admin_reindex.rs` modify the `run` function. Replace the existing `if !semantic { ... }` early bail with a branched dispatch. Add a `from_db = sub.get_flag("from-db")` near the existing flag reads.

Replace the early bail block:

```rust
let from_db = sub.get_flag("from-db");

if !semantic && !from_db {
    return bail(
        json,
        "admin reindex",
        "UsageError",
        "specify --semantic or --from-db",
        64, // EX_USAGE
    );
}
```

After model probe, branch on `from_db`. Add a new helper function `run_rebuild_from_db_async` that:
1. Opens the store with the embedder (same as the existing `run_reindex_async`).
2. Calls `cairn_store_sqlite::rebuild_from_db(conn)`.
3. Drives the drain loop using the existing pattern.
4. Emits a `ReindexOutput` augmented with `fts_rebuilt`.

Add a new fields struct:

```rust
#[derive(Debug, Serialize)]
struct RebuildOutput {
    fts_rebuilt: u64,
    drained: usize,
    failed: usize,
    remaining: usize,
}
```

Then a sibling async function:

```rust
#[allow(clippy::too_many_lines)]
async fn run_rebuild_from_db_async(
    db_path: &Path,
    models_root: &Path,
    kind: cairn_core::config::EmbeddingModelKind,
    json: bool,
) -> ExitCode {
    use anyhow::Context as _;
    use std::sync::Arc;

    let cache = cairn_embeddings_local::ModelCache::new(models_root);
    let embedder = match tokio::task::spawn_blocking(move || cache.ensure(kind))
        .await
        .context("join error")
        .and_then(|r| r.context("model load failed"))
    {
        Ok(e) => e,
        Err(e) => {
            return bail(json, "admin reindex", "Internal", &format!("{e:#}"), 1);
        }
    };

    let store = match cairn_store_sqlite::open_with_embedder(db_path, Some(Arc::clone(&embedder))).await {
        Ok(s) => s,
        Err(e) => {
            return bail(json, "admin reindex", "Internal", &format!("store open: {e}"), 1);
        }
    };

    let Some(raw_conn) = store.raw_conn_for_admin() else {
        return bail(json, "admin reindex", "Internal", "store connection not available", 1);
    };
    let conn = Arc::clone(raw_conn);

    let stats = match cairn_store_sqlite::rebuild_from_db(Arc::clone(&conn)).await {
        Ok(s) => s,
        Err(e) => {
            return bail(json, "admin reindex", "Internal", &format!("rebuild_from_db: {e}"), 1);
        }
    };

    let mut total_drained = 0usize;
    let mut total_failed = 0usize;
    let mut last_remaining = 0usize;
    for _ in 0..10_000u32 {
        match cairn_store_sqlite::drain_once(Arc::clone(&conn), Arc::clone(&embedder)).await {
            Ok(s) => {
                total_drained += s.drained;
                total_failed += s.failed;
                last_remaining = s.remaining;
                if s.remaining == 0 || (s.drained == 0 && s.failed == 0) {
                    break;
                }
            }
            Err(e) => {
                return bail(json, "admin reindex", "Internal", &format!("drain_once: {e}"), 1);
            }
        }
    }

    let out = RebuildOutput {
        fts_rebuilt: stats.fts_rebuilt,
        drained: total_drained,
        failed: total_failed,
        remaining: last_remaining,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&out)
                .expect("invariant: RebuildOutput is always serializable")
        );
    } else {
        println!(
            "cairn admin reindex --from-db: fts_rebuilt={} drained={} failed={} remaining={}",
            out.fts_rebuilt, out.drained, out.failed, out.remaining
        );
    }
    ExitCode::SUCCESS
}
```

In the existing `run` function, after the model presence check, dispatch:

```rust
if from_db {
    return rt.block_on(async move {
        run_rebuild_from_db_async(&db_path, &models_root, kind, json).await
    });
}
// existing semantic dispatch follows
```

- [ ] **Step 3: Update CLI docgen snapshot**

Run: `cargo run -p cairn-cli --bin cairn-docgen --locked -- --write`
Expected: docs under `docs/site/src/reference/generated/` updated with the new `--from-db` flag entry.

- [ ] **Step 4: Run check + lint**

Run: `cargo check -p cairn-cli --locked`
Run: `cargo clippy -p cairn-cli --all-targets --locked -- -D warnings`
Expected: Clean.

Run: `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`
Expected: Exit 0.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/command.rs \
        crates/cairn-cli/src/verbs/admin_reindex.rs \
        docs/site/src/reference/generated/
git commit -m "feat(cli): add cairn admin reindex --from-db flag (issue #49)"
```

---

## Task 11: Collapse CLI search verbs onto the dispatcher

**Files:**
- Modify: `crates/cairn-cli/src/verbs/search.rs`

- [ ] **Step 1: Replace per-mode runners with dispatcher call**

In `crates/cairn-cli/src/verbs/search.rs`, keep the top-level `run` function (parses flags, gates `--explain`). Replace the three `run_keyword`/`run_semantic`/`run_hybrid` plus their `_async` siblings with one `run_async` that:

1. Loads config + probes model.
2. Builds the embedder (when needed by mode).
3. Opens the store via `open_with_embedder_and_config`.
4. Builds a `SearchRequest` and calls `cairn_core::verbs::search::run`.
5. Renders the outcome (human or `--json`), including the `explain` block when present.

Skeleton (replace existing per-mode functions):

```rust
async fn run_async(
    sub: &ArgMatches,
    json: bool,
    explain: bool,
    mode: SearchMode,
) -> ExitCode {
    let query = sub.get_one::<String>("query").cloned().unwrap_or_default();
    let limit: usize = sub
        .get_one::<u32>("limit")
        .copied()
        .map_or(10, |l| usize::try_from(l.max(1)).unwrap_or(1));

    let vault_root = if let Ok(p) = std::env::var("CAIRN_VAULT") {
        std::path::PathBuf::from(p)
    } else {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    };
    let db_path = vault_root.join(".cairn").join("cairn.db");

    let config = match crate::config::load(&vault_root, &crate::config::CliOverrides::default()) {
        Ok(c) => c,
        Err(e) => {
            let op_id = new_operation_id();
            if json {
                emit_json(&serde_json::json!({
                    "operation_id": op_id.0,
                    "verb": "search",
                    "status": "error",
                    "error": { "code": "ConfigError", "message": format!("{e:#}") }
                }));
            } else {
                human_error("search", "ConfigError", &format!("{e:#}"), &op_id);
            }
            return ExitCode::from(78);
        }
    };

    let models_root = vault_root.join(".cairn").join("models");
    let cache = cairn_embeddings_local::ModelCache::new(&models_root);
    let kind = config.search.embedding_model;
    let model_present = cache.is_present(kind);
    let caps = config.capabilities(model_present);
    let provider = config.search.default_provider;

    if let Some(rc) = openai_feature_gate(provider, json) {
        return rc;
    }

    // Embedder is required for semantic + hybrid; for keyword it's optional.
    let embedder = if matches!(mode, SearchMode::Semantic | SearchMode::Hybrid) {
        match resolve_embedder(&vault_root, kind, provider).await {
            Ok(e) => Some(e),
            Err(rc) => return rc.emit(json),
        }
    } else {
        None
    };

    let store = match cairn_store_sqlite::open_with_embedder_and_config(
        &db_path,
        embedder,
        config.search.fts_column_weights,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            let op_id = new_operation_id();
            let msg = format!("store open: {e}");
            if json {
                emit_json(&serde_json::json!({
                    "operation_id": op_id.0,
                    "verb": "search",
                    "status": "error",
                    "error": { "code": "Internal", "message": msg }
                }));
            } else {
                human_error("search", "Internal", &msg, &op_id);
            }
            return ExitCode::FAILURE;
        }
    };

    let request = cairn_core::verbs::search::SearchRequest {
        query,
        mode: match mode {
            SearchMode::Keyword => cairn_core::verbs::search::SearchMode::Keyword,
            SearchMode::Semantic => cairn_core::verbs::search::SearchMode::Semantic,
            SearchMode::Hybrid => cairn_core::verbs::search::SearchMode::Hybrid,
        },
        limit,
        visibility_allowlist: vec![],
        model_label: kind.as_str().to_owned(),
        explain,
    };

    match cairn_core::verbs::search::run(&store, &config, &caps, request).await {
        Ok(outcome) => render_outcome(&outcome, json),
        Err(cairn_core::verbs::search::SearchError::CapabilityUnavailable { capability }) => {
            let op_id = new_operation_id();
            let msg = format!("capability unavailable: {capability}");
            if json {
                emit_json(&serde_json::json!({
                    "operation_id": op_id.0,
                    "verb": "search",
                    "status": "error",
                    "error": { "code": "CapabilityUnavailable", "message": msg }
                }));
            } else {
                human_error("search", "CapabilityUnavailable", &msg, &op_id);
            }
            ExitCode::from(69)
        }
        Err(e) => {
            let op_id = new_operation_id();
            let msg = format!("{e}");
            if json {
                emit_json(&serde_json::json!({
                    "operation_id": op_id.0,
                    "verb": "search",
                    "status": "error",
                    "error": { "code": "Internal", "message": msg }
                }));
            } else {
                human_error("search", "Internal", &msg, &op_id);
            }
            ExitCode::FAILURE
        }
    }
}

/// Local enum mirroring the IDL `SearchArgsMode` to avoid leaking the
/// generated type into the dispatcher signature.
#[derive(Debug, Clone, Copy)]
enum SearchMode {
    Keyword,
    Semantic,
    Hybrid,
}

fn render_outcome(outcome: &cairn_core::verbs::search::SearchOutcome, json: bool) -> ExitCode {
    if json {
        let hits: Vec<serde_json::Value> = outcome
            .candidates
            .iter()
            .map(|c| {
                serde_json::json!({
                    "record_id": c.record_id.as_str(),
                    "bm25": c.bm25,
                    "semantic_distance": c.semantic_distance,
                    "snippet": c.snippet,
                })
            })
            .collect();
        let mut out = serde_json::json!({ "hits": hits });
        if let Some(exps) = outcome.explain.as_ref() {
            let arr: Vec<serde_json::Value> = exps
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "record_id": e.record_id.as_str(),
                        "bm25_rank": e.bm25_rank,
                        "semantic_rank": e.semantic_rank,
                        "rrf_score": e.rrf_score,
                        "cosine": e.cosine,
                        "final_score": e.final_score,
                    })
                })
                .collect();
            out["score_explain"] = serde_json::Value::Array(arr);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&out).unwrap_or_default()
        );
    } else if outcome.candidates.is_empty() {
        println!("search: no results");
    } else {
        for (i, c) in outcome.candidates.iter().enumerate() {
            let dist = c
                .semantic_distance
                .map_or_else(|| "n/a".to_owned(), |d| format!("{d:.4}"));
            println!(
                "{}. [{}] bm25={:.4} dist={} {}",
                i + 1,
                c.record_id.as_str(),
                c.bm25,
                dist,
                c.snippet,
            );
        }
        if let Some(exps) = outcome.explain.as_ref() {
            println!("\n--- score explain ---");
            for e in exps {
                println!(
                    "  [{}] bm25_rank={:?} sem_rank={:?} rrf={:.4} cos={:?} final={:.4}",
                    e.record_id.as_str(),
                    e.bm25_rank,
                    e.semantic_rank,
                    e.rrf_score,
                    e.cosine,
                    e.final_score
                );
            }
        }
    }
    ExitCode::SUCCESS
}
```

- [ ] **Step 2: Wire dispatch into `run`**

Replace the existing `match mode { ... }` block in `run`:

```rust
let rt = match tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
{
    Ok(rt) => rt,
    Err(e) => {
        let op_id = new_operation_id();
        let msg = format!("runtime build: {e}");
        if json {
            emit_json(&serde_json::json!({
                "operation_id": op_id.0,
                "verb": "search",
                "status": "error",
                "error": { "code": "Internal", "message": msg }
            }));
        } else {
            human_error("search", "Internal", &msg, &op_id);
        }
        return ExitCode::FAILURE;
    }
};

let mode_local = match mode {
    SearchArgsMode::Keyword => SearchMode::Keyword,
    SearchArgsMode::Semantic => SearchMode::Semantic,
    SearchArgsMode::Hybrid => SearchMode::Hybrid,
    _ => {
        let resp = unimplemented_response(ResponseVerb::Search);
        if json { emit_json(&resp); }
        else { human_error("search", "Internal", "unknown search mode", &resp.operation_id); }
        return ExitCode::FAILURE;
    }
};

rt.block_on(async move { run_async(sub, json, explain, mode_local).await })
```

Delete the old `run_keyword`, `run_semantic`, `run_semantic_async`, `run_hybrid`, `run_hybrid_async`, `render_semantic_results`, `render_hybrid_results` functions — replaced by `run_async` + `render_outcome`. Keep `openai_feature_gate`, `EmbedderInitError`, `resolve_embedder`, `resolve_local_embedder`, `resolve_openai_embedder`.

- [ ] **Step 3: Run smoke test**

Run: `cargo build -p cairn-cli --locked`
Expected: Clean.

Manual smoke test (in a tmp vault with model staged):
```bash
CAIRN_VAULT=/tmp/cairn-test cargo run -p cairn-cli -- search "hello" --mode keyword --json
CAIRN_VAULT=/tmp/cairn-test cargo run -p cairn-cli -- search "hello" --mode hybrid --explain --json
```
Expected: JSON envelope with `hits` array; second invocation has `score_explain` array.

- [ ] **Step 4: Run lint + tests**

Run: `cargo clippy -p cairn-cli --all-targets --locked -- -D warnings`
Run: `cargo nextest run -p cairn-cli --locked`
Expected: Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/search.rs
git commit -m "refactor(cli): collapse search verbs onto cairn-core dispatcher (issue #49)"
```

---

## Task 12: Wire SDK `search()` to the dispatcher

**Files:**
- Modify: `crates/cairn-sdk/src/transport.rs`
- Modify: `crates/cairn-sdk/src/lib.rs`
- Modify: `crates/cairn-sdk/Cargo.toml` (likely already has `cairn-core` dep; needs `tokio` for `block_on` in tests)

- [ ] **Step 1: Add a store-aware constructor**

In `crates/cairn-sdk/src/transport.rs` add at the top imports:

```rust
use std::sync::Arc;
use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::MemoryStore;
```

Add a new field to `SdkClient` (search the file for the existing struct):

```rust
pub struct SdkClient {
    // ...existing fields...
    store: Option<Arc<dyn MemoryStore>>,
    config: CairnConfig,
}
```

Add constructor:

```rust
impl SdkClient {
    /// Construct a client wired to an in-process store. The store is
    /// shared (`Arc`) so callers can keep their own handle.
    #[must_use]
    pub fn with_store(store: Arc<dyn MemoryStore>, config: CairnConfig) -> Self {
        Self {
            // ...existing fields default-init or copied from `Self::new()`...
            store: Some(store),
            config,
        }
    }
}
```

Update the existing `Self::new()` (or whatever the no-store constructor is named) to default `store: None`, `config: CairnConfig::default()`.

- [ ] **Step 2: Wire `search()` to dispatch**

Modify `SdkClient::search` (around line 140):

```rust
pub async fn search(&self, args: &SearchArgs) -> Result<VerbResponse<SearchData>, SdkError> {
    validate_search(args)?;
    self.require_capability(args.mode.capability())?;
    if args.explain == Some(true) {
        self.require_capability(Some("cairn.mcp.v1.policy_trace"))?;
    }

    let Some(store) = self.store.as_ref() else {
        return Err(unimplemented("search"));
    };

    let mode = match args.mode {
        cairn_core::generated::verbs::search::SearchArgsMode::Keyword =>
            cairn_core::verbs::search::SearchMode::Keyword,
        cairn_core::generated::verbs::search::SearchArgsMode::Semantic =>
            cairn_core::verbs::search::SearchMode::Semantic,
        cairn_core::generated::verbs::search::SearchArgsMode::Hybrid =>
            cairn_core::verbs::search::SearchMode::Hybrid,
        _ => return Err(unimplemented("search")),
    };

    let caps = self.config.capabilities(true); // SDK caller is responsible for not asking for semantic without a model

    let request = cairn_core::verbs::search::SearchRequest {
        query: args.query.clone(),
        mode,
        limit: args.limit.unwrap_or(10) as usize,
        visibility_allowlist: vec![],
        model_label: self.config.search.embedding_model.as_str().to_owned(),
        explain: args.explain.unwrap_or(false),
    };

    match cairn_core::verbs::search::run(store.as_ref(), &self.config, &caps, request).await {
        Ok(outcome) => Ok(envelope_from_outcome(outcome)),
        Err(cairn_core::verbs::search::SearchError::CapabilityUnavailable { capability }) => {
            Err(SdkError::CapabilityUnavailable {
                capability: capability.to_owned(),
                reason: "rejected by dispatcher".to_owned(),
                operation_id: crate::stub::new_operation_id(),
            })
        }
        Err(cairn_core::verbs::search::SearchError::InvalidArgs { reason }) => {
            Err(SdkError::InvalidArgs { reason })
        }
        Err(cairn_core::verbs::search::SearchError::Store(e)) => {
            Err(SdkError::Internal { message: format!("{e}") })
        }
    }
}
```

> **Note:** the existing `SdkClient::search` is sync (no `async`). If the trait signature is sync, change it to `pub async fn search` — or wrap the dispatcher with a `tokio::runtime::Handle::current().block_on(...)`. Inspect the existing signature (line ~140) before deciding; if sync, prefer making it async (breaking change is acceptable; SDK is pre-1.0).

- [ ] **Step 3: Implement `envelope_from_outcome`**

Below `search()` add:

```rust
fn envelope_from_outcome(
    outcome: cairn_core::verbs::search::SearchOutcome,
) -> VerbResponse<SearchData> {
    use cairn_core::generated::verbs::search::{SearchData, SearchDataHit};
    let hits: Vec<SearchDataHit> = outcome
        .candidates
        .iter()
        .map(|c| SearchDataHit {
            record_id: cairn_core::generated::common::Ulid(c.record_id.as_str().to_owned()),
            score: c.bm25,
            snippet: Some(c.snippet.clone()),
            citation: None,
            trust: cairn_core::generated::verbs::search::SearchDataHitTrust::Unknown,
        })
        .collect();
    let score_explain = outcome.explain.map(|exps| {
        // Match the IDL `SearchData.score_explain` field generated by Task 6.
        exps.into_iter()
            .map(|e| cairn_core::generated::verbs::search::SearchDataScoreExplain {
                record_id: cairn_core::generated::common::Ulid(e.record_id.as_str().to_owned()),
                bm25_rank: e.bm25_rank.map(|r| r as i64),
                semantic_rank: e.semantic_rank.map(|r| r as i64),
                rrf_score: e.rrf_score,
                cosine: e.cosine,
                final_score: e.final_score,
            })
            .collect()
    });
    let data = SearchData {
        hits,
        next_cursor: None,
        excluded: None,
        score_explain,
    };
    VerbResponse {
        operation_id: crate::stub::new_operation_id(),
        verb: cairn_core::generated::envelope::ResponseVerb::Search,
        status: cairn_core::generated::envelope::ResponseStatus::Ok,
        data: Some(data),
        error: None,
    }
}
```

(Field names like `SearchDataHit`, `SearchDataScoreExplain` come from cairn-codegen output — adjust to match the actual generated identifiers after Task 6 reruns codegen.)

- [ ] **Step 4: Re-export from `lib.rs`**

In `crates/cairn-sdk/src/lib.rs` ensure `SdkClient` and the new constructor are exported:

```rust
pub use transport::SdkClient;
```

- [ ] **Step 5: Tests**

Create `crates/cairn-sdk/tests/search_dispatch.rs`:

```rust
//! SDK search dispatch — verifies executive path through `with_store`.

use std::sync::Arc;
use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::{
    HybridSearchPage, KeywordSearchArgs, KeywordSearchPage, MemoryStore, MemoryStoreCapabilities,
    SemanticSearchArgs, SemanticSearchPage, StoreError,
};
use cairn_sdk::SdkClient;

struct EmptyStore;

#[async_trait::async_trait]
impl MemoryStore for EmptyStore {
    fn name(&self) -> &str { "empty" }
    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: true, vector: true, graph_edges: false, transactions: true,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> cairn_core::contract::version::VersionRange {
        cairn_core::contract::version::VersionRange::exact(
            cairn_core::contract::memory_store::CONTRACT_VERSION
        )
    }
    async fn upsert(
        &self, _r: &cairn_core::domain::record::MemoryRecord,
    ) -> Result<cairn_core::contract::memory_store::UpsertOutcome, StoreError> {
        unimplemented!()
    }
    async fn get(
        &self, _id: &cairn_core::domain::RecordId,
    ) -> Result<Option<cairn_core::domain::record::MemoryRecord>, StoreError> { Ok(None) }
    async fn search_keyword(
        &self, _args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError> {
        Ok(KeywordSearchPage { candidates: vec![], next_cursor: None, explain: None })
    }
    async fn search_semantic(
        &self, _args: &SemanticSearchArgs<'_>,
    ) -> Result<SemanticSearchPage, StoreError> {
        Ok(SemanticSearchPage { candidates: vec![], explain: None })
    }
    async fn search_hybrid(
        &self, _args: &cairn_core::contract::memory_store::HybridSearchArgs<'_>,
    ) -> Result<HybridSearchPage, StoreError> {
        Ok(HybridSearchPage { candidates: vec![], explain: None })
    }
}

#[tokio::test]
async fn keyword_dispatch_returns_empty_page() {
    let client = SdkClient::with_store(Arc::new(EmptyStore), CairnConfig::default());
    let args = cairn_sdk::SearchArgs {
        query: "hello".to_owned(),
        mode: cairn_core::generated::verbs::search::SearchArgsMode::Keyword,
        scope: None,
        filters: None,
        limit: Some(10),
        citations: None,
        cursor: None,
        explain: Some(false),
    };
    let resp = client.search(&args).await.expect("ok");
    assert!(resp.data.unwrap().hits.is_empty());
}
```

(Field name `cairn_sdk::SearchArgs` may live under `cairn_core::generated::verbs::search::SearchArgs` — adjust the import to match SDK's public re-export.)

- [ ] **Step 6: Run tests**

Run: `cargo nextest run -p cairn-sdk --locked`
Expected: New test passes; existing SDK tests still pass.

Run: `cargo clippy -p cairn-sdk --all-targets --locked -- -D warnings`
Expected: Clean.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-sdk/
git commit -m "feat(sdk): wire SdkClient::search to cairn-core dispatcher (issue #49)"
```

---

## Task 13: Wire MCP search tool to the dispatcher

**Files:**
- Modify: `crates/cairn-mcp/src/handler.rs`
- Modify: `crates/cairn-mcp/src/lib.rs`

- [ ] **Step 1: Inspect the current handler**

Run: `cat crates/cairn-mcp/src/handler.rs`

Note the existing dispatcher pattern. Identify the `search` tool handler and where the server is constructed.

- [ ] **Step 2: Add a store + config to the handler**

Add `store: Option<Arc<dyn MemoryStore>>` and `config: CairnConfig` fields to whatever struct holds the per-tool dispatch state (likely `Handler` or `ServerState`). Add a constructor `Handler::with_store(store, config)`.

In the `search` tool handler, deserialize args from the MCP request (which already validates against the IDL schema), build a `SearchRequest`, call the dispatcher, and serialize the outcome to the MCP response envelope.

```rust
async fn handle_search(
    &self,
    args: cairn_core::generated::verbs::search::SearchArgs,
) -> McpResult<cairn_core::generated::verbs::search::SearchData> {
    let Some(store) = self.store.as_ref() else {
        return Err(McpError::Unimplemented("search".into()));
    };
    let mode = match args.mode {
        cairn_core::generated::verbs::search::SearchArgsMode::Keyword =>
            cairn_core::verbs::search::SearchMode::Keyword,
        cairn_core::generated::verbs::search::SearchArgsMode::Semantic =>
            cairn_core::verbs::search::SearchMode::Semantic,
        cairn_core::generated::verbs::search::SearchArgsMode::Hybrid =>
            cairn_core::verbs::search::SearchMode::Hybrid,
        _ => return Err(McpError::InvalidArgs("unknown mode".into())),
    };
    let caps = self.config.capabilities(true);
    let request = cairn_core::verbs::search::SearchRequest {
        query: args.query.clone(),
        mode,
        limit: args.limit.unwrap_or(10) as usize,
        visibility_allowlist: vec![],
        model_label: self.config.search.embedding_model.as_str().to_owned(),
        explain: args.explain.unwrap_or(false),
    };
    let outcome = cairn_core::verbs::search::run(store.as_ref(), &self.config, &caps, request)
        .await
        .map_err(|e| match e {
            cairn_core::verbs::search::SearchError::CapabilityUnavailable { capability } =>
                McpError::CapabilityUnavailable(capability.to_owned()),
            cairn_core::verbs::search::SearchError::InvalidArgs { reason } =>
                McpError::InvalidArgs(reason),
            cairn_core::verbs::search::SearchError::Store(e) =>
                McpError::Internal(format!("{e}")),
        })?;
    Ok(data_from_outcome(outcome))
}
```

(Adjust `McpError` variants to whatever the existing handler uses — search the file.)

- [ ] **Step 3: Implement `data_from_outcome`**

Mirror `envelope_from_outcome` in the SDK, returning the IDL `SearchData` struct (without the envelope wrapper since MCP wraps separately):

```rust
fn data_from_outcome(
    outcome: cairn_core::verbs::search::SearchOutcome,
) -> cairn_core::generated::verbs::search::SearchData {
    use cairn_core::generated::verbs::search::{SearchData, SearchDataHit, SearchDataScoreExplain, SearchDataHitTrust};
    use cairn_core::generated::common::Ulid;
    let hits = outcome.candidates.iter().map(|c| SearchDataHit {
        record_id: Ulid(c.record_id.as_str().to_owned()),
        score: c.bm25,
        snippet: Some(c.snippet.clone()),
        citation: None,
        trust: SearchDataHitTrust::Unknown,
    }).collect();
    let score_explain = outcome.explain.map(|exps| exps.into_iter().map(|e| SearchDataScoreExplain {
        record_id: Ulid(e.record_id.as_str().to_owned()),
        bm25_rank: e.bm25_rank.map(|r| r as i64),
        semantic_rank: e.semantic_rank.map(|r| r as i64),
        rrf_score: e.rrf_score,
        cosine: e.cosine,
        final_score: e.final_score,
    }).collect());
    SearchData { hits, next_cursor: None, excluded: None, score_explain }
}
```

- [ ] **Step 4: Tests**

Create `crates/cairn-mcp/tests/search_tool.rs` mirroring the SDK test in Task 12 — construct a handler with a stub store, send a search request, assert the response shape. Reuse the `EmptyStore` pattern from Task 12 (move to `cairn-test-fixtures` if it'll be needed in three places).

- [ ] **Step 5: Run tests**

Run: `cargo nextest run -p cairn-mcp --locked`
Run: `cargo clippy -p cairn-mcp --all-targets --locked -- -D warnings`
Expected: Clean.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-mcp/
git commit -m "feat(mcp): wire search tool to cairn-core dispatcher (issue #49)"
```

---

## Task 14: Build hybrid test vault helper in `cairn-test-fixtures`

**Files:**
- Create: `crates/cairn-test-fixtures/src/hybrid_vault.rs`
- Modify: `crates/cairn-test-fixtures/src/lib.rs`

- [ ] **Step 1: Design the helper API**

```rust
//! `build_hybrid_test_vault` — produce a tempdir-rooted vault with the
//! test embedding model staged, an open SQLite store, and N seeded records
//! ready for `cairn search` integration tests.

use std::path::PathBuf;
use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore;
use cairn_embeddings_local::EmbeddingModelKind;
use tempfile::TempDir;

/// Spec for one record to seed.
pub struct RecordSpec {
    /// Body text indexed by FTS5 + embedded.
    pub body: String,
    /// Snippet; if empty, derived from body.
    pub snippet: String,
}

impl RecordSpec {
    /// Build from a single body string; snippet = first 80 chars of body.
    #[must_use]
    pub fn from_body(body: impl Into<String>) -> Self {
        let body = body.into();
        let snippet = body.chars().take(80).collect();
        Self { body, snippet }
    }
}

/// Returned harness; drop the `dir` to clean up.
pub struct HybridTestVault {
    /// Tempdir (drop = cleanup).
    pub dir: TempDir,
    /// Vault root inside the tempdir.
    pub root: PathBuf,
    /// Path to `.cairn/cairn.db`.
    pub db_path: PathBuf,
    /// The opened store.
    pub store: Arc<dyn MemoryStore>,
}

/// Build a vault with `records` seeded, return the harness.
///
/// # Panics
///
/// Panics on any setup error — these helpers are for tests.
pub async fn build_hybrid_test_vault(records: &[RecordSpec]) -> HybridTestVault {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path().to_path_buf();
    let cairn = root.join(".cairn");
    std::fs::create_dir_all(cairn.join("models")).expect("mkdir");
    let db_path = cairn.join("cairn.db");

    let cache = cairn_embeddings_local::ModelCache::new(&cairn.join("models"));
    let kind = EmbeddingModelKind::default();
    let embedder = cache.ensure(kind).expect("test model load");

    let store = cairn_store_sqlite::open_with_embedder(&db_path, Some(Arc::clone(&embedder)))
        .await
        .expect("open store");

    for spec in records {
        seed_one_record(&store, spec).await;
    }

    // Drain the embedding queue so semantic search has vectors before the
    // test invokes search.
    let conn = store.raw_conn_for_admin().expect("raw conn");
    for _ in 0..100 {
        let stats = cairn_store_sqlite::drain_once(Arc::clone(conn), Arc::clone(&embedder))
            .await
            .expect("drain");
        if stats.remaining == 0 { break; }
    }

    HybridTestVault {
        dir,
        root,
        db_path,
        store: Arc::new(store),
    }
}

/// Insert one record with a `MemoryRecord` built from the spec.
async fn seed_one_record(_store: &impl MemoryStore, _spec: &RecordSpec) {
    // Use the existing record-builder fixture (`MemoryRecord::test_builder()`
    // or similar) — adjust to match the project's seed pattern.
    todo!("implement using existing test record builder; see crates/cairn-test-fixtures/src/lib.rs for prior helpers")
}
```

> **Note:** the `seed_one_record` body is intentionally `todo!()` — every project has its own preferred record builder. Before committing, replace with the established pattern from `crates/cairn-test-fixtures/src/lib.rs` or the existing tests.

- [ ] **Step 2: Wire module**

In `crates/cairn-test-fixtures/src/lib.rs`:

```rust
pub mod hybrid_vault;
pub use hybrid_vault::{HybridTestVault, RecordSpec, build_hybrid_test_vault};
```

- [ ] **Step 3: Smoke-test the helper**

Add an inline test at the bottom of `hybrid_vault.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn build_vault_with_three_records() {
        let vault = build_hybrid_test_vault(&[
            RecordSpec::from_body("the quick brown fox"),
            RecordSpec::from_body("jumps over the lazy dog"),
            RecordSpec::from_body("rust is memory safe"),
        ])
        .await;
        assert!(vault.db_path.exists());
    }
}
```

- [ ] **Step 4: Run**

Run: `cargo nextest run -p cairn-test-fixtures hybrid_vault --locked`
Expected: Pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-test-fixtures/
git commit -m "test(fixtures): add build_hybrid_test_vault helper (issue #49)"
```

---

## Task 15: Golden-query CLI snapshot tests

**Files:**
- Create: `crates/cairn-cli/tests/search_modes_golden.rs`
- Create: `crates/cairn-cli/tests/search_explain.rs`
- Create: `crates/cairn-cli/tests/admin_reindex_from_db.rs`

- [ ] **Step 1: Golden-query tests for the three modes**

Create `crates/cairn-cli/tests/search_modes_golden.rs`:

```rust
//! Golden-query CLI snapshot tests.
//!
//! Builds a fixture vault, runs `cairn search ... --json` for each mode,
//! and snapshots the output via `insta`. Assertions are insta-driven: edit
//! snapshots with `cargo insta review` after intentional changes.

use assert_cmd::Command;
use cairn_test_fixtures::{RecordSpec, build_hybrid_test_vault};

const QUERY: &str = "memory safety in rust";

async fn fixture() -> cairn_test_fixtures::HybridTestVault {
    build_hybrid_test_vault(&[
        RecordSpec::from_body("rust offers memory safety without garbage collection"),
        RecordSpec::from_body("the quick brown fox jumps over the lazy dog"),
        RecordSpec::from_body("ownership and borrowing prevent memory bugs at compile time"),
        RecordSpec::from_body("python is dynamically typed"),
    ]).await
}

#[tokio::test(flavor = "multi_thread")]
async fn keyword_mode_snapshot() {
    let vault = fixture().await;
    let output = Command::cargo_bin("cairn")
        .unwrap()
        .env("CAIRN_VAULT", &vault.root)
        .args(["search", QUERY, "--mode", "keyword", "--json"])
        .output()
        .expect("run cli");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    insta::assert_snapshot!("search_keyword_json", stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn semantic_mode_snapshot() {
    let vault = fixture().await;
    let output = Command::cargo_bin("cairn")
        .unwrap()
        .env("CAIRN_VAULT", &vault.root)
        .args(["search", QUERY, "--mode", "semantic", "--json"])
        .output()
        .expect("run cli");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    insta::assert_snapshot!("search_semantic_json", stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn hybrid_mode_snapshot() {
    let vault = fixture().await;
    let output = Command::cargo_bin("cairn")
        .unwrap()
        .env("CAIRN_VAULT", &vault.root)
        .args(["search", QUERY, "--mode", "hybrid", "--json"])
        .output()
        .expect("run cli");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    insta::assert_snapshot!("search_hybrid_json", stdout);
}
```

- [ ] **Step 2: `--explain` snapshot test**

Create `crates/cairn-cli/tests/search_explain.rs`:

```rust
//! `--explain` golden test: assert score_explain block is present and
//! aligns with hits.

use assert_cmd::Command;
use cairn_test_fixtures::{RecordSpec, build_hybrid_test_vault};

#[tokio::test(flavor = "multi_thread")]
async fn hybrid_explain_block_snapshot() {
    let vault = build_hybrid_test_vault(&[
        RecordSpec::from_body("rust offers memory safety without garbage collection"),
        RecordSpec::from_body("ownership and borrowing prevent memory bugs at compile time"),
        RecordSpec::from_body("python is dynamically typed"),
    ]).await;

    let output = Command::cargo_bin("cairn")
        .unwrap()
        .env("CAIRN_VAULT", &vault.root)
        .args(["search", "memory safety", "--mode", "hybrid", "--explain", "--json"])
        .output()
        .expect("run cli");
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    // Quick structural check before the snapshot.
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert!(parsed.get("score_explain").is_some(), "score_explain absent: {stdout}");
    let hits = parsed["hits"].as_array().unwrap();
    let exps = parsed["score_explain"].as_array().unwrap();
    assert_eq!(hits.len(), exps.len(), "explain length must match hits");

    insta::assert_snapshot!("search_explain_json", stdout);
}
```

- [ ] **Step 3: Destructive-rebuild CLI snapshot**

Create `crates/cairn-cli/tests/admin_reindex_from_db.rs`:

```rust
//! `cairn admin reindex --from-db` end-to-end via assert_cmd.

use assert_cmd::Command;
use cairn_test_fixtures::{RecordSpec, build_hybrid_test_vault};

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_from_db_snapshot() {
    let vault = build_hybrid_test_vault(&[
        RecordSpec::from_body("alpha"),
        RecordSpec::from_body("bravo"),
        RecordSpec::from_body("charlie"),
    ]).await;

    // Destruction: open the DB and nuke both indexes via raw SQLite.
    let conn = rusqlite::Connection::open(&vault.db_path).expect("open");
    conn.execute("DELETE FROM records_fts", []).unwrap();
    conn.execute("DELETE FROM record_vectors", []).unwrap();
    drop(conn);

    let output = Command::cargo_bin("cairn")
        .unwrap()
        .env("CAIRN_VAULT", &vault.root)
        .args(["admin", "reindex", "--from-db", "--json"])
        .output()
        .expect("run cli");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(parsed["fts_rebuilt"].as_u64().unwrap(), 3);
    assert!(parsed["drained"].as_u64().unwrap() >= 3);

    // Snapshot the *shape* (counts + zero failures + zero remaining).
    insta::assert_snapshot!("admin_reindex_from_db", stdout);
}
```

- [ ] **Step 4: First-run snapshots**

Run: `cargo nextest run -p cairn-cli --test search_modes_golden --test search_explain --test admin_reindex_from_db --locked`
Expected: Snapshots auto-create as `.snap.new` files.

Run: `cargo insta review`
Action: Inspect each snapshot, accept if shape is correct (hits non-empty, score_explain aligned, fts_rebuilt = 3).

Re-run the test command. Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/tests/ \
        crates/cairn-cli/tests/snapshots/
git commit -m "test(cli): golden snapshots for search modes + --explain + --from-db (issue #49)"
```

---

## Task 16: Verification + cleanup

**Files:**
- (Verification only — no code changes unless failures surface.)

- [ ] **Step 1: Run full CI suite locally**

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

Expected: All commands exit 0. Fix any failures inline.

- [ ] **Step 2: Update traceability**

Edit `docs/design/traceability.md` to add row(s) mapping issue #49 → the brief sections it implements (§5.1, §8 search, §15 capability negotiation). One-line entry per section.

- [ ] **Step 3: Commit**

```bash
git add docs/design/traceability.md
git commit -m "docs(traceability): record issue #49 mapping to brief sections"
```

- [ ] **Step 4: Open the PR**

```bash
gh pr create \
  --title "feat(search): hybrid orchestration + reindex --from-db (issue #49)" \
  --body "$(cat <<'EOF'
Closes #49.

## Summary
- New `cairn-core::verbs::search::run` dispatcher; CLI/SDK/MCP all call it.
- Char-count proxy `token_budget_trim` in `cairn-core` with a configurable budget (`search.max_snippet_chars_per_page`).
- `ScoreExplain` block on `*SearchPage` and surfaced through CLI `--json`/`--explain` plus the `SearchData.score_explain` IDL field.
- `cairn admin reindex --from-db` rebuilds FTS5 + vectors from the authoritative `records` table.
- Keyword CLI mode wired (TODO from #46 closed).
- Golden CLI snapshots for keyword/semantic/hybrid + explain + destructive rebuild.

## Brief sections touched
§3.0, §5.1, §8 (search), §15 (capability negotiation).

## Verification
- `cargo nextest run --workspace --locked --no-fail-fast` ✅
- `cargo clippy --workspace --all-targets --locked -- -D warnings` ✅
- `./scripts/check-core-boundary.sh` ✅
- `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check` ✅
- `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check` ✅
EOF
)"
```

---

## Self-Review Notes

**Spec coverage:**
- Hybrid orchestration with deterministic weighting/dedup → Tasks 4+5 (existing orchestrator + new explain emission).
- Token-budget trimming → Task 2.
- Component-score explanations for lint/eval debugging → Tasks 1, 3, 4, 5, 6, 11, 12, 13.
- `search_mode` exposure on CLI/MCP/SDK + capability negotiation → Tasks 7, 11, 12, 13.
- `cairn admin reindex --from-db` → Tasks 8, 9, 10.
- US7 golden queries for the three modes → Task 15 (`search_modes_golden.rs`).
- Reindex destructive fixture → Task 9 (`reindex_from_db.rs`).
- Latency smoke tests → not explicitly tasked; existing `cairn-bench` harness covers it. Add a follow-up issue if smoke threshold lands in CI.

**Type consistency:** `cairn_core::verbs::search::SearchMode` is the canonical enum; CLI's local `SearchMode` (Task 11) maps onto it. SDK and MCP use the IDL-generated `SearchArgsMode` and map to `cairn_core::verbs::search::SearchMode` at the dispatch boundary. `ScoreExplain` is identical across core/store/CLI; the IDL-generated `SearchDataScoreExplain` is the wire form (Task 6) — generated identifier may be `SearchDataScoreExplainItem` or similar; adjust at codegen time.

**Open items (intentionally deferred):**
- `policy_trace` flag on `CapabilitySet` — Task 7 documents this as a follow-up; current dispatcher trusts the caller's gate.
- Single-transaction `rebuild_from_db` once vec0 + FTS5 cooperation is verified (Task 8 note).
- Token-accurate trim via embedder tokenizer (P1, brief §11).
