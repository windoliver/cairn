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

                let mut stmt = c
                    .prepare(&sql)
                    .map_err(|e| classify_fts_error(e).into_tokio())?;
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
                    .map_err(|e| classify_fts_error(e).into_tokio())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| classify_fts_error(e).into_tokio())?;

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
         WHERE r.active = 1 AND r.tombstoned = 0",
    );
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

/// Convert a `rusqlite::Error` into [`StoreError::FtsQuery`] when the
/// underlying `SQLite` message identifies it as an FTS5 parse failure.
///
/// FTS5 emits three message shapes for malformed `MATCH` operands
/// (observed against the bundled `SQLite` 3.46 build):
/// 1. `"fts5: syntax error near \"...\""`
/// 2. `"unknown special query: ..."`
/// 3. `"unterminated string"`
///
/// All three are wire-stable across `SQLite` versions used in P0; the
/// classifier matches on each prefix so the verb layer can return a
/// user-actionable error instead of a generic SQL failure.
fn classify_fts_error(err: rusqlite::Error) -> StoreError {
    let message = match &err {
        rusqlite::Error::SqliteFailure(_, Some(m)) => m.clone(),
        other => other.to_string(),
    };
    if is_fts_message(&message) {
        StoreError::FtsQuery { message }
    } else {
        StoreError::from(err)
    }
}

fn is_fts_message(msg: &str) -> bool {
    let lower_starts_with = |needle: &str| {
        msg.len() >= needle.len() && msg[..needle.len()].eq_ignore_ascii_case(needle)
    };
    lower_starts_with("fts5:")
        || lower_starts_with("unknown special query")
        || lower_starts_with("unterminated string")
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
