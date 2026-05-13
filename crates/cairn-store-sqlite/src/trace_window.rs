//! `list_trace_turns` — page through a session's `turn_summary` records
//! ordered by `trace_sequence`.
//!
//! Used by `cairn-workflows::consolidation` to build the candidate
//! window for [`cairn_core::pipeline::consolidation::pick_window`].
//!
//! Also provides [`SqliteMemoryStore::find_summaries_by_source`] for
//! the forget-cleanup handler (Task 14).

use cairn_core::domain::record::RecordId;
use cairn_core::pipeline::consolidation::TurnHeader;
use rusqlite::params;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Page through `turn_summary` records for `session_id` whose
    /// `trace_sequence > since_sequence`, ascending. Capped by `limit`.
    ///
    /// Salience is set to a constant `0.5` baseline — real salience
    /// scoring is a separate workstream.
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
        let conn = self.require_conn("list_trace_turns")?.clone();
        let session_id = session_id.to_owned();

        let rows = conn
            .call(move |c| {
                const SQL: &str = "
                    SELECT record_id,
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
                    ORDER BY trace_sequence ASC
                    LIMIT ?3
                ";
                let mut stmt = c.prepare_cached(SQL)?;
                let rows: Result<Vec<_>, rusqlite::Error> = stmt
                    .query_map(
                        params![session_id, since_sequence, limit],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,  // record_id
                                row.get::<_, String>(1)?,  // trace_session_id
                                row.get::<_, String>(2)?,  // trace_turn_id
                                row.get::<_, u32>(3)?,     // trace_sequence
                                row.get::<_, u32>(4)?,     // body_len (approx chars / 4 tokens)
                            ))
                        },
                    )?
                    .collect();
                Ok(rows?)
            })
            .await?;

        let headers = rows
            .into_iter()
            .map(|(record_id, session_id, turn_id, sequence, body_len)| TurnHeader {
                record_id,
                session_id,
                turn_id,
                sequence,
                // Approximate token count: characters / 4.
                approx_tokens: body_len / 4,
                // Constant baseline — real salience scoring is a separate workstream.
                salience: 0.5,
            })
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
                let rows: Result<Vec<String>, rusqlite::Error> =
                    stmt.query_map(params![source_id], |row| row.get::<_, String>(0))?
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
}
