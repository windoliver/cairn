//! Hybrid retrieval: parallel keyword + semantic, RRF fusion, cosine re-rank.
//!
//! Pipeline:
//!
//! 1. Capability gate — `caps.vector` must be true and an embedder must be wired.
//! 2. Run [`SqliteMemoryStore::do_search_keyword`] and
//!    [`SqliteMemoryStore::do_search_semantic`] in parallel via
//!    [`tokio::try_join!`]. Each leg over-fetches (`limit = 50`) so RRF has a
//!    healthy candidate pool to fuse.
//! 3. Embed the query a second time on the blocking pool for the cosine
//!    re-rank pass. (`do_search_semantic` already embedded once internally;
//!    avoiding the double-embed is a v0.2 optimization out of scope here.)
//! 4. Issue a single non-MATCH `record_vectors` SELECT to fetch top-K vectors
//!    by `record_id`. Filtering on the auxiliary `model` column is permitted
//!    in this query because no `MATCH` clause is present (vec0 only blocks
//!    `+col` predicates on KNN queries — see `do_search_semantic` doc).
//! 5. Drive the pure `cairn_core::search::hybrid_search` orchestrator with
//!    the fetched data. Hydrate the returned `RerankedCandidate`s back into
//!    `SearchCandidate` rows from the underlying legs so callers see snippets,
//!    bm25, and `semantic_distance` exactly as the legs produced them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cairn_core::contract::memory_store::{
    GraphNeighborsArgs, HybridSearchArgs, HybridSearchPage, KeywordSearchArgs, SearchCandidate,
    SemanticSearchArgs,
};
use cairn_core::domain::RecordId;
use cairn_core::search::{
    DegradationReason, DegradedLeg, GraphCandidate, GraphSource, HybridSearchInputs,
    HybridSearchParams, RerankedCandidate, ScoreExplain, ScoredCandidate, hybrid_search,
};
use cairn_embeddings_local::EmbeddingModel;
use rusqlite::types::Value as SqlVal;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

/// Candidate-pool size each leg fetches before RRF fusion. Set generously so
/// the fusion has overlap to work with even when the legs disagree heavily;
/// the final page is trimmed by `args.limit`.
const HYBRID_LEG_LIMIT: usize = 50;

/// Auth-only graph-seed overfetch (spec §4.3 / §5.1). The graph leg gets
/// this many seed record ids — independent of the lexical legs' filtered
/// pool — so neighbors of records outside the user's narrowing filter
/// remain reachable through graph traversal as long as they pass
/// authorization.
const GRAPH_SEED_OVERFETCH: usize = 400;

impl SqliteMemoryStore {
    /// Inherent `search_hybrid` implementation; the trait method
    /// [`MemoryStore::search_hybrid`] guards `self.conn` then delegates here.
    ///
    /// [`MemoryStore::search_hybrid`]: cairn_core::contract::memory_store::MemoryStore::search_hybrid
    ///
    /// # Errors
    ///
    /// - [`StoreError::CapabilityUnavailable`] when `caps.vector` is `false`
    ///   or no embedder is wired.
    /// - Any error from the underlying keyword or semantic legs.
    /// - [`StoreError::Invariant`] when the cosine re-rank embedding task
    ///   fails or panics.
    #[instrument(
        skip(self, args),
        err,
        fields(verb = "search_hybrid", limit = args.limit, blend = args.blend),
    )]
    pub(crate) async fn do_search_hybrid(
        &self,
        args: &HybridSearchArgs<'_>,
    ) -> Result<HybridSearchPage, StoreError> {
        // Capability gate — fail closed (brief §4 invariant 6).
        if !self.caps.vector {
            return Err(StoreError::CapabilityUnavailable { what: "vector" });
        }
        let embedder = self
            .embedder
            .as_ref()
            .ok_or(StoreError::CapabilityUnavailable { what: "vector" })?
            .clone();

        // Run keyword + semantic legs in parallel. The arg structs are
        // bound to locals so they live across the await — passing inline
        // with `&KeywordSearchArgs { ... }` would drop the temporary at
        // the end of the macro expansion's match arm and break the borrow.
        let kw_args = KeywordSearchArgs {
            query: args.query.clone(),
            filter: args.filter,
            auth_scope: args.auth_scope.clone(),
            visibility_allowlist: args.visibility_allowlist.clone(),
            limit: HYBRID_LEG_LIMIT,
            cursor: None,
            with_explain: false,
        };
        let sem_args = SemanticSearchArgs {
            query: args.query.clone(),
            filter: args.filter,
            auth_scope: args.auth_scope.clone(),
            visibility_allowlist: args.visibility_allowlist.clone(),
            limit: HYBRID_LEG_LIMIT,
            model_label: args.model_label.clone(),
            with_explain: false,
        };
        // Run keyword + semantic in parallel, but tolerate a semantic
        // failure: hybrid degrades to keyword + graph rather than hard-
        // failing. Keyword is the lexical anchor and remains a hard
        // requirement — if FTS5 fails, there's nothing meaningful to
        // rerank.
        let (kw_res, sem_res) = tokio::join!(
            self.do_search_keyword(&kw_args),
            self.do_search_semantic(&sem_args),
        );
        let keyword = kw_res?;
        let mut leg_degradations: Vec<DegradedLeg> = Vec::new();
        let semantic = match sem_res {
            Ok(page) => page,
            Err(e) => {
                tracing::warn!(error = %e, "semantic leg failed; degrading hybrid response");
                leg_degradations.push(DegradedLeg::Semantic {
                    reason: DegradationReason::SqlError,
                });
                cairn_core::contract::memory_store::SemanticSearchPage {
                    candidates: Vec::new(),
                    explain: None,
                }
            }
        };

        let kw_list = scored_from_keyword(&keyword.candidates);
        let sem_list = scored_from_semantic(&semantic.candidates);

        let (graph_candidates, graph_degraded) = self
            .run_graph_leg(args, &keyword.candidates, &semantic.candidates)
            .await;
        leg_degradations.extend(graph_degraded);
        let degraded_legs = leg_degradations;

        // Build 1-based rank lookup maps for the explain block. Constructed
        // here while `kw_list` / `sem_list` are in leg-order (rank order).
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

        // Embed the query for cosine re-rank. `do_search_semantic` already
        // embedded once internally; the v0.1 hybrid path accepts the second
        // embed for simplicity. Future optimization: thread the precomputed
        // vector through a `do_search_semantic_with_vector` shortcut.
        let query_vector = embed_query_blocking(&embedder, args.query.clone()).await?;

        // Build the dedup'd candidate id list bounded by `rerank_topk` —
        // RRF only re-ranks its own top-K, so fetching more vectors is waste.
        let combined_ids = combined_topk_ids(&kw_list, &sem_list, args.rerank_topk);
        let conn = self.require_conn("search_hybrid")?.clone();
        let doc_vectors = fetch_doc_vectors(conn, combined_ids, args.model_label.clone()).await?;

        // Run the pure-function orchestration.
        let reranked = hybrid_search(
            &HybridSearchInputs {
                keyword: kw_list,
                semantic: sem_list,
                graph: graph_candidates,
                query_vector,
                doc_vectors,
            },
            HybridSearchParams {
                rrf_k: args.rrf_k,
                rerank_topk: args.rerank_topk,
                blend: args.blend,
                skip_rerank: false,
                confidence_floor: args.confidence_floor,
            },
        );

        // Hydrate from lexical legs + graph-only `records` rows; apply
        // `limit` AFTER drop-out so a row tombstoned between the leg
        // query and now cannot shrink the page below `limit`.
        let mut by_id = hydrate_candidates(keyword.candidates, semantic.candidates);
        self.hydrate_graph_only_into(&reranked, &mut by_id, args)
            .await?;
        let candidates: Vec<SearchCandidate> = reranked
            .iter()
            .filter_map(|r| by_id.remove(&r.record_id))
            .take(args.limit)
            .collect();

        let explain = args
            .with_explain
            .then(|| build_explain(&candidates, &reranked, &kw_ranks, &sem_ranks));

        Ok(HybridSearchPage {
            candidates,
            explain,
            degraded_legs,
        })
    }

    /// Hydrate `SearchCandidate` rows for any reranked id that the lexical
    /// legs did not surface (graph-only hits). Pulled out to keep the
    /// caller under `clippy::too_many_lines`.
    async fn hydrate_graph_only_into(
        &self,
        reranked: &[RerankedCandidate],
        by_id: &mut HashMap<RecordId, SearchCandidate>,
        args: &HybridSearchArgs<'_>,
    ) -> Result<(), StoreError> {
        let missing_ids: Vec<RecordId> = reranked
            .iter()
            .filter(|r| !by_id.contains_key(&r.record_id))
            .map(|r| r.record_id.clone())
            .collect();
        if missing_ids.is_empty() {
            return Ok(());
        }
        let conn = self.require_conn("search_hybrid")?.clone();
        let visibilities: Vec<String> = args
            .visibility_allowlist
            .iter()
            .map(|v| v.as_str().to_owned())
            .collect();
        let scope = args.auth_scope.clone();
        let compiled = args.filter.map(cairn_core::domain::filter::compile_filter);
        let extra = hydrate_graph_only(conn, missing_ids, visibilities, scope, compiled).await?;
        for c in extra {
            by_id.entry(c.record_id.clone()).or_insert(c);
        }
        Ok(())
    }

    /// Run the graph leg for [`Self::do_search_hybrid`]. Returns
    /// `(graph_candidates, degraded_legs)`. Extracted to keep the parent
    /// function under the workspace `clippy::too_many_lines` cap.
    ///
    /// Seed pool = independent auth-only fetch (no `filter`) up to
    /// `GRAPH_SEED_OVERFETCH`, so neighbors of records outside the
    /// caller's narrowing filter remain reachable as long as they pass
    /// authorization. Ranked exclusion = the filtered top-of-leg ids, so
    /// the graph never re-promotes a record that already won its lexical
    /// slot. Capability-missing yields empty + `DegradedLeg::graph_…`,
    /// SQL errors yield empty + `DegradedLeg::Graph { SqlError }`.
    async fn run_graph_leg(
        &self,
        args: &HybridSearchArgs<'_>,
        kw_candidates: &[SearchCandidate],
        sem_candidates: &[SearchCandidate],
    ) -> (Vec<GraphCandidate>, Vec<DegradedLeg>) {
        if !self.caps.graph_search {
            return (
                Vec::new(),
                vec![DegradedLeg::graph_capability_unavailable()],
            );
        }

        // Auth-only seed pool. Same `auth_scope` + `visibility_allowlist`
        // + supersession + active/tombstoned predicates as the lexical
        // legs, but **without** `args.filter` — the user's narrowing
        // filter must not erase records from the seed pool, otherwise
        // graph rank-rescue collapses on aggressive filters.
        //
        // The two seed sources (keyword + semantic) run independently:
        // a single failure on one side still allows the surviving source
        // to seed the graph traversal, with a per-source `DegradedLeg`
        // entry recording the partial loss. Only when BOTH sides fail
        // does the leg short-circuit empty.
        let mut graph_degradations: Vec<DegradedLeg> = Vec::new();
        let seed_ids = match self.fetch_graph_seed_pool(args).await {
            SeedPoolOutcome::Both(ids) => ids,
            SeedPoolOutcome::Partial { ids, lost } => {
                tracing::warn!(source = ?lost, "graph seed source failed; continuing with surviving source");
                graph_degradations.push(DegradedLeg::Graph {
                    reason: DegradationReason::SqlError,
                    source: lost,
                });
                ids
            }
            SeedPoolOutcome::None => {
                tracing::warn!("graph seed pool failed (both sources); continuing without graph results");
                return (
                    Vec::new(),
                    vec![DegradedLeg::Graph {
                        reason: DegradationReason::SqlError,
                        source: GraphSource::All,
                    }],
                );
            }
        };

        // Ranked exclusion = filtered top-of-leg ids only. These are the
        // records already destined for the lexical RRF positions, so the
        // graph leg dedups them out and the remaining graph-only hits
        // are pure rank-rescue territory.
        let mut seen: HashSet<RecordId> = HashSet::new();
        let mut ranked_ids: Vec<RecordId> = Vec::new();
        for c in kw_candidates.iter().chain(sem_candidates.iter()) {
            if seen.insert(c.record_id.clone()) {
                ranked_ids.push(c.record_id.clone());
            }
        }

        let graph_args = GraphNeighborsArgs {
            seed_record_ids: seed_ids,
            ranked_record_ids: ranked_ids,
            filter: args.filter,
            auth_scope: args.auth_scope.clone(),
            visibility_allowlist: args.visibility_allowlist.clone(),
            limit: HYBRID_LEG_LIMIT,
            confidence_min: 0.0,
        };
        match self.do_search_graph_neighbors(&graph_args).await {
            Ok(c) => (c, graph_degradations),
            Err(e) => {
                tracing::warn!(error = %e, "graph leg failed; continuing without graph results");
                graph_degradations.push(DegradedLeg::Graph {
                    reason: DegradationReason::SqlError,
                    source: GraphSource::All,
                });
                (Vec::new(), graph_degradations)
            }
        }
    }

    /// Fetch the query-aware, auth-only seed pool for the graph leg.
    ///
    /// Runs the **same FTS5 + ANN retrievals as the lexical legs**, but
    /// with `filter = None` and `with_explain = false`. The result is the
    /// union of keyword and semantic ids that match `args.query` and
    /// pass `auth_scope` + `visibility_allowlist` + supersession — but
    /// NOT `args.filter`. This restores the spec's "authorized records
    /// that match the query but were narrowed out by the user filter"
    /// seed semantics, without admitting query-irrelevant rows into
    /// graph traversal (which would have happened with a query-agnostic
    /// `records` scan).
    async fn fetch_graph_seed_pool(&self, args: &HybridSearchArgs<'_>) -> SeedPoolOutcome {
        let kw_seed_args = KeywordSearchArgs {
            query: args.query.clone(),
            filter: None,
            auth_scope: args.auth_scope.clone(),
            visibility_allowlist: args.visibility_allowlist.clone(),
            limit: GRAPH_SEED_OVERFETCH,
            cursor: None,
            with_explain: false,
        };
        let sem_seed_args = SemanticSearchArgs {
            query: args.query.clone(),
            filter: None,
            auth_scope: args.auth_scope.clone(),
            visibility_allowlist: args.visibility_allowlist.clone(),
            limit: GRAPH_SEED_OVERFETCH,
            model_label: args.model_label.clone(),
            with_explain: false,
        };
        // Run the two seed queries in parallel and tolerate a single-side
        // failure. `tokio::join!` (not `try_join!`) returns both Results
        // independently; we union the surviving source(s) and report any
        // loss as a per-`GraphSource` degradation entry.
        let (kw_res, sem_res) = tokio::join!(
            self.do_search_keyword(&kw_seed_args),
            self.do_search_semantic(&sem_seed_args),
        );

        let kw_ok = match kw_res {
            Ok(r) => Some(r.candidates),
            Err(e) => {
                tracing::warn!(error = %e, "graph keyword-seed retrieval failed");
                None
            }
        };
        let sem_ok = match sem_res {
            Ok(r) => Some(r.candidates),
            Err(e) => {
                tracing::warn!(error = %e, "graph semantic-seed retrieval failed");
                None
            }
        };

        match (kw_ok, sem_ok) {
            (None, None) => SeedPoolOutcome::None,
            (Some(kw), Some(sem)) => SeedPoolOutcome::Both(union_seed_ids(kw, Some(sem))),
            (Some(kw), None) => SeedPoolOutcome::Partial {
                ids: union_seed_ids(kw, None),
                lost: GraphSource::AuthSemanticSeed,
            },
            (None, Some(sem)) => SeedPoolOutcome::Partial {
                ids: union_seed_ids(sem, None),
                lost: GraphSource::AuthKeywordSeed,
            },
        }
    }
}

/// Internal outcome of [`SqliteMemoryStore::fetch_graph_seed_pool`].
///
/// Carries the per-source degradation signal up to `run_graph_leg` so a
/// single-source failure surfaces as a recall reduction, not a full leg
/// loss.
enum SeedPoolOutcome {
    /// Both keyword + semantic seed retrievals succeeded.
    Both(Vec<RecordId>),
    /// One source survived; the other failed and is reported via `lost`.
    Partial {
        ids: Vec<RecordId>,
        lost: GraphSource,
    },
    /// Both sources failed.
    None,
}

/// De-duplicating union of two ordered candidate lists, capped at
/// [`GRAPH_SEED_OVERFETCH`]. The first list is preferred for ordering
/// (its hits enter the pool first), then the second list contributes any
/// records the first did not surface.
fn union_seed_ids(
    primary: Vec<SearchCandidate>,
    secondary: Option<Vec<SearchCandidate>>,
) -> Vec<RecordId> {
    let mut seen: HashSet<RecordId> = HashSet::new();
    let mut out: Vec<RecordId> = Vec::with_capacity(GRAPH_SEED_OVERFETCH);
    let secondary_iter = secondary.unwrap_or_default().into_iter();
    for c in primary.into_iter().chain(secondary_iter) {
        if out.len() >= GRAPH_SEED_OVERFETCH {
            break;
        }
        if seen.insert(c.record_id.clone()) {
            out.push(c.record_id);
        }
    }
    out
}

/// Project keyword candidates into the rank-only `ScoredCandidate` list RRF
/// consumes. `do_search_keyword` returns rows already ordered by ascending
/// bm25 (FTS5: smaller = better), which is the rank order RRF wants.
fn scored_from_keyword(candidates: &[SearchCandidate]) -> Vec<ScoredCandidate> {
    candidates
        .iter()
        .map(|c| ScoredCandidate {
            record_id: c.record_id.clone(),
            score: c.bm25,
        })
        .collect()
}

/// Project semantic candidates into the rank-only `ScoredCandidate` list RRF
/// consumes. `do_search_semantic` returns rows ordered by ascending L2
/// distance; we record `-distance` so any descending tie-break uses the
/// same convention as the keyword leg, but RRF itself only reads positions.
fn scored_from_semantic(candidates: &[SearchCandidate]) -> Vec<ScoredCandidate> {
    candidates
        .iter()
        .map(|c| ScoredCandidate {
            record_id: c.record_id.clone(),
            score: f64::from(-c.semantic_distance.unwrap_or(0.0)),
        })
        .collect()
}

/// Embed `query` on the blocking pool. Hybrid path embeds once for the
/// cosine re-rank, distinct from the embed `do_search_semantic` already
/// performed for the ANN leg — see the docstring on `do_search_hybrid`.
async fn embed_query_blocking(
    embedder: &Arc<dyn EmbeddingModel>,
    query: String,
) -> Result<Vec<f32>, StoreError> {
    let embedder = Arc::clone(embedder);
    tokio::task::spawn_blocking(move || embedder.embed_query(&query))
        .await
        .map_err(|e| StoreError::Invariant {
            what: format!("hybrid embedding task panicked: {e}"),
        })?
        .map_err(|e| StoreError::Invariant {
            what: format!("hybrid embed_query failed: {e}"),
        })
}

/// Build the union of candidate ids across both legs, deduplicated while
/// preserving first-seen order, capped at `rerank_topk`.
fn combined_topk_ids(
    kw: &[ScoredCandidate],
    sem: &[ScoredCandidate],
    rerank_topk: usize,
) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    kw.iter()
        .chain(sem.iter())
        .map(|c| c.record_id.as_str().to_owned())
        .filter(|s| seen.insert(s.clone()))
        .take(rerank_topk)
        .collect()
}

/// Single batch SELECT against `record_vectors` for top-K vectors. Not a
/// KNN query (no MATCH clause), so the auxiliary `model` column is allowed
/// in WHERE — vec0 only blocks aux predicates on MATCH-driven queries.
async fn fetch_doc_vectors(
    conn: Arc<tokio_rusqlite::Connection>,
    ids: Vec<String>,
    model_label: String,
) -> Result<HashMap<RecordId, Vec<f32>>, StoreError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let raw: Vec<(String, Vec<f32>)> = conn
        .call(move |c| {
            let placeholders = vec!["?"; ids.len()].join(",");
            let sql = format!(
                "SELECT record_id, embedding \
                   FROM record_vectors \
                  WHERE record_id IN ({placeholders}) AND model = ?"
            );
            let mut params: Vec<SqlVal> = ids.iter().map(|s| SqlVal::Text(s.clone())).collect();
            params.push(SqlVal::Text(model_label));
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                    let id: String = r.get(0)?;
                    let blob: Vec<u8> = r.get(1)?;
                    Ok((id, blob_to_f32_vec(&blob)))
                })?
                .collect::<Result<Vec<_>, rusqlite::Error>>()?;
            Ok::<_, tokio_rusqlite::Error>(rows)
        })
        .await?;
    let mut out: HashMap<RecordId, Vec<f32>> = HashMap::with_capacity(raw.len());
    for (id_str, v) in raw {
        if let Ok(rid) = RecordId::parse(id_str) {
            out.insert(rid, v);
        }
    }
    Ok(out)
}

/// Merge keyword and semantic candidate lists keyed by record id. Where both
/// legs returned a record, prefer the keyword row (which has snippet + bm25)
/// but propagate the `semantic_distance` from the semantic leg.
fn hydrate_candidates(
    keyword: Vec<SearchCandidate>,
    semantic: Vec<SearchCandidate>,
) -> HashMap<RecordId, SearchCandidate> {
    let mut by_id: HashMap<RecordId, SearchCandidate> = HashMap::new();
    for c in keyword {
        by_id.entry(c.record_id.clone()).or_insert(c);
    }
    for c in semantic {
        by_id
            .entry(c.record_id.clone())
            .and_modify(|existing| {
                if existing.semantic_distance.is_none() {
                    existing.semantic_distance = c.semantic_distance;
                }
            })
            .or_insert(c);
    }
    by_id
}

/// Build the explain block aligned with the surviving candidate page.
/// Iterates candidates (post hydrate + `filter_map` + take) and looks up each
/// row in the rerank output so `explain[i].record_id == candidates[i].record_id`
/// even when hydration drops a row.
fn build_explain(
    candidates: &[SearchCandidate],
    reranked: &[RerankedCandidate],
    kw_ranks: &HashMap<RecordId, usize>,
    sem_ranks: &HashMap<RecordId, usize>,
) -> Vec<ScoreExplain> {
    let reranked_map: HashMap<&RecordId, &RerankedCandidate> =
        reranked.iter().map(|r| (&r.record_id, r)).collect();
    candidates
        .iter()
        .filter_map(|c| {
            let r = reranked_map.get(&c.record_id)?;
            Some(ScoreExplain {
                record_id: c.record_id.clone(),
                bm25_rank: kw_ranks.get(&c.record_id).copied(),
                semantic_rank: sem_ranks.get(&c.record_id).copied(),
                rrf_score: r.rrf_score,
                cosine: r.cosine,
                final_score: r.final_score,
            })
        })
        .collect()
}

/// Hydrate `SearchCandidate` rows for graph-only ids that did not surface
/// in either lexical leg. Reads the same `records` columns the keyword
/// query projects, but with `bm25 = 0.0`, empty `snippet`, and
/// `semantic_distance = None` — those signals are unavailable on the
/// graph-only path. Tombstoned / inactive rows are filtered out so a
/// record retired between the leg query and now cannot resurface as a
/// graph hit.
async fn hydrate_graph_only(
    conn: Arc<tokio_rusqlite::Connection>,
    ids: Vec<RecordId>,
    visibilities: Vec<String>,
    scope: cairn_core::domain::ScopeTuple,
    compiled: Option<cairn_core::domain::filter::CompiledFilter>,
) -> Result<Vec<SearchCandidate>, StoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let now_ms = crate::store::current_unix_ms();
    let id_strings: Vec<String> = ids.iter().map(|r| r.as_str().to_owned()).collect();

    let out = conn
        .call(
            move |c| -> Result<Vec<SearchCandidate>, tokio_rusqlite::Error> {
                let (sql, params) =
                    build_graph_only_query(&id_strings, &visibilities, &scope, compiled.as_ref());
                let mut stmt = c.prepare(&sql)?;
                let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
                let mut out: Vec<SearchCandidate> = Vec::new();
                while let Some(row) = rows.next()? {
                    out.push(map_graph_only_row(row, now_ms)?);
                }
                Ok(out)
            },
        )
        .await
        .map_err(StoreError::from)?;
    Ok(out)
}

/// Build SQL + bound params for the graph-only hydration query. Reapplies
/// the same authorization (`auth_scope`), narrowing-filter, supersession,
/// and visibility predicates as the lexical legs so any bug or race in
/// `graph_search.rs` cannot leak rows that fail those checks back into
/// the page.
fn build_graph_only_query(
    id_strings: &[String],
    visibilities: &[String],
    scope: &cairn_core::domain::ScopeTuple,
    compiled: Option<&cairn_core::domain::filter::CompiledFilter>,
) -> (String, Vec<SqlVal>) {
    let id_placeholders: String = std::iter::repeat_n("?", id_strings.len())
        .collect::<Vec<_>>()
        .join(",");
    let visibility_clause = if visibilities.is_empty() {
        String::new()
    } else {
        let vis_placeholders: String = std::iter::repeat_n("?", visibilities.len())
            .collect::<Vec<_>>()
            .join(",");
        format!(" AND r.visibility IN ({vis_placeholders})")
    };
    let (scope_sql, scope_params) =
        crate::store::scope_predicate::build_scope_predicate("r", scope);
    let filter_clause = compiled
        .as_ref()
        .map(|cf| format!(" AND ({})", cf.sql))
        .unwrap_or_default();
    let sql = format!(
        "SELECT r.record_id, r.target_id, r.scope, r.kind, r.class, r.visibility, \
                r.updated_at, r.confidence, r.salience, r.created_at, r.body \
           FROM records r \
          WHERE r.record_id IN ({id_placeholders}) \
            AND r.active = 1 AND r.tombstoned = 0{visibility_clause}{scope_sql}{filter_clause} \
            AND {}",
        crate::store::search::SUPERSESSION_NOT_EXISTS_CLAUSE
    );
    let mut params: Vec<SqlVal> = id_strings.iter().map(|s| SqlVal::Text(s.clone())).collect();
    for v in visibilities {
        params.push(SqlVal::Text(v.clone()));
    }
    params.extend(scope_params);
    if let Some(cf) = compiled {
        for p in &cf.params {
            params.push(crate::store::search::json_to_sql(p));
        }
    }
    (sql, params)
}

/// Project one `records` row into a `SearchCandidate` for graph-only
/// hydration. `bm25 = 0`, `snippet = ""`, `semantic_distance = None`
/// because those signals come from the lexical legs and are unavailable
/// on the graph-only path.
fn map_graph_only_row(
    row: &rusqlite::Row<'_>,
    now_ms: i64,
) -> Result<SearchCandidate, tokio_rusqlite::Error> {
    use cairn_core::domain::{MemoryClass, MemoryKind, MemoryVisibility, ScopeTuple, TargetId};

    use crate::store::projection::record_id_from_str;

    let invariant =
        |what: String| tokio_rusqlite::Error::Other(Box::new(StoreError::Invariant { what }));

    let rec_str: String = row.get(0)?;
    let target_str: String = row.get(1)?;
    let scope_json: String = row.get(2)?;
    let kind_str: String = row.get(3)?;
    let class_str: String = row.get(4)?;
    let visibility_str: String = row.get(5)?;
    let updated_at: i64 = row.get(6)?;
    let confidence: f64 = row.get(7)?;
    let salience: f64 = row.get(8)?;
    let created_at: i64 = row.get(9)?;
    let body: String = row.get(10)?;

    let record_id = record_id_from_str(&rec_str).map_err(|e| {
        invariant(format!(
            "hydrate_graph_only: bad record_id `{rec_str}`: {e}"
        ))
    })?;
    let target_id = TargetId::parse(&target_str).map_err(|e| {
        invariant(format!(
            "hydrate_graph_only: bad target_id `{target_str}`: {e}"
        ))
    })?;
    let scope: ScopeTuple = serde_json::from_str(&scope_json)
        .map_err(|e| invariant(format!("hydrate_graph_only: bad scope `{scope_json}`: {e}")))?;
    let kind = MemoryKind::parse(&kind_str)
        .map_err(|e| invariant(format!("hydrate_graph_only: bad kind `{kind_str}`: {e}")))?;
    let class = MemoryClass::parse(&class_str)
        .map_err(|e| invariant(format!("hydrate_graph_only: bad class `{class_str}`: {e}")))?;
    let visibility = MemoryVisibility::parse(&visibility_str).map_err(|e| {
        invariant(format!(
            "hydrate_graph_only: bad visibility `{visibility_str}`: {e}"
        ))
    })?;

    #[allow(clippy::cast_possible_truncation, reason = "REAL→f32 narrow")]
    Ok(SearchCandidate {
        record_id,
        target_id,
        scope,
        kind,
        class,
        visibility,
        bm25: 0.0,
        recency_seconds: (now_ms - updated_at) / 1000,
        confidence: confidence as f32,
        salience: salience as f32,
        staleness_seconds: (now_ms - created_at) / 1000,
        snippet: String::new(),
        record_json: body,
        semantic_distance: None,
    })
}

/// Decode a sqlite-vec blob (LE f32 sequence) into a `Vec<f32>`. Tail bytes
/// that don't form a complete f32 are silently dropped — the writer side in
/// `do_upsert` always emits exact 4-byte chunks, so any leftover here is a
/// schema-drift signal worth logging at trace once embed-on-read lands.
fn blob_to_f32_vec(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip an f32 sequence through `blob_to_f32_vec`. Spot-checks
    /// the LE decoding without standing up the full store.
    #[test]
    fn blob_to_f32_vec_roundtrips() {
        let v: Vec<f32> = vec![0.0, 1.0, -1.0, 1.5, -2.5];
        let blob: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        let back = blob_to_f32_vec(&blob);
        assert_eq!(back, v);
    }

    /// A blob whose length is not a multiple of 4 should drop the tail
    /// bytes; the prefix decodes normally.
    #[test]
    fn blob_to_f32_vec_drops_partial_tail() {
        let v: Vec<f32> = vec![1.0, 2.0];
        let mut blob: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        blob.extend_from_slice(&[0xAB, 0xCD, 0xEF]); // 3 trailing bytes
        let back = blob_to_f32_vec(&blob);
        assert_eq!(back, v);
    }
}
