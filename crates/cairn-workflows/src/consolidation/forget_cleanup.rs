//! `ConsolidationForgetCleanupHandler` — when a source turn record is
//! forgotten, any rolling summary that referenced it as a source
//! becomes orphan-linked. The cleanup handler tombstones such
//! summaries with reason `Forget`. Triggered by the `forget` verb
//! enqueuing one of these jobs per forgotten record.

use std::sync::Arc;

use cairn_core::contract::job_store::{JobKind, JobPayload};
use cairn_core::contract::memory_store::{MemoryStore, TombstoneReason};
use cairn_store_sqlite::SqliteMemoryStore;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::scheduler::{HandlerOutcome, JobHandler};

/// Kind discriminator stored in `workflow_jobs.kind`.
pub const FORGET_CLEANUP_KIND: &str = "consolidation.forget_cleanup";

/// Payload — the record id that was forgotten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgetCleanupPayload {
    /// The forgotten source `record_id`.
    pub forgotten_record_id: String,
}

impl ForgetCleanupPayload {
    /// Serialize to a [`JobPayload`] byte vector.
    ///
    /// # Errors
    ///
    /// JSON encoding; unreachable in practice for this flat struct.
    pub fn to_bytes(&self) -> Result<JobPayload, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// Forget-cleanup handler.
///
/// When the `forget` verb tombstones a source turn record, it enqueues a
/// [`ForgetCleanupPayload`] job. This handler finds any active
/// consolidation-summary records that listed the forgotten record in their
/// `extra_frontmatter.consolidation.source_record_ids` array and tombstones
/// each one with [`TombstoneReason::Forget`].
pub struct ConsolidationForgetCleanupHandler {
    store: Arc<SqliteMemoryStore>,
}

impl ConsolidationForgetCleanupHandler {
    /// Construct with a shared store handle.
    #[must_use]
    pub fn new(store: Arc<SqliteMemoryStore>) -> Self {
        Self { store }
    }

    async fn run(
        &self,
        payload: ForgetCleanupPayload,
    ) -> Result<HandlerOutcome, Box<dyn std::error::Error + Send + Sync>> {
        let summaries = self
            .store
            .find_summaries_by_source(&payload.forgotten_record_id)
            .await?;

        for record_id in summaries {
            self.store
                .tombstone(&record_id, TombstoneReason::Forget)
                .await?;
            info!(
                source = %payload.forgotten_record_id,
                summary = %record_id.as_str(),
                "tombstoned orphan summary"
            );
        }

        Ok(HandlerOutcome::Done)
    }
}

#[async_trait::async_trait]
impl JobHandler for ConsolidationForgetCleanupHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(FORGET_CLEANUP_KIND)
    }

    async fn handle(&self, bytes: &JobPayload) -> HandlerOutcome {
        let payload = match serde_json::from_slice::<ForgetCleanupPayload>(bytes) {
            Ok(p) => p,
            Err(e) => {
                return HandlerOutcome::Permanent {
                    reason: format!("payload decode: {e}"),
                };
            }
        };
        match self.run(payload).await {
            Ok(o) => o,
            Err(e) => {
                warn!(error = %e, "forget cleanup failed");
                HandlerOutcome::Retry {
                    reason: e.to_string(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use cairn_test_fixtures::trace::sample_turn_summary;

    async fn make_store() -> Arc<SqliteMemoryStore> {
        Arc::new(
            cairn_store_sqlite::open_in_memory()
                .await
                .expect("open in-memory store"),
        )
    }

    const SESSION_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    #[tokio::test]
    #[allow(
        clippy::expect_used,
        reason = "test: panics surface broken invariants immediately"
    )]
    async fn cleanup_decode_error_is_permanent() {
        let store = make_store().await;
        let h = ConsolidationForgetCleanupHandler::new(store);
        let outcome = h.handle(&b"{not json".to_vec()).await;
        assert!(
            matches!(outcome, HandlerOutcome::Permanent { .. }),
            "malformed payload must yield Permanent, got {outcome:?}"
        );
    }

    #[tokio::test]
    #[allow(
        clippy::expect_used,
        reason = "test: panics surface broken invariants immediately"
    )]
    async fn find_summaries_by_source_empty_when_no_match() {
        let store = make_store().await;
        // Insert a turn_summary but no consolidation summary.
        store
            .upsert(&sample_turn_summary(SESSION_A, 1))
            .await
            .expect("upsert turn");

        let ids = store
            .find_summaries_by_source("01ARZ3NDEKTSV4RRFFQ69G5FAX")
            .await
            .expect("find_summaries_by_source");
        assert!(ids.is_empty(), "expected no summaries; got {ids:?}");
    }

    #[tokio::test]
    #[allow(
        clippy::expect_used,
        reason = "test: panics surface broken invariants immediately"
    )]
    async fn cleanup_done_when_no_orphans() {
        let store = make_store().await;
        let h = ConsolidationForgetCleanupHandler::new(store);
        let payload = ForgetCleanupPayload {
            forgotten_record_id: "01ARZ3NDEKTSV4RRFFQ69G5FAX".to_owned(),
        };
        let bytes = payload.to_bytes().expect("encode");
        let outcome = h.handle(&bytes).await;
        assert_eq!(
            outcome,
            HandlerOutcome::Done,
            "handler must succeed even when no orphan summaries exist"
        );
    }

    // Note: the end-to-end scenario — emit a summary, tombstone its source,
    // run the handler, verify the summary is tombstoned — is exercised in
    // tests/forget_propagation.rs (Task 19).
}
