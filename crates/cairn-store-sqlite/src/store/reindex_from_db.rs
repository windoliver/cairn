//! `cairn admin reindex --from-db`: rebuild FTS5 + vector indexes from the
//! authoritative `records` table.
//!
//! Two transactions:
//!   TX1 — `DELETE FROM records_fts;` + repopulate from `records`
//!         (active = 1 AND tombstoned = 0).
//!   TX2 — `DELETE FROM record_vectors;` + enqueue all active records
//!         into `pending_embeddings` with `reason = 'opt_in_backfill'`.
//!
//! The caller drives the existing `drain_once` loop afterwards.
//!
//! Idempotent: re-running succeeds with the same final counts.

use std::sync::Arc;

use tracing::instrument;

use crate::error::StoreError;

/// Statistics returned by [`rebuild_from_db`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RebuildStats {
    /// Number of rows inserted into the FTS5 mirror.
    pub fts_rebuilt: u64,
    /// Number of rows enqueued into `pending_embeddings` for re-embedding.
    pub enqueued: u64,
}

/// Rebuild FTS5 and vector indexes from the authoritative `records` table.
///
/// Runs two independent transactions:
/// 1. Truncate `records_fts` and repopulate from active, non-tombstoned records.
/// 2. Truncate `record_vectors` and enqueue all active, non-tombstoned records
///    into `pending_embeddings` with `reason = 'opt_in_backfill'`.
///
/// The caller is responsible for driving the [`crate::store::reindex::drain_once`]
/// loop afterwards to materialise the new embeddings.
///
/// # Errors
///
/// Returns [`StoreError`] if any underlying database operation fails.
#[instrument(skip(conn), err)]
pub async fn rebuild_from_db(
    conn: Arc<tokio_rusqlite::Connection>,
) -> Result<RebuildStats, StoreError> {
    // TX1 — FTS5 mirror.
    // Schema (migration 0030): records_fts(rowid, kind, class, scope, body)
    let fts_rebuilt: u64 = conn
        .call(|c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM records_fts", [])?;
            let inserted = tx.execute(
                "INSERT INTO records_fts (rowid, kind, class, scope, body)
                   SELECT rowid, kind, class, scope, body
                     FROM records
                    WHERE active = 1 AND tombstoned = 0",
                [],
            )?;
            tx.commit()?;
            // `execute` returns `usize`; cast is lossless on all 64-bit targets
            // (the records table cannot hold 2^64 rows without exhausting memory
            // first).
            #[allow(clippy::cast_possible_truncation)]
            Ok::<_, tokio_rusqlite::Error>(inserted as u64)
        })
        .await?;

    // TX2 — vector enqueue.
    // Truncate the vec0 virtual table then enqueue all active records.
    // `DELETE FROM record_vectors` (whole-table, no WHERE) is attempted first;
    // the existing per-row pattern in reindex.rs uses `WHERE record_id = ?`
    // because vec0 was found to support it, but whole-table truncation is also
    // supported per the vec0 documentation for this version.
    let enqueued: u64 = conn
        .call(|c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM record_vectors", [])?;
            let inserted = tx.execute(
                "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
                   SELECT record_id, 'opt_in_backfill', 0, strftime('%s','now')
                     FROM records
                    WHERE active = 1 AND tombstoned = 0
                  ON CONFLICT(record_id) DO UPDATE
                    SET reason = 'opt_in_backfill', attempt_count = 0",
                [],
            )?;
            tx.commit()?;
            #[allow(clippy::cast_possible_truncation)]
            Ok::<_, tokio_rusqlite::Error>(inserted as u64)
        })
        .await?;

    Ok(RebuildStats {
        fts_rebuilt,
        enqueued,
    })
}
