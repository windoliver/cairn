//! `MemoryStore::tombstone` impl.
//!
//! Marks one specific record version (by `record_id`, not `target_id`) as
//! tombstoned with the given reason. Idempotent: re-tombstoning the same
//! row writes the same `tombstoned = 1` flag without producing a new row,
//! so callers in retry/replay paths can invoke this freely.
//!
//! The supersession lifecycle (brief §5.6) lives elsewhere — this verb
//! does not deactivate other versions of the same target, only the row
//! the caller named.

use cairn_core::contract::memory_store::TombstoneReason;
use cairn_core::domain::RecordId;
use rusqlite::params;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::{SqliteMemoryStore, current_unix_ms};

impl SqliteMemoryStore {
    /// Inherent tombstone implementation; the trait method
    /// [`MemoryStore::tombstone`] guards `self.conn` then delegates here.
    ///
    /// Updates `tombstoned`, `tombstone_reason`, and `updated_at` on the
    /// single row matching `record_id`. Missing rows silently no-op (zero
    /// rows affected); the contract treats "already gone" the same as
    /// "successfully tombstoned" for retry safety.
    ///
    /// [`MemoryStore::tombstone`]: cairn_core::contract::memory_store::MemoryStore::tombstone
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] when the background `tokio_rusqlite`
    /// worker fails (channel closed, panic in worker) and
    /// [`StoreError::Sqlite`] for SQL errors surfaced through the worker.
    #[instrument(
        skip(self),
        err,
        fields(verb = "tombstone", record_id = %id.as_str(), reason = ?reason),
    )]
    pub(crate) async fn do_tombstone(
        &self,
        id: &RecordId,
        reason: TombstoneReason,
    ) -> Result<(), StoreError> {
        let conn = self.require_conn("tombstone")?.clone();
        let key = id.as_str().to_owned();
        let reason_str = reason.as_db_str();

        conn.call(move |c| {
            let now_ms = current_unix_ms();
            c.execute(
                "UPDATE records \
                    SET tombstoned = 1, tombstone_reason = ?1, updated_at = ?2 \
                  WHERE record_id = ?3",
                params![reason_str, now_ms, key],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await?;
        Ok(())
    }
}
