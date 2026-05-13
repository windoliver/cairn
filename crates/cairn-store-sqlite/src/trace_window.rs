//! `list_trace_turns` — page through a session's `turn_summary` records
//! ordered by `trace_sequence`.
//!
//! Used by `cairn-workflows::consolidation` to build the candidate
//! window for [`cairn_core::pipeline::consolidation::pick_window`].
//!
//! Also provides [`SqliteMemoryStore::find_summaries_by_source`] for
//! the forget-cleanup handler (Task 14).

use cairn_core::domain::ScopeTuple;
use cairn_core::domain::record::RecordId;
use cairn_core::pipeline::consolidation::TurnHeader;
use rusqlite::params;
use rusqlite::types::Value;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Page through `turn_summary` records for `session_id` whose
    /// `trace_sequence > since_sequence`, ascending. Capped by `limit`.
    /// Convenience wrapper around [`Self::list_trace_turns_scoped`]
    /// with no scope filter.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] on background-thread failure or
    /// [`StoreError::Sqlite`] for surfaced SQL errors.
    pub async fn list_trace_turns(
        &self,
        session_id: &str,
        since_sequence: u32,
        limit: u32,
    ) -> Result<Vec<TurnHeader>, StoreError> {
        self.list_trace_turns_scoped(session_id, since_sequence, limit, None)
            .await
    }

    /// Like [`Self::list_trace_turns`] but additionally filters by the
    /// caller-bound [`ScopeTuple`]. When `bound_scope` is `Some`, only
    /// rows whose `scope` JSON matches every set dimension are returned
    /// — preventing one tenant's consolidation handler from reading
    /// another tenant's `turn_summary` records that happen to share a
    /// session id (round-4 adversarial review #1). When `None`, no
    /// scope narrowing is applied (single-tenant P0 default).
    ///
    /// Salience is set to a constant `0.5` baseline — real salience
    /// scoring is a separate workstream.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] on background-thread failure or
    /// [`StoreError::Sqlite`] for surfaced SQL errors.
    pub async fn list_trace_turns_scoped(
        &self,
        session_id: &str,
        since_sequence: u32,
        limit: u32,
        bound_scope: Option<&ScopeTuple>,
    ) -> Result<Vec<TurnHeader>, StoreError> {
        let conn = self.require_conn("list_trace_turns_scoped")?.clone();
        let session_id = session_id.to_owned();
        let (extra_where, scope_bind_values) = scope_where_clause(bound_scope);

        let rows = conn
            .call(move |c| {
                // All placeholders explicit-numbered. The `{extra_where}`
                // fragment uses `?4`, `?5`, … so the static LIMIT slot
                // doesn't collide with auto-numbered unnamed `?`.
                let sql = format!(
                    "SELECT record_id,
                            trace_session_id,
                            trace_turn_id,
                            trace_sequence,
                            length(body) AS body_len
                     FROM records
                     WHERE trace_event = 'turn_summary'
                       AND trace_session_id = ?1
                       AND trace_sequence   > ?2
                       AND active     = 1
                       AND tombstoned = 0
                       {extra_where}
                     ORDER BY trace_sequence ASC
                     LIMIT ?3"
                );
                let mut stmt = c.prepare_cached(&sql)?;
                // First three slots are the fixed args; remaining slots are
                // the scope-dimension binds in the order produced above.
                let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![
                    Box::new(session_id.clone()),
                    Box::new(since_sequence),
                    Box::new(limit),
                ];
                for v in &scope_bind_values {
                    binds.push(Box::new(v.clone()));
                }
                let param_refs: Vec<&dyn rusqlite::ToSql> =
                    binds.iter().map(std::convert::AsRef::as_ref).collect();
                let rows: Result<Vec<_>, rusqlite::Error> = stmt
                    .query_map(param_refs.as_slice(), |row| {
                        Ok((
                            row.get::<_, String>(0)?, // record_id
                            row.get::<_, String>(1)?, // trace_session_id
                            row.get::<_, String>(2)?, // trace_turn_id
                            row.get::<_, u32>(3)?,    // trace_sequence
                            row.get::<_, u32>(4)?,    // body_len
                        ))
                    })?
                    .collect();
                Ok(rows?)
            })
            .await?;

        let headers = rows
            .into_iter()
            .map(
                |(record_id, session_id, turn_id, sequence, body_len)| TurnHeader {
                    record_id,
                    session_id,
                    turn_id,
                    sequence,
                    // Approximate token count: characters / 4.
                    approx_tokens: body_len / 4,
                    // Constant baseline — real salience scoring is a separate workstream.
                    salience: 0.5,
                },
            )
            .collect();

        Ok(headers)
    }

    /// Find the `record_id`s of active, non-tombstoned consolidation-summary
    /// records whose `extra_frontmatter.consolidation.source_record_ids` JSON
    /// array contains `source_record_id`.
    ///
    /// Used by the forget-cleanup handler to locate orphan-linked summaries
    /// after their source turn record has been tombstoned with reason `Forget`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] on background-thread failure or
    /// [`StoreError::Sqlite`] for surfaced SQL errors.
    pub async fn find_summaries_by_source(
        &self,
        source_record_id: &str,
    ) -> Result<Vec<RecordId>, StoreError> {
        let conn = self.require_conn("find_summaries_by_source")?.clone();
        let source_id = source_record_id.to_owned();

        let raw_ids = conn
            .call(move |c| {
                const SQL: &str = "
                    SELECT record_id
                    FROM records
                    WHERE active = 1 AND tombstoned = 0
                      AND json_extract(extra_frontmatter, '$.consolidation') IS NOT NULL
                      AND EXISTS (
                          SELECT 1
                          FROM json_each(
                              json_extract(extra_frontmatter,
                                           '$.consolidation.source_record_ids')
                          )
                          WHERE value = ?1
                      )
                ";
                let mut stmt = c.prepare_cached(SQL)?;
                let rows: Result<Vec<String>, rusqlite::Error> = stmt
                    .query_map(params![source_id], |row| row.get::<_, String>(0))?
                    .collect();
                Ok(rows?)
            })
            .await?;

        raw_ids
            .into_iter()
            .map(|s| {
                RecordId::parse(s).map_err(|e| StoreError::Invariant {
                    what: format!("find_summaries_by_source: invalid record_id in DB: {e}"),
                })
            })
            .collect()
    }

    /// Return the highest `trace.sequence` across `turn_summary`
    /// records (active **or** tombstoned) for `session_id`, narrowed by
    /// the optional bound scope. Returns `0` when no summary exists yet.
    /// Used by the `capture_trace` enqueue path as the
    /// `latest_sequence` argument to `enqueue_if_due`, so cadence
    /// progress doesn't regress when a forget tombstones the newest
    /// `turn_summary` (round-5 adversarial review #2).
    ///
    /// # Errors
    /// Returns [`StoreError::Worker`] on background-thread failure or
    /// [`StoreError::Sqlite`] for surfaced SQL errors.
    pub async fn max_turn_summary_sequence_scoped(
        &self,
        session_id: &str,
        bound_scope: Option<&ScopeTuple>,
    ) -> Result<u32, StoreError> {
        let conn = self
            .require_conn("max_turn_summary_sequence_scoped")?
            .clone();
        let session = session_id.to_owned();
        let (extra_where, scope_bind_values) = scope_where_clause_starting_at(bound_scope, 2);
        let max_seq: Option<i64> = conn
            .call(move |c| {
                let sql = format!(
                    "SELECT MAX(trace_sequence)
                     FROM records
                     WHERE trace_event = 'turn_summary'
                       AND trace_session_id = ?1
                       {extra_where}"
                );
                let mut stmt = c.prepare_cached(&sql)?;
                let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(session.clone())];
                for v in &scope_bind_values {
                    binds.push(Box::new(v.clone()));
                }
                let param_refs: Vec<&dyn rusqlite::ToSql> =
                    binds.iter().map(std::convert::AsRef::as_ref).collect();
                let v: Option<i64> = stmt.query_row(param_refs.as_slice(), |row| row.get(0))?;
                Ok(v)
            })
            .await?;
        Ok(u32::try_from(max_seq.unwrap_or(0).max(0)).unwrap_or(u32::MAX))
    }

    /// Return the highest `extra_frontmatter.consolidation.last_sequence`
    /// across rolling-summary records (active **or** tombstoned) for
    /// `session_id`. Returns `0` when no summary exists yet.
    /// Convenience wrapper around
    /// [`Self::latest_consolidation_watermark_scoped`] with no scope
    /// narrowing.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] on background-thread failure or
    /// [`StoreError::Sqlite`] for surfaced SQL errors.
    pub async fn latest_consolidation_watermark(
        &self,
        session_id: &str,
    ) -> Result<u32, StoreError> {
        self.latest_consolidation_watermark_scoped(session_id, None)
            .await
    }

    /// Like [`Self::latest_consolidation_watermark`] but additionally
    /// narrows by the caller-bound [`ScopeTuple`]. Required so two
    /// scopes sharing a session id don't collide on the same watermark
    /// (round-4 adversarial review #1).
    ///
    /// Note: the watermark INCLUDES tombstoned summaries (round-3
    /// adversarial review #1) — once a window is consolidated, the
    /// watermark advances permanently even if forget-cleanup later
    /// removes the synthesized prose.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] on background-thread failure or
    /// [`StoreError::Sqlite`] for surfaced SQL errors.
    pub async fn latest_consolidation_watermark_scoped(
        &self,
        session_id: &str,
        bound_scope: Option<&ScopeTuple>,
    ) -> Result<u32, StoreError> {
        let conn = self
            .require_conn("latest_consolidation_watermark_scoped")?
            .clone();
        let session = session_id.to_owned();
        // Watermark SQL has one fixed placeholder (?1 for session). Scope
        // dims start at ?2.
        let (extra_where, scope_bind_values) = scope_where_clause_starting_at(bound_scope, 2);

        let watermark: Option<i64> = conn
            .call(move |c| {
                let sql = format!(
                    "SELECT MAX(CAST(
                        json_extract(extra_frontmatter, '$.consolidation.last_sequence')
                        AS INTEGER))
                     FROM records
                     WHERE kind = 'reasoning'
                       AND json_extract(extra_frontmatter, '$.consolidation') IS NOT NULL
                       AND json_extract(scope, '$.session_id') = ?1
                       {extra_where}"
                );
                let mut stmt = c.prepare_cached(&sql)?;
                let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(session.clone())];
                for v in &scope_bind_values {
                    binds.push(Box::new(v.clone()));
                }
                let param_refs: Vec<&dyn rusqlite::ToSql> =
                    binds.iter().map(std::convert::AsRef::as_ref).collect();
                let v: Option<i64> = stmt.query_row(param_refs.as_slice(), |row| row.get(0))?;
                Ok(v)
            })
            .await?;
        Ok(u32::try_from(watermark.unwrap_or(0).max(0)).unwrap_or(u32::MAX))
    }
}

/// One session backlog entry returned by
/// [`SqliteMemoryStore::list_consolidation_backlog`]. The fields are
/// passed verbatim to `cairn-workflows::consolidation::enqueue_if_due_scoped`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationBacklogEntry {
    /// Session id of the backlog entry.
    pub session_id: String,
    /// Serialized `ScopeTuple` JSON (verbatim from the records row).
    pub scope_json: String,
    /// Active eligible `turn_summary` count past `since_sequence`.
    pub active_eligible: u32,
    /// Watermark used as `since_sequence`.
    pub since_sequence: u32,
}

impl SqliteMemoryStore {
    /// Scan every `(session, scope)` whose active `turn_summary` count
    /// past the latest consolidation watermark is at least
    /// `min_turns_for_trigger`. Used by the startup reconciliation pass
    /// in `cairn-workflows::consolidation::reconcile` to recover from
    /// the post-capture crash window (round-9 adversarial review #1).
    ///
    /// # Errors
    /// Returns [`StoreError::Worker`] on background-thread failure or
    /// [`StoreError::Sqlite`] for surfaced SQL errors.
    pub async fn list_consolidation_backlog(
        &self,
        min_turns_for_trigger: u32,
    ) -> Result<Vec<ConsolidationBacklogEntry>, StoreError> {
        let conn = self.require_conn("list_consolidation_backlog")?.clone();
        let min_turns = i64::from(min_turns_for_trigger);
        let rows: Vec<(String, String, i64, i64)> = conn
            .call(move |c| {
                const SQL: &str = "
                    WITH watermarks AS (
                        SELECT
                            json_extract(scope, '$.session_id') AS session_id,
                            scope                               AS scope_json,
                            COALESCE(MAX(CAST(
                                json_extract(extra_frontmatter,
                                             '$.consolidation.last_sequence')
                                AS INTEGER)), 0)                AS watermark
                        FROM records
                        WHERE kind = 'reasoning'
                          AND json_extract(extra_frontmatter, '$.consolidation') IS NOT NULL
                          AND json_extract(scope, '$.session_id') IS NOT NULL
                        GROUP BY session_id, scope_json
                    ),
                    sessions AS (
                        SELECT
                            r.trace_session_id  AS session_id,
                            r.scope             AS scope_json,
                            COALESCE(w.watermark, 0) AS watermark
                        FROM records r
                        LEFT JOIN watermarks w
                          ON w.session_id = r.trace_session_id
                         AND w.scope_json = r.scope
                        WHERE r.trace_event = 'turn_summary'
                          AND r.trace_session_id IS NOT NULL
                          AND r.active = 1 AND r.tombstoned = 0
                        GROUP BY r.trace_session_id, r.scope, w.watermark
                    )
                    SELECT
                        s.session_id,
                        s.scope_json,
                        s.watermark,
                        (SELECT COUNT(*) FROM records r2
                         WHERE r2.trace_event = 'turn_summary'
                           AND r2.trace_session_id = s.session_id
                           AND r2.scope            = s.scope_json
                           AND r2.active = 1 AND r2.tombstoned = 0
                           AND r2.trace_sequence  > s.watermark) AS active_eligible
                    FROM sessions s
                    WHERE active_eligible >= ?1
                ";
                let mut stmt = c.prepare_cached(SQL)?;
                let mut out = Vec::new();
                let mut q = stmt.query(params![min_turns])?;
                while let Some(row) = q.next()? {
                    out.push((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ));
                }
                Ok(out)
            })
            .await?;
        Ok(rows
            .into_iter()
            .map(|(session_id, scope_json, watermark, active_eligible)| {
                ConsolidationBacklogEntry {
                    session_id,
                    scope_json,
                    active_eligible: u32::try_from(active_eligible.max(0)).unwrap_or(u32::MAX),
                    since_sequence: u32::try_from(watermark.max(0)).unwrap_or(u32::MAX),
                }
            })
            .collect())
    }
}

/// Build the `AND json_extract(scope, '$.<dim>') = ?N` predicate
/// fragment plus the matching bind values for every set dimension of
/// `bound_scope`.
///
/// `first_placeholder` is the lowest `?N` index to start from — callers
/// pass it so the generated `?N` numbers don't collide with the SQL
/// template's static placeholders (mixing `?N` and unnamed `?` in
/// `SQLite` leads to confusing implicit renumbering and silent
/// mismatches; numbering every placeholder explicitly is safer).
///
/// Returns `(empty, empty)` when `bound_scope` is `None` or has no
/// dimensions set.
fn scope_where_clause(bound_scope: Option<&ScopeTuple>) -> (String, Vec<Value>) {
    scope_where_clause_starting_at(bound_scope, 4)
}

fn scope_where_clause_starting_at(
    bound_scope: Option<&ScopeTuple>,
    first_placeholder: usize,
) -> (String, Vec<Value>) {
    use std::fmt::Write as _;
    let Some(scope) = bound_scope else {
        return (String::new(), Vec::new());
    };
    let mut clauses = String::new();
    let mut binds: Vec<Value> = Vec::new();
    let dims: &[(&str, Option<&str>)] = &[
        ("tenant", scope.tenant.as_deref()),
        ("workspace", scope.workspace.as_deref()),
        ("user", scope.user.as_deref()),
        ("agent", scope.agent.as_deref()),
        ("entity", scope.entity.as_deref()),
    ];
    let mut next = first_placeholder;
    for (name, value) in dims {
        if let Some(v) = value {
            let _ = write!(clauses, " AND json_extract(scope, '$.{name}') = ?{next}");
            binds.push(Value::Text((*v).to_owned()));
            next += 1;
        }
    }
    (clauses, binds)
}
