//! Embedding drain task — `drain_loop` and `drain_once`.
//!
//! `drain_loop` runs as a tokio task tied to the store's lifetime, waking every
//! 30 s to call `drain_once`. `drain_once` is also the implementation behind
//! `cairn admin reindex --semantic`.

use std::sync::Arc;
use std::time::Duration;

use cairn_embeddings_local::EmbeddingModel;
use tokio_rusqlite::Connection as AsyncConn;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use crate::StoreError;

const BATCH: i64 = 32;
const MAX_ATTEMPTS: i64 = 5;

/// Statistics from a single drain pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainStats {
    /// Rows successfully embedded and removed from the queue.
    pub drained: usize,
    /// Rows that errored this pass (`attempt_count` bumped).
    pub failed: usize,
    /// Rows still in the queue after this pass.
    pub remaining: usize,
}

/// Runs forever, draining `pending_embeddings` every 30 seconds.
/// Exits cleanly when `cancel` is triggered.
pub(crate) async fn drain_loop(
    conn: Arc<AsyncConn>,
    embedder: Arc<dyn EmbeddingModel>,
    cancel: CancellationToken,
) {
    drain_loop_with_interval(conn, embedder, cancel, Duration::from_secs(30)).await;
}

/// Same as `drain_loop` but with a configurable interval (for tests).
pub(crate) async fn drain_loop_with_interval(
    conn: Arc<AsyncConn>,
    embedder: Arc<dyn EmbeddingModel>,
    cancel: CancellationToken,
    interval: Duration,
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = tick.tick() => {
                match drain_once(Arc::clone(&conn), Arc::clone(&embedder)).await {
                    Ok(stats) if stats.drained > 0 || stats.failed > 0 => {
                        tracing::info!(
                            drained = stats.drained,
                            failed = stats.failed,
                            remaining = stats.remaining,
                            "embedding drain pass"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "drain_once error"),
                }
            }
        }
    }
}

/// Single drain pass: embeds up to `BATCH` rows from `pending_embeddings`,
/// skipping rows with `attempt_count > MAX_ATTEMPTS` (poison-pill cap).
///
/// # Errors
///
/// Returns [`StoreError`] if the underlying database operation fails.
#[instrument(skip(conn, embedder), err)]
pub async fn drain_once(
    conn: Arc<AsyncConn>,
    embedder: Arc<dyn EmbeddingModel>,
) -> Result<DrainStats, StoreError> {
    // Fetch a batch of pending rows (record_id + body), skipping poison pills.
    let rows: Vec<(String, String)> = conn
        .call(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT pe.record_id, r.body
                   FROM pending_embeddings pe
                   JOIN records r ON r.record_id = pe.record_id
                  WHERE pe.attempt_count <= ?
                  ORDER BY pe.enqueued_at
                  LIMIT ?",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![MAX_ATTEMPTS, BATCH], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, tokio_rusqlite::Error>(rows)
        })
        .await?;

    let mut drained = 0usize;
    let mut failed = 0usize;
    let model_label = embedder.kind().as_str().to_owned();

    for (record_id, body) in &rows {
        let emb = Arc::clone(&embedder);
        let body_clone = body.clone();
        let embed_result =
            tokio::task::spawn_blocking(move || emb.embed_document(&body_clone)).await;

        let vector_result: Result<Vec<u8>, String> = match embed_result {
            Ok(Ok(v)) => Ok(v.iter().flat_map(|&f| f.to_le_bytes()).collect()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(e) => Err(e.to_string()),
        };

        let rid = record_id.clone();
        let ml = model_label.clone();

        match vector_result {
            Ok(bytes) => {
                conn.call(move |c| {
                    // vec0 rejects ON CONFLICT, so DELETE + INSERT (same pattern as upsert.rs).
                    let tx = c.transaction()?;
                    tx.execute(
                        "DELETE FROM record_vectors WHERE record_id = ?",
                        rusqlite::params![rid],
                    )?;
                    tx.execute(
                        "INSERT INTO record_vectors(record_id, embedding, model)
                           VALUES (?, ?, ?)",
                        rusqlite::params![rid, bytes, ml],
                    )?;
                    tx.execute(
                        "DELETE FROM pending_embeddings WHERE record_id = ?",
                        rusqlite::params![rid],
                    )?;
                    tx.commit()?;
                    Ok::<_, tokio_rusqlite::Error>(())
                })
                .await?;
                drained += 1;
            }
            Err(err_msg) => {
                conn.call(move |c| {
                    c.execute(
                        "UPDATE pending_embeddings
                            SET attempt_count   = attempt_count + 1,
                                last_error      = ?,
                                last_attempt_at = strftime('%s','now')
                          WHERE record_id = ?",
                        rusqlite::params![err_msg, rid],
                    )?;
                    Ok::<_, tokio_rusqlite::Error>(())
                })
                .await?;
                failed += 1;
            }
        }
    }

    let remaining_raw: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM pending_embeddings", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(Into::into)
        })
        .await?;
    // COUNT(*) is always non-negative; usize is at least 32 bits on all
    // supported platforms, and a table this large would OOM before overflowing.
    let remaining = usize::try_from(remaining_raw).unwrap_or(usize::MAX);

    Ok(DrainStats {
        drained,
        failed,
        remaining,
    })
}
