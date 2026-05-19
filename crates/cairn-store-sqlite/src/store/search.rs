//! `MemoryStore::search_keyword` impl (brief §5.1, §8.0.d, issue #47).
//!
//! Pipeline:
//!
//! 1. The FTS5 virtual table `records_fts` indexes `body` (migration 0001,
//!    pinned in brief §3 lines 380-385). Path/title narrowing happens via
//!    the metadata filter, not by widening the FTS index.
//!    A subquery `MATCH ?` produces `(rowid, bm25, snippet)` for hits.
//! 2. The outer query joins back to `records r`, drops tombstoned and
//!    inactive rows, narrows by the visibility allowlist, and AND-combines
//!    the [`compile_filter`]-produced SQL fragment.
//! 3. Ranking inputs (`bm25`, `recency_seconds`, `confidence`, `salience`,
//!    `staleness_seconds`) are returned alongside `record_json` so the
//!    pure-function ranker in `cairn-core` can re-score without a second
//!    round-trip.
//! 4. Keyset pagination uses `(bm25, record_id)` row-value comparison —
//!    SQLite-stable since 3.15. The store over-fetches by one to detect
//!    end-of-stream.
//!
//! FTS5 query parse failures bubble up from `SQLite` as a generic
//! [`rusqlite::Error::SqliteFailure`] whose message starts with `"fts5:"`.
//! The store rewraps these as [`StoreError::FtsQuery`] so the verb layer
//! can return a user-actionable error instead of a generic SQL failure.
//!
//! ## Scope is the caller's responsibility
//!
//! This module does NOT enforce scope-tuple narrowing — see the trait
//! docstring on [`cairn_core::contract::memory_store::MemoryStore::search_keyword`].
//! Callers running against a shared multi-tenant DB MUST fold scope
//! resolution into either the `visibility_allowlist` or the validated
//! `filter` before invoking; otherwise rows from another scope can match
//! the keyword query and leak through.
//!
//! [`compile_filter`]: cairn_core::domain::filter::compile_filter

use cairn_core::contract::memory_store::{
    KeywordCursor, KeywordSearchArgs, KeywordSearchPage, SearchCandidate, SemanticSearchArgs,
    SemanticSearchPage,
};
use cairn_core::domain::filter::compile_filter;
use cairn_core::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
use cairn_core::domain::{RecordId, ScopeTuple};
use cairn_core::search::ScoreExplain;
use rusqlite::types::Value as SqlVal;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::projection::{record_id_from_str, target_id_from_str};
use crate::store::{SqliteMemoryStore, current_unix_ms};

/// Hard upper bound on a single search page. Matches `do_list` so callers
/// see consistent paging semantics across read verbs.
const SEARCH_LIMIT_MAX: usize = 1000;

/// Records-latest supersession predicate, written so `SQLite`'s partial-index
/// proof rule can match it against migration 0013's
/// `INDEX edges_updates_dst_idx ON edges(dst) WHERE kind = 'updates'`.
///
/// Why this exact form:
///
/// `SQLite` uses a partial index for a query only when it can prove the
/// query's `WHERE` clause implies the index's `WHERE` clause. The proof
/// engine handles literal-equality conjuncts (`e.kind = 'updates'`) but
/// does *not* recognise equivalent rewrites such as `e.kind IN ('updates')`
/// or a parameterised `e.kind = ?`. Either rewrite would silently
/// regress the supersession check to a full `edges` scan per FTS hit
/// (brief §5.1).
///
/// The string is shared verbatim with the EXPLAIN-QUERY-PLAN regression
/// test below so any change here must continue to plan as a `SEARCH`
/// against the partial index.
pub(crate) const SUPERSESSION_NOT_EXISTS_CLAUSE: &str = "NOT EXISTS ( SELECT 1 FROM edges e \
                  WHERE e.kind = 'updates' AND e.dst = r.record_id )";

/// Tokens shown in the FTS5 `snippet()` highlight. Hard-coded so the wire
/// shape is stable across callers — the verb layer can re-render if it
/// needs different markers.
const SNIPPET_OPEN: &str = "<mark>";
const SNIPPET_CLOSE: &str = "</mark>";
const SNIPPET_ELLIPSIS: &str = "…";
const SNIPPET_MAX_TOKENS: i32 = 16;

/// Column index, inside `records_fts`, that the `snippet()` SQL function
/// should highlight. Migration 0030 reshaped `records_fts` from
/// `(body)` to `(kind, class, scope, body)`; column index 3 is `body`,
/// the only column carrying long-prose text worth showing as a snippet.
const SNIPPET_COLUMN: i32 = 3;

/// Validate that every per-column BM25 weight is finite and non-negative.
/// FTS5 accepts arbitrary `REAL`s into `bm25(...)`, but a NaN or negative
/// weight collapses ranking into garbage; reject early with a typed
/// invariant error so the misconfiguration surfaces at the verb layer
/// instead of poisoning the result page.
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

impl SqliteMemoryStore {
    /// Inherent `search_keyword` implementation; the trait method
    /// [`MemoryStore::search_keyword`] guards `self.conn` then delegates here.
    ///
    /// [`MemoryStore::search_keyword`]: cairn_core::contract::memory_store::MemoryStore::search_keyword
    ///
    /// # Errors
    ///
    /// - [`StoreError::FtsQuery`] when the FTS5 engine rejects the query
    ///   string (e.g. unmatched quotes, unknown operator).
    /// - [`StoreError::Sqlite`] / [`StoreError::Worker`] for SQL or worker
    ///   failures.
    /// - [`StoreError::Codec`] when projecting a row to a typed enum fails.
    /// - [`StoreError::Invariant`] when a stored id, version, or scope
    ///   cannot be parsed (corruption / schema-drift signal).
    #[instrument(
        skip(self, args),
        err,
        fields(verb = "search_keyword", limit = args.limit, has_filter = args.filter.is_some()),
    )]
    pub(crate) async fn do_search_keyword(
        &self,
        args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError> {
        let page = self.do_search_keyword_untracked(args).await?;
        self.record_search_access(&page.candidates, "keyword search")
            .await;
        Ok(page)
    }

    pub(crate) async fn do_search_keyword_untracked(
        &self,
        args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError> {
        // Validate weights up-front so a misconfigured store fails before
        // we burn a worker round-trip. The check is cheap (4 floats) and
        // surfaces the bug as `Invariant` rather than as an FTS5 runtime
        // error from inside the worker callback.
        assert_finite_weights(&self.fts_column_weights)?;
        let weights = self.fts_column_weights;
        let conn = self.require_conn("search_keyword")?.clone();
        let limit = args.limit.clamp(1, SEARCH_LIMIT_MAX);
        let query = args.query.clone();
        let visibilities: Vec<String> = args
            .visibility_allowlist
            .iter()
            .map(|v| v.as_str().to_owned())
            .collect();
        let cursor = args.cursor.clone();
        let compiled = args.filter.map(compile_filter);
        let now_ms = current_unix_ms();
        let with_explain = args.with_explain;
        let scope = args.auth_scope.clone();

        let page = conn
            .call(move |c| {
                let (sql, params) = build_search_query(
                    &query,
                    &visibilities,
                    compiled.as_ref(),
                    cursor.as_ref(),
                    limit,
                    &weights,
                    &scope,
                )
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;

                // `prepare` runs SQL parsing + name resolution against the
                // outer query (records, edges, generated columns, filter
                // SQL). An error here is a schema/SQL bug in *our* string,
                // not an FTS5 user-syntax issue, so we don't widen
                // classification to runtime-only message shapes.
                let mut stmt = c
                    .prepare(&sql)
                    .map_err(|e| classify_fts_error(e, FtsErrorStage::Prepare).into_tokio())?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                        Ok(RawRow {
                            record_id: row.get::<_, String>(0)?,
                            target_id: row.get::<_, String>(1)?,
                            scope_json: row.get::<_, String>(2)?,
                            kind: row.get::<_, String>(3)?,
                            class: row.get::<_, String>(4)?,
                            visibility: row.get::<_, String>(5)?,
                            bm25: row.get::<_, f64>(6)?,
                            updated_at_ms: row.get::<_, i64>(7)?,
                            confidence: row.get::<_, f64>(8)?,
                            salience: row.get::<_, f64>(9)?,
                            created_at_ms: row.get::<_, i64>(10)?,
                            snippet: row.get::<_, String>(11)?,
                            record_json: row.get::<_, String>(12)?,
                        })
                    })
                    .map_err(|e| classify_fts_error(e, FtsErrorStage::Runtime).into_tokio())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| classify_fts_error(e, FtsErrorStage::Runtime).into_tokio())?;

                project_page(rows, limit, now_ms, with_explain)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
            })
            .await
            .map_err(unpack_worker_err)?;
        Ok(page)
    }

    pub(crate) async fn record_search_access(
        &self,
        candidates: &[SearchCandidate],
        failure_context: &'static str,
    ) {
        let record_ids: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.record_id.clone())
            .collect();
        if !record_ids.is_empty() {
            let accessed_at_ms = current_unix_ms();
            if let Err(error) = self
                .do_record_access(&record_ids, accessed_at_ms, "search")
                .await
            {
                tracing::warn!(
                    error = %error,
                    records = record_ids.len(),
                    "workflow.access_tracker failed after {failure_context}"
                );
            }
        }
    }
}

/// Convert a `tokio_rusqlite::Error` back into [`StoreError`], preserving
/// the typed inner variant when the worker callback wrapped one in
/// `Other(Box<StoreError>)`. Without this, the blanket `From` impl would
/// re-wrap every error as [`StoreError::Worker`] and erase the
/// [`StoreError::FtsQuery`] / [`StoreError::Invariant`] / [`StoreError::Codec`]
/// context the search worker carefully attached.
fn unpack_worker_err(err: tokio_rusqlite::Error) -> StoreError {
    match err {
        tokio_rusqlite::Error::Other(boxed) => match boxed.downcast::<StoreError>() {
            Ok(inner) => *inner,
            Err(other) => StoreError::Worker(tokio_rusqlite::Error::Other(other)),
        },
        other => StoreError::from(other),
    }
}

/// Single row tuple read out of the FTS5 + records join. Pulled into a
/// named struct so [`project_page`] takes one parameter and the async
/// shell stays under the workspace's `clippy::too_many_lines` limit.
struct RawRow {
    record_id: String,
    target_id: String,
    scope_json: String,
    kind: String,
    class: String,
    visibility: String,
    bm25: f64,
    updated_at_ms: i64,
    confidence: f64,
    salience: f64,
    created_at_ms: i64,
    snippet: String,
    record_json: String,
}

/// Convert raw rows into the typed [`KeywordSearchPage`], computing the
/// `recency`/`staleness` deltas against `now_ms` and surfacing the
/// cursor for the next page if the over-fetch detected more rows.
///
/// When `with_explain` is true the page's `explain` block is populated with
/// one [`ScoreExplain`] per candidate, using 1-based `bm25_rank` and the
/// candidate's raw BM25 score as `final_score`.
fn project_page(
    rows: Vec<RawRow>,
    limit: usize,
    now_ms: i64,
    with_explain: bool,
) -> Result<KeywordSearchPage, StoreError> {
    let has_more = rows.len() > limit;
    let mut candidates = Vec::with_capacity(rows.len().min(limit));
    let mut last: Option<(f64, RecordId)> = None;
    for (i, row) in rows.into_iter().enumerate() {
        if i >= limit {
            break;
        }
        let candidate = project_row(&row, now_ms)?;
        last = Some((candidate.bm25, candidate.record_id.clone()));
        candidates.push(candidate);
    }
    let next_cursor = if has_more {
        last.map(|(bm25, record_id)| KeywordCursor { bm25, record_id })
    } else {
        None
    };
    let explain = if with_explain {
        Some(
            candidates
                .iter()
                .enumerate()
                .map(|(i, c)| ScoreExplain {
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
    Ok(KeywordSearchPage {
        candidates,
        next_cursor,
        explain,
    })
}

/// Project one raw row into a typed [`SearchCandidate`].
///
/// Recency and staleness derive from the same `now_ms` captured at query
/// dispatch — this keeps the two deltas mutually consistent within one
/// page even if the wall clock shifts between rows.
fn project_row(row: &RawRow, now_ms: i64) -> Result<SearchCandidate, StoreError> {
    let record_id = record_id_from_str(&row.record_id)?;
    let target_id = target_id_from_str(&row.target_id)?;
    let scope: ScopeTuple = serde_json::from_str(&row.scope_json)?;
    let kind = MemoryKind::parse(&row.kind).map_err(|e| StoreError::Invariant {
        what: format!("invalid kind `{}`: {e}", row.kind),
    })?;
    let class = MemoryClass::parse(&row.class).map_err(|e| StoreError::Invariant {
        what: format!("invalid class `{}`: {e}", row.class),
    })?;
    let visibility =
        MemoryVisibility::parse(&row.visibility).map_err(|e| StoreError::Invariant {
            what: format!("invalid visibility `{}`: {e}", row.visibility),
        })?;
    let recency_seconds = delta_seconds(now_ms, row.updated_at_ms);
    let staleness_seconds = delta_seconds(now_ms, row.created_at_ms);
    Ok(SearchCandidate {
        record_id,
        target_id,
        scope,
        kind,
        class,
        visibility,
        bm25: row.bm25,
        recency_seconds,
        // `confidence` / `salience` round-trip f32 → f64 → f32 because the
        // `records.confidence/salience` columns are SQLite REAL (f64); the
        // typed `MemoryRecord` carries f32. The narrowing is explicit and
        // documented at the trait surface.
        #[allow(clippy::cast_possible_truncation, reason = "REAL→f32 narrow")]
        confidence: row.confidence as f32,
        #[allow(clippy::cast_possible_truncation, reason = "REAL→f32 narrow")]
        salience: row.salience as f32,
        staleness_seconds,
        snippet: row.snippet.clone(),
        record_json: row.record_json.clone(),
        semantic_distance: None,
    })
}

/// Difference between `now_ms` and a stored epoch-ms value, expressed in
/// seconds. Negative values are clamped to zero so a clock-skew misread
/// cannot flow into the ranker as "future" recency.
fn delta_seconds(now_ms: i64, then_ms: i64) -> i64 {
    let ms = now_ms.saturating_sub(then_ms);
    if ms <= 0 { 0 } else { ms / 1000 }
}

/// Compose the search SQL string + bound parameter list for a single page.
///
/// Inner subquery: scan `records_fts MATCH ?` only — the FTS engine
/// returns `(rowid, bm25, snippet)`. Outer query joins to `records r` on
/// rowid, applies the freshness/visibility/filter constraints, applies
/// the optional keyset cursor, and over-fetches by one row so the caller
/// can detect end-of-stream via `rows.len() > limit`.
fn build_search_query(
    query: &str,
    visibilities: &[String],
    compiled: Option<&cairn_core::domain::filter::CompiledFilter>,
    cursor: Option<&KeywordCursor>,
    limit: usize,
    fts_column_weights: &[f64; 4],
    scope: &cairn_core::domain::ScopeTuple,
) -> Result<(String, Vec<SqlVal>), StoreError> {
    use std::fmt::Write as _;

    let mut params: Vec<SqlVal> = Vec::new();
    let mut sql = String::with_capacity(512);

    // Format the per-column BM25 call from the (already-validated) weights.
    // Values come from store-construction-time config, never from user
    // input, so direct interpolation is safe — `assert_finite_weights`
    // gated NaN/Inf/negative values, and `{:.6}` produces a stable decimal.
    let [w0, w1, w2, w3] = *fts_column_weights;
    let bm25_call = format!("bm25(records_fts, {w0:.6}, {w1:.6}, {w2:.6}, {w3:.6})");

    // The supersession predicate matches the `records_latest` view in
    // migration 0001 (brief §3 lines ~417-426): a record version is
    // "latest" only when no `updates` edge points to it. ConflictDAG /
    // PromotionWorkflow emit `updates` edges to retire stale facts, and
    // keyword search must respect the same exclusion as graph traversal —
    // otherwise a body match against an `updates`.dst can resurface a
    // fact the consolidator already retired.
    //
    // `snippet(records_fts, 3, ...)` targets column 3 (`body`) — migration
    // 0030 made column 0 = `kind`, which is single-token and useless to
    // highlight. Snippets continue to come from rich-prose body text.
    sql.push_str(
        "SELECT \
            r.record_id, r.target_id, r.scope, r.kind, r.class, r.visibility, \
            fts.bm25_score, r.updated_at, r.confidence, r.salience, r.created_at, \
            fts.snippet, r.record_json \
         FROM records r \
         JOIN ( \
            SELECT rowid, \
                   ",
    );
    sql.push_str(&bm25_call);
    sql.push_str(
        " AS bm25_score, \
                   snippet(records_fts, ",
    );
    write!(&mut sql, "{SNIPPET_COLUMN}").map_err(|e| StoreError::Invariant {
        what: format!("format snippet column index: {e}"),
    })?;
    sql.push_str(
        ", ?, ?, ?, ?) AS snippet \
              FROM records_fts \
             WHERE records_fts MATCH ? \
         ) fts ON fts.rowid = r.rowid \
         WHERE r.active = 1 \
           AND r.tombstoned = 0 \
           AND r.cow_staged = 0 \
           AND ",
    );
    sql.push_str(SUPERSESSION_NOT_EXISTS_CLAUSE);
    params.push(SqlVal::Text(SNIPPET_OPEN.to_owned()));
    params.push(SqlVal::Text(SNIPPET_CLOSE.to_owned()));
    params.push(SqlVal::Text(SNIPPET_ELLIPSIS.to_owned()));
    params.push(SqlVal::Integer(i64::from(SNIPPET_MAX_TOKENS)));
    params.push(SqlVal::Text(query.to_owned()));

    if !visibilities.is_empty() {
        sql.push_str(" AND r.visibility IN (");
        sql.push_str(&vec!["?"; visibilities.len()].join(","));
        sql.push(')');
        for v in visibilities {
            params.push(SqlVal::Text(v.clone()));
        }
    }

    let (scope_sql, scope_params) =
        crate::store::scope_predicate::build_scope_predicate("r", scope);
    if !scope_sql.is_empty() {
        sql.push_str(&scope_sql);
        params.extend(scope_params);
    }

    if let Some(filter) = compiled {
        sql.push_str(" AND (");
        sql.push_str(&filter.sql);
        sql.push(')');
        for p in &filter.params {
            params.push(json_to_sql(p));
        }
    }

    if let Some(cur) = cursor {
        sql.push_str(" AND (fts.bm25_score, r.record_id) > (?, ?)");
        params.push(SqlVal::Real(cur.bm25));
        params.push(SqlVal::Text(cur.record_id.as_str().to_owned()));
    }

    sql.push_str(" ORDER BY fts.bm25_score ASC, r.record_id ASC LIMIT ?");
    let plus_one = limit.checked_add(1).ok_or_else(|| StoreError::Invariant {
        what: format!("search limit + 1 overflows usize: {limit}"),
    })?;
    let bound = i64::try_from(plus_one).map_err(|_| StoreError::Invariant {
        what: format!("search limit + 1 overflows i64: {plus_one}"),
    })?;
    params.push(SqlVal::Integer(bound));
    Ok((sql, params))
}

/// Convert a `serde_json::Value` produced by `compile_filter` into the
/// `rusqlite::types::Value` accepted by `params_from_iter`. Unsupported
/// JSON shapes (objects, nested arrays) are not produced by the filter
/// compiler; if one ever appears we surface NULL so the predicate fails
/// closed instead of bypassing the filter.
pub(crate) fn json_to_sql(v: &serde_json::Value) -> SqlVal {
    match v {
        serde_json::Value::String(s) => SqlVal::Text(s.clone()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlVal::Integer(i)
            } else {
                SqlVal::Real(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::Bool(b) => SqlVal::Integer(i64::from(*b)),
        _ => SqlVal::Null,
    }
}

/// Pipeline stage at which a `SQLite` error surfaced. The classifier
/// widens its FTS5 prefix set at runtime because the outer query and
/// filter SQL are statically authored — at runtime the only source of
/// `"no such column: ..."` is the FTS5 module parsing the user-supplied
/// MATCH operand (e.g. `title:foo` against a body-only FTS table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FtsErrorStage {
    /// `prepare` returned an error. SQL parse / name resolution against
    /// the outer query — not user-driven, so don't classify as FTS.
    Prepare,
    /// `query_map` / row iteration. The MATCH operand is evaluated here,
    /// so column-filter parse errors flow through as runtime errors.
    Runtime,
}

/// Convert a `rusqlite::Error` into [`StoreError::FtsQuery`] when the
/// underlying `SQLite` message identifies it as an FTS5 parse failure.
///
/// FTS5 emits four message shapes for malformed `MATCH` operands
/// (observed against the bundled `SQLite` 3.46 build):
/// 1. `"fts5: syntax error near \"...\""`
/// 2. `"unknown special query: ..."`
/// 3. `"unterminated string"`
/// 4. `"no such column: <name>"` — column-filter syntax (`title:foo`)
///    against a column not indexed by the FTS table. Runtime-only.
///
/// All four are wire-stable across `SQLite` versions used in P0; the
/// classifier matches on each prefix so the verb layer can return a
/// user-actionable error instead of a generic SQL failure.
fn classify_fts_error(err: rusqlite::Error, stage: FtsErrorStage) -> StoreError {
    let message = match &err {
        rusqlite::Error::SqliteFailure(_, Some(m)) => m.clone(),
        other => other.to_string(),
    };
    if is_fts_message(&message, stage) {
        StoreError::FtsQuery { message }
    } else {
        StoreError::from(err)
    }
}

fn is_fts_message(msg: &str, stage: FtsErrorStage) -> bool {
    let lower_starts_with = |needle: &str| {
        msg.len() >= needle.len() && msg[..needle.len()].eq_ignore_ascii_case(needle)
    };
    if lower_starts_with("fts5:")
        || lower_starts_with("unknown special query")
        || lower_starts_with("unterminated string")
    {
        return true;
    }
    // `no such column` is ambiguous between FTS column-filter syntax and
    // a real outer-query schema bug. We only recognize it at runtime —
    // the static outer SQL would fail at prepare for a real schema bug.
    stage == FtsErrorStage::Runtime && lower_starts_with("no such column")
}

impl SqliteMemoryStore {
    /// Semantic ANN search over the `record_vectors` sqlite-vec table (brief §5.1, §8.0.d).
    ///
    /// Pipeline:
    ///
    /// 1. Capability gate — returns [`StoreError::CapabilityUnavailable`] when
    ///    `caps.vector` is false or no embedder is wired.
    /// 2. Embed the query string via `embedder.embed_query` on a blocking thread.
    /// 3. Convert the embedding to a little-endian byte blob and pass it to
    ///    the sqlite-vec `MATCH` clause. `k = limit * 2` over-fetches to buffer
    ///    against post-filter row drops.
    /// 4. Join back to `records` for hot columns; filter by
    ///    `active = 1 AND tombstoned = 0`, plus the caller's visibility allowlist
    ///    and optional [`compile_filter`]-produced SQL.
    /// 5. Apply model filter in Rust (see [`run_ann_query`] for why SQL is not used).
    /// 6. Project each row into a [`SearchCandidate`] with `semantic_distance`
    ///    set and `bm25 = 0.0` / `snippet = ""` (not applicable on this path).
    ///
    /// [`compile_filter`]: cairn_core::domain::filter::compile_filter
    ///
    /// # Errors
    ///
    /// - [`StoreError::CapabilityUnavailable`] when vector capability is absent.
    /// - [`StoreError::Sqlite`] / [`StoreError::Worker`] for SQL or worker failures.
    /// - [`StoreError::Codec`] when projecting a row to a typed enum fails.
    /// - [`StoreError::Invariant`] when a stored id or scope cannot be parsed.
    #[instrument(
        skip(self, args),
        err,
        fields(verb = "search_semantic", limit = args.limit),
    )]
    pub(crate) async fn do_search_semantic(
        &self,
        args: &SemanticSearchArgs<'_>,
    ) -> Result<SemanticSearchPage, StoreError> {
        let page = self.do_search_semantic_untracked(args).await?;
        self.record_search_access(&page.candidates, "semantic search")
            .await;
        Ok(page)
    }

    pub(crate) async fn do_search_semantic_untracked(
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

        // Embed the query on a blocking thread — model inference is CPU-bound.
        let query_text = args.query.clone();
        let query_vec: Vec<f32> =
            tokio::task::spawn_blocking(move || embedder.embed_query(&query_text))
                .await
                .map_err(|e| StoreError::Invariant {
                    what: format!("embedding task panicked: {e}"),
                })?
                .map_err(StoreError::Embedding)?;

        let query_bytes: Vec<u8> = query_vec.iter().flat_map(|&f| f.to_le_bytes()).collect();
        let limit = args.limit.clamp(1, SEARCH_LIMIT_MAX);
        let visibilities: Vec<String> = args
            .visibility_allowlist
            .iter()
            .map(|v| v.as_str().to_owned())
            .collect();
        let compiled = args.filter.map(compile_filter);
        let now_ms = current_unix_ms();
        let scope = args.auth_scope.clone();
        let conn = self.require_conn("search_semantic")?.clone();

        let rows = run_ann_query(
            conn,
            query_bytes,
            visibilities,
            compiled,
            limit,
            now_ms,
            scope,
        )
        .await
        .map_err(unpack_worker_err)?;

        // Model filter applied in Rust — see `run_ann_query` doc for rationale.
        let model_label = args.model_label.as_str();
        let with_explain = args.with_explain;
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|r| r.vec_model == model_label)
            .take(limit)
            .collect();

        project_semantic_page(rows, now_ms, with_explain)
    }
}

/// Single row projected from the `record_vectors JOIN records` ANN query.
struct SemanticRawRow {
    record_id: String,
    target_id: String,
    scope_json: String,
    kind: String,
    class: String,
    visibility: String,
    /// L2 distance returned by sqlite-vec.
    distance: f64,
    /// Model label stored alongside the vector (`+model` auxiliary column).
    /// Compared against `args.model_label` in Rust to skip stale-model rows —
    /// sqlite-vec forbids WHERE predicates on auxiliary columns in KNN queries.
    vec_model: String,
    /// `now_ms - r.updated_at` (milliseconds).
    recency_ms: i64,
    confidence: f64,
    salience: f64,
    /// `now_ms - r.updated_at` (milliseconds) — same column, kept distinct
    /// so future recency/staleness divergence is straightforward.
    staleness_ms: i64,
    record_json: String,
}

/// Drive the sqlite-vec ANN query on the DB worker thread.
///
/// ## Why model filtering is done in Rust, not SQL
///
/// sqlite-vec rejects any WHERE predicate on an auxiliary (`+col`) column
/// when a MATCH clause is active — even if the predicate is in an outer
/// query joined to the vec0 subquery. `SQLite`'s query planner pushes the
/// predicate down into the virtual table's `xBestIndex`, which then errors.
/// The workaround is to SELECT the auxiliary column (`model`) and filter in
/// Rust after the rows are returned.
async fn run_ann_query(
    conn: std::sync::Arc<tokio_rusqlite::Connection>,
    query_bytes: Vec<u8>,
    visibilities: Vec<String>,
    compiled: Option<cairn_core::domain::filter::CompiledFilter>,
    limit: usize,
    now_ms: i64,
    scope: cairn_core::domain::ScopeTuple,
) -> Result<Vec<SemanticRawRow>, tokio_rusqlite::Error> {
    // Over-fetch 2× so the model post-filter still delivers `limit` results
    // in the common case where all rows share the active model.
    let k = i64::try_from((limit * 2).max(1)).unwrap_or(i64::MAX);

    conn.call(move |c| {
        let vis_clause = if visibilities.is_empty() {
            String::new()
        } else {
            let placeholders = vec!["?"; visibilities.len()].join(", ");
            format!("AND r.visibility IN ({placeholders})")
        };
        let filter_clause = compiled
            .as_ref()
            .map(|cf| format!("AND ({})", cf.sql))
            .unwrap_or_default();
        let (scope_sql, scope_params) =
            crate::store::scope_predicate::build_scope_predicate("r", &scope);

        let sql = format!(
            "SELECT r.record_id, r.target_id, r.scope, r.kind, r.class,
                    r.visibility, ann.distance, ann.model AS vec_model,
                    (? - r.updated_at) AS recency_ms,
                    r.confidence, r.salience,
                    (? - r.updated_at) AS staleness_ms,
                    r.record_json
             FROM (
                 SELECT record_id, distance, model
                   FROM record_vectors
                  WHERE embedding MATCH ?
                  LIMIT {k}
             ) ann
             JOIN   records r ON r.record_id = ann.record_id
             WHERE  r.active = 1 AND r.tombstoned = 0 AND r.cow_staged = 0
               {vis_clause}
               {filter_clause}{scope_sql}
             ORDER BY ann.distance ASC"
        );

        let mut params: Vec<SqlVal> = Vec::new();
        params.push(SqlVal::Integer(now_ms));
        params.push(SqlVal::Integer(now_ms));
        params.push(SqlVal::Blob(query_bytes.clone()));
        for v in &visibilities {
            params.push(SqlVal::Text(v.clone()));
        }
        if let Some(cf) = compiled.as_ref() {
            for p in &cf.params {
                params.push(json_to_sql(p));
            }
        }
        params.extend(scope_params);

        let mut stmt = c
            .prepare_cached(&sql)
            .map_err(tokio_rusqlite::Error::Rusqlite)?;
        let raw: Vec<_> = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok(SemanticRawRow {
                    record_id: row.get::<_, String>(0)?,
                    target_id: row.get::<_, String>(1)?,
                    scope_json: row.get::<_, String>(2)?,
                    kind: row.get::<_, String>(3)?,
                    class: row.get::<_, String>(4)?,
                    visibility: row.get::<_, String>(5)?,
                    distance: row.get::<_, f64>(6)?,
                    vec_model: row.get::<_, String>(7)?,
                    recency_ms: row.get::<_, i64>(8)?,
                    confidence: row.get::<_, f64>(9)?,
                    salience: row.get::<_, f64>(10)?,
                    staleness_ms: row.get::<_, i64>(11)?,
                    record_json: row.get::<_, String>(12)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok::<_, tokio_rusqlite::Error>(raw)
    })
    .await
}

/// Project a set of [`SemanticRawRow`]s into a [`SemanticSearchPage`].
///
/// When `with_explain` is true the page's `explain` block is populated with
/// one [`ScoreExplain`] per candidate, using 1-based `semantic_rank` and the
/// negated L2 distance as `final_score` (smaller distance = higher score).
fn project_semantic_page(
    rows: Vec<SemanticRawRow>,
    _now_ms: i64,
    with_explain: bool,
) -> Result<SemanticSearchPage, StoreError> {
    let mut candidates = Vec::with_capacity(rows.len());
    for row in rows {
        candidates.push(project_semantic_row(&row)?);
    }
    let explain = if with_explain {
        Some(
            candidates
                .iter()
                .enumerate()
                .map(|(i, c)| ScoreExplain {
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
    Ok(SemanticSearchPage {
        candidates,
        explain,
    })
}

/// Project a single [`SemanticRawRow`] into a [`SearchCandidate`].
fn project_semantic_row(row: &SemanticRawRow) -> Result<SearchCandidate, StoreError> {
    let record_id = record_id_from_str(&row.record_id)?;
    let target_id = target_id_from_str(&row.target_id)?;
    let scope: ScopeTuple = serde_json::from_str(&row.scope_json)?;
    let kind = MemoryKind::parse(&row.kind).map_err(|e| StoreError::Invariant {
        what: format!("invalid kind `{}`: {e}", row.kind),
    })?;
    let class = MemoryClass::parse(&row.class).map_err(|e| StoreError::Invariant {
        what: format!("invalid class `{}`: {e}", row.class),
    })?;
    let visibility =
        MemoryVisibility::parse(&row.visibility).map_err(|e| StoreError::Invariant {
            what: format!("invalid visibility `{}`: {e}", row.visibility),
        })?;
    Ok(SearchCandidate {
        record_id,
        target_id,
        scope,
        kind,
        class,
        visibility,
        bm25: 0.0,
        snippet: String::new(),
        #[allow(clippy::cast_possible_truncation, reason = "REAL→f32 narrow")]
        semantic_distance: Some(row.distance as f32),
        // `row.recency_ms` = `now_ms - r.updated_at`. Convert ms → s and
        // clamp at zero to guard against minor clock-skew misreads.
        recency_seconds: row.recency_ms.max(0) / 1000,
        #[allow(clippy::cast_possible_truncation, reason = "REAL→f32 narrow")]
        confidence: row.confidence as f32,
        #[allow(clippy::cast_possible_truncation, reason = "REAL→f32 narrow")]
        salience: row.salience as f32,
        staleness_seconds: row.staleness_ms.max(0) / 1000,
        record_json: row.record_json.clone(),
    })
}

/// Helper trait so the worker callback can map a `StoreError` into a
/// `tokio_rusqlite::Error::Other` without naming the wrapper type at
/// every call site.
trait IntoTokio {
    fn into_tokio(self) -> tokio_rusqlite::Error;
}

impl IntoTokio for StoreError {
    fn into_tokio(self) -> tokio_rusqlite::Error {
        tokio_rusqlite::Error::Other(Box::new(self))
    }
}

#[cfg(test)]
mod tests {
    //! Index-coupling tests for the supersession predicate.
    //!
    //! These guards live next to the SQL builder so they reference the
    //! actual SQL production emits — rather than a hand-written copy
    //! that could silently drift. They cover both the index *shape*
    //! (key column, partial predicate, name) and the *use* of the
    //! `edges_updates_dst_idx` partial index by the *built* search SQL
    //! under EXPLAIN QUERY PLAN.
    //!
    //! The motivating concern (round-7/8 review): equivalent rewrites
    //! such as `e.kind IN ('updates')` or a parameterised `e.kind = ?`
    //! defeat `SQLite`'s partial-index proof rule even though they look
    //! correct to a human reader. The test drives EXPLAIN from
    //! [`build_search_query`] output so any such rewrite — whether in
    //! [`SUPERSESSION_NOT_EXISTS_CLAUSE`] or inlined directly into the
    //! builder — surfaces here as a `SCAN edges` plan.
    use rusqlite::params_from_iter;

    use super::{SUPERSESSION_NOT_EXISTS_CLAUSE, build_search_query};
    use crate::open_in_memory_sync;

    /// Asserts the partial index is shaped correctly *and* used by the
    /// production search SQL.
    ///
    /// Three independent checks together close the loopholes round-5
    /// through round-8 reviewers identified:
    /// 1. `pragma_index_xinfo` confirms the leading key column is `dst`.
    /// 2. `sqlite_schema.sql` matches the canonical partial-index DDL
    ///    exactly, so any broadened predicate (e.g. adding `OR kind =
    ///    'mentions'`) fails the test.
    /// 3. EXPLAIN QUERY PLAN of the *built* search SQL — produced by
    ///    [`build_search_query`] — reports
    ///    `SEARCH ... USING INDEX edges_updates_dst_idx` for the inner
    ///    `edges` reference. This catches predicate rewrites both
    ///    inside the constant and inlined into the builder, plus
    ///    accidental removal of the partial index.
    #[test]
    fn supersession_clause_uses_partial_index() {
        let conn = open_in_memory_sync().expect("open");

        // (1) Leading indexed column is `dst`.
        let mut xinfo = conn
            .prepare("SELECT name, key FROM pragma_index_xinfo('edges_updates_dst_idx')")
            .expect("prepare index_xinfo");
        let key_cols: Vec<(String, i64)> = xinfo
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .expect("query index_xinfo")
            .filter_map(Result::ok)
            .collect();
        let leading_key: Vec<&str> = key_cols
            .iter()
            .filter(|(_, key)| *key == 1)
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(
            leading_key,
            vec!["dst"],
            "edges_updates_dst_idx must key on dst (got {key_cols:?})",
        );

        // (2) Partial predicate is exactly the supersession-only one.
        let ddl: String = conn
            .query_row(
                "SELECT sql FROM sqlite_schema \
                 WHERE type = 'index' AND name = 'edges_updates_dst_idx'",
                [],
                |r| r.get(0),
            )
            .expect("read index DDL");
        let normalized: String = ddl.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            normalized, "CREATE INDEX edges_updates_dst_idx ON edges(dst) WHERE kind = 'updates'",
            "broadened or rewritten DDL would re-introduce edges-table bloat. Got: {ddl}",
        );

        // Sanity: the production builder must keep using the constant.
        // If a future edit inlines a different predicate string, this
        // fails before the more expensive EXPLAIN check below.
        let (sql, params) = build_search_query(
            "anything",
            &[],
            None,
            None,
            10,
            &[10.0, 10.0, 5.0, 1.0],
            &cairn_core::domain::ScopeTuple::default(),
        )
        .expect("build search SQL");
        assert!(
            sql.contains(SUPERSESSION_NOT_EXISTS_CLAUSE),
            "build_search_query must continue to emit SUPERSESSION_NOT_EXISTS_CLAUSE \
             verbatim; inlining or rewriting it bypasses the partial-index \
             coupling. Got SQL:\n{sql}",
        );

        // (3) EXPLAIN the *built* SQL with the *built* params and assert
        // the inner `edges` reference plans as a partial-index search.
        // Driving EXPLAIN from `build_search_query` rather than the
        // constant directly means a future builder edit that diverges
        // from the constant — say, inlining `kind IN ('updates')` —
        // surfaces here as a `SCAN edges` plan even if the constant
        // itself is left untouched.
        let explain_sql = format!("EXPLAIN QUERY PLAN {sql}");
        let mut stmt = conn.prepare(&explain_sql).expect("prepare EXPLAIN");
        let plan_rows: Vec<String> = stmt
            .query_map(params_from_iter(params.iter()), |r| r.get::<_, String>(3))
            .expect("query plan")
            .filter_map(Result::ok)
            .collect();
        let joined = plan_rows.join("\n");

        // Plan-row analysis. We need *both* of these to hold:
        //
        //   (a) The plan must contain no `SCAN` row other than the FTS
        //       virtual table — `SCAN records_fts` is the only legal
        //       scan in this query. Any other `SCAN <alias>` (the
        //       supersession `e`, an aliased second edges access like
        //       `SCAN u`, or even `SCAN edges` if someone re-aliased)
        //       proves a per-FTS-hit table scan was reintroduced.
        //
        //   (b) Some plan row must `SEARCH edges_updates_dst_idx` with
        //       `dst=?` — proving SQLite chose the partial index as a
        //       keyed lookup, not as a covering scan or a different
        //       index entirely.
        //
        // Anchoring (a) on a known-good allowlist (just `records_fts`)
        // rather than a denylist of edges aliases closes round-10's
        // loophole: a future builder edit that adds a second aliased
        // `FROM edges u ...` cannot satisfy the test no matter what
        // alias it picks.
        let mut illegal_scan_rows: Vec<&String> = Vec::new();
        let mut indexed_search_row: Option<&String> = None;
        for row in &plan_rows {
            if let Some(rest) = row.trim_start().strip_prefix("SCAN ") {
                let alias = rest.split_whitespace().next().unwrap_or("");
                if alias != "records_fts" {
                    illegal_scan_rows.push(row);
                }
            }
            if row.contains("edges_updates_dst_idx")
                && row.contains("SEARCH")
                && row.contains("dst=?")
            {
                indexed_search_row = Some(row);
            }
        }

        assert!(
            illegal_scan_rows.is_empty(),
            "plan contains a forbidden SCAN — the only legal scan in \
             keyword search is the FTS virtual table `records_fts`. Any \
             other SCAN means a per-FTS-hit table scan was reintroduced \
             (e.g. a second aliased edges access bypassing the partial \
             index). Offending row(s): {illegal_scan_rows:?}\n\
             full plan:\n{joined}",
        );
        assert!(
            indexed_search_row.is_some(),
            "plan must SEARCH edges_updates_dst_idx with dst=? — \
             a rewrite to e.g. `kind IN ('updates')` or parameterised \
             `kind = ?` would lose proof equivalence and surface here. \
             Full plan:\n{joined}",
        );
    }

    /// Guards `assert_finite_weights`: NaN, +Inf, and a negative coefficient
    /// must all surface as `StoreError::Invariant` so a misconfigured store
    /// fails with an actionable error rather than poisoning ranking output.
    #[test]
    fn assert_finite_weights_rejects_nan_inf_negative() {
        use super::assert_finite_weights;

        assert!(assert_finite_weights(&[10.0, 10.0, 5.0, 1.0]).is_ok());

        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            let weights = [bad, 10.0, 5.0, 1.0];
            let err =
                assert_finite_weights(&weights).expect_err("NaN/Inf/negative weights must error");
            assert!(
                matches!(err, crate::error::StoreError::Invariant { .. }),
                "expected Invariant variant, got {err:?}",
            );
        }
    }

    /// End-to-end check that `do_search_keyword` honors the weights
    /// supplied at open time: a single-token match against the `kind`
    /// column (weight 100) outranks a token-only match against the
    /// `body` column (weight 1) when the table is loaded with both rows.
    /// This exercises the full SQL path — `bm25(records_fts, w0..w3)`,
    /// the join, the visibility allowlist, and projection — rather than
    /// the SQL builder in isolation.
    #[tokio::test]
    async fn weighted_bm25_kind_match_outranks_body_match_via_search_keyword() {
        use cairn_core::contract::memory_store::{KeywordSearchArgs, MemoryStore};
        use cairn_core::domain::record::tests_export::sample_record;
        use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
        use cairn_core::domain::{RecordId, TargetId};

        // Lopsided weights make the column attribution unambiguous: a
        // body-only hit at weight 1 cannot outrank a kind-only hit at
        // weight 100, regardless of token-frequency / row-length effects.
        let store = crate::open_in_memory_with_embedder_and_config(None, [100.0, 1.0, 1.0, 1.0])
            .await
            .expect("open store with weighted bm25");

        // Record A: 'feedback' lands in the `kind` column (weight 100).
        // The body intentionally excludes the token so the only match
        // path is the kind column. Confidence/salience/tags inherited
        // from `sample_record`.
        let mut a = sample_record();
        "unrelated body content".clone_into(&mut a.body);
        a.kind = MemoryKind::Feedback;

        // Record B: 'feedback' lands in the `body` column (weight 1).
        // Use a fresh record_id and target_id so both rows persist.
        let mut b = sample_record();
        b.id = RecordId::parse("01HQZX9F5N00000000000000B1".to_owned()).expect("valid id");
        b.target_id =
            TargetId::parse("01HQZX9F5N00000000000000B2".to_owned()).expect("valid target");
        "feedback was clear and helpful".clone_into(&mut b.body);
        // Keep b.kind from sample_record (User) so the body is the only
        // place 'feedback' appears for this row.

        store.upsert(&a).await.expect("upsert a");
        store.upsert(&b).await.expect("upsert b");

        let args = KeywordSearchArgs {
            query: "feedback".into(),
            filter: None,
            auth_scope: cairn_core::domain::ScopeTuple::default(),
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 5,
            cursor: None,
            with_explain: false,
        };
        let page = store.search_keyword(&args).await.expect("search");
        assert!(
            !page.candidates.is_empty(),
            "search returned 0 results — both seeded rows should match",
        );
        assert_eq!(
            page.candidates[0].record_id,
            a.id,
            "kind-column hit (weight 100) must outrank body-column hit \
             (weight 1) at weights [100, 1, 1, 1]; got order {:?}",
            page.candidates
                .iter()
                .map(|c| c.record_id.as_str())
                .collect::<Vec<_>>(),
        );
    }
}
