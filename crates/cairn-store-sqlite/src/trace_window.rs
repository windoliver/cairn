//! `list_trace_turns` — page through a session's `turn_summary` records
//! ordered by `trace_sequence`.
//!
//! Used by `cairn-workflows::consolidation` to build the candidate
//! window for [`cairn_core::pipeline::consolidation::pick_window`].

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
}
