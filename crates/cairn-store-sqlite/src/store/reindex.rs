//! Embedding drain task — `drain_loop` and `drain_once`.
//! Full implementation in Task 9; stub here so Task 6 compiles.

use std::sync::Arc;

use cairn_embeddings_local::EmbeddingModel;
use tokio_rusqlite::Connection as AsyncConn;
use tokio_util::sync::CancellationToken;

/// Statistics from a single drain pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainStats {
    /// Rows successfully embedded and removed from the queue.
    pub drained: usize,
    /// Rows that errored this pass.
    pub failed: usize,
    /// Rows still in the queue.
    pub remaining: usize,
}

/// Runs forever, draining `pending_embeddings` on a 30-second interval.
/// Full implementation in Task 9.
pub(crate) async fn drain_loop(
    _conn: Arc<AsyncConn>,
    _embedder: Arc<dyn EmbeddingModel>,
    cancel: CancellationToken,
) {
    cancel.cancelled().await;
}

/// Single drain pass. Full implementation in Task 9.
///
/// # Errors
/// Returns [`StoreError`] if the underlying database operation fails.
pub async fn drain_once(
    _conn: Arc<AsyncConn>,
    _embedder: Arc<dyn EmbeddingModel>,
) -> Result<DrainStats, crate::StoreError> {
    Ok(DrainStats {
        drained: 0,
        failed: 0,
        remaining: 0,
    })
}
