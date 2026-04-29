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
    KeywordCursor, KeywordSearchArgs, KeywordSearchPage, SearchCandidate,
};
use cairn_core::domain::filter::compile_filter;
use cairn_core::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
use cairn_core::domain::{RecordId, ScopeTuple};
use rusqlite::types::Value as SqlVal;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::projection::{record_id_from_str, target_id_from_str};
use crate::store::{SqliteMemoryStore, current_unix_ms};

/// Hard upper bound on a single search page. Matches `do_list` so callers
/// see consistent paging semantics across read verbs.
const SEARCH_LIMIT_MAX: usize = 1000;

/// Records-latest supersession predicate, written so `SQLite`'s partial-index
/// proof rule can match it against migration 0012's
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

        let page = conn
            .call(move |c| {
                let (sql, params) = build_search_query(
                    &query,
                    &visibilities,
                    compiled.as_ref(),
                    cursor.as_ref(),
                    limit,
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

                project_page(rows, limit, now_ms)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
            })
            .await
            .map_err(unpack_worker_err)?;
        Ok(page)
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
fn project_page(
    rows: Vec<RawRow>,
    limit: usize,
    now_ms: i64,
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
    Ok(KeywordSearchPage {
        candidates,
        next_cursor,
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
) -> Result<(String, Vec<SqlVal>), StoreError> {
    let mut params: Vec<SqlVal> = Vec::new();
    let mut sql = String::with_capacity(512);

    // The supersession predicate matches the `records_latest` view in
    // migration 0001 (brief §3 lines ~417-426): a record version is
    // "latest" only when no `updates` edge points to it. ConflictDAG /
    // PromotionWorkflow emit `updates` edges to retire stale facts, and
    // keyword search must respect the same exclusion as graph traversal —
    // otherwise a body match against an `updates`.dst can resurface a
    // fact the consolidator already retired.
    sql.push_str(
        "SELECT \
            r.record_id, r.target_id, r.scope, r.kind, r.class, r.visibility, \
            fts.bm25_score, r.updated_at, r.confidence, r.salience, r.created_at, \
            fts.snippet, r.record_json \
         FROM records r \
         JOIN ( \
            SELECT rowid, \
                   bm25(records_fts) AS bm25_score, \
                   snippet(records_fts, 0, ?, ?, ?, ?) AS snippet \
              FROM records_fts \
             WHERE records_fts MATCH ? \
         ) fts ON fts.rowid = r.rowid \
         WHERE r.active = 1 \
           AND r.tombstoned = 0 \
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
fn json_to_sql(v: &serde_json::Value) -> SqlVal {
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
        let (sql, params) =
            build_search_query("anything", &[], None, None, 10).expect("build search SQL");
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

        // Plan-row analysis. We need *both* of these to hold for every
        // EXPLAIN:
        //
        //   (a) No plan row may scan `edges` — neither a full-table
        //       `SCAN edges` nor a `SCAN e` (the supersession alias)
        //       nor a covering `SCAN e USING INDEX <other>`. Any of
        //       those means the supersession lookup regressed to a
        //       per-FTS-hit edges scan even if the partial index is
        //       still used elsewhere.
        //
        //   (b) Some plan row must `SEARCH` the partial index keyed
        //       on `dst` — proving SQLite chose the index as a keyed
        //       lookup, not as a covering scan.
        //
        // Together (a) and (b) close round-9's loophole: a future
        // builder change that introduces a second `edges` access point
        // (one indexed, one scanned) cannot satisfy both at once.
        let mut edges_scan_rows: Vec<&String> = Vec::new();
        let mut indexed_search_row: Option<&String> = None;
        for row in &plan_rows {
            // `SCAN <alias>` and `SCAN <alias> USING ...`. Split the
            // first whitespace-token after "SCAN " — if it's exactly
            // `e` (the supersession alias) or `edges` (table name),
            // this row is forbidden.
            if let Some(rest) = row.trim_start().strip_prefix("SCAN ") {
                let alias = rest.split_whitespace().next().unwrap_or("");
                if alias == "e" || alias == "edges" {
                    edges_scan_rows.push(row);
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
            edges_scan_rows.is_empty(),
            "plan contains a forbidden SCAN of edges — supersession check \
             must always go through edges_updates_dst_idx. Offending row(s): \
             {edges_scan_rows:?}\nfull plan:\n{joined}",
        );
        assert!(
            indexed_search_row.is_some(),
            "plan must SEARCH edges_updates_dst_idx with dst=? — \
             a rewrite to e.g. `kind IN ('updates')` or parameterised \
             `kind = ?` would lose proof equivalence and surface here. \
             Full plan:\n{joined}",
        );
    }
}
