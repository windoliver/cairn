//! Enqueue rolling-summary jobs. Called from the `capture_trace` verb
//! after every `turn_summary` write. Idempotent via `dedupe_key`.

use cairn_core::config::ConsolidationConfig;
use cairn_core::contract::job_store::{
    EnqueueRequest, JobId, JobKind, JobStore, JobStoreError, RetryPolicy,
};

use super::{CONSOLIDATION_KIND, ConsolidationPayload};

/// Outcome of a single [`enqueue_if_due`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueDecision {
    /// A consolidation job was accepted (or deduplicated) by the store.
    Enqueued {
        /// The stable [`JobId`] used for this enqueue request. Identical
        /// on a deduplicated second call so the caller can log it.
        job_id: JobId,
    },
    /// The trigger threshold was not reached; no job was submitted.
    NotDue {
        /// Highest turn sequence in the current session at call time.
        latest_sequence: u32,
        /// Watermark from the previous summary — turns `<= since_sequence`
        /// are already covered.
        since_sequence: u32,
    },
    /// Consolidation is administratively disabled via
    /// [`ConsolidationConfig::enabled`]. No job will ever be submitted
    /// while this flag is `false`.
    Disabled,
}

/// Enqueue a consolidation job if the cadence threshold is met.
///
/// Returns [`EnqueueDecision::Disabled`] when `config.enabled` is `false`,
/// [`EnqueueDecision::NotDue`] when fewer than `config.min_turns_for_trigger`
/// new turns have accumulated since `since_sequence`, and
/// [`EnqueueDecision::Enqueued`] otherwise.
///
/// The call is idempotent: a second invocation with the same `(session_id,
/// since_sequence)` pair hits the store's `dedupe_key` constraint and is
/// silently swallowed — `Enqueued` is still returned so the caller can
/// confirm idempotent success.
///
/// # Errors
/// Returns [`JobStoreError::Backend`] on underlying store I/O failures.
/// [`JobStoreError::DuplicateDedupeKey`] is swallowed and mapped to
/// `Enqueued`.
pub async fn enqueue_if_due(
    store: &dyn JobStore,
    config: &ConsolidationConfig,
    session_id: &str,
    latest_sequence: u32,
    since_sequence: u32,
    now_ms: i64,
) -> Result<EnqueueDecision, JobStoreError> {
    if !config.enabled {
        return Ok(EnqueueDecision::Disabled);
    }
    let new_turns = latest_sequence.saturating_sub(since_sequence);
    if new_turns < config.min_turns_for_trigger {
        return Ok(EnqueueDecision::NotDue {
            latest_sequence,
            since_sequence,
        });
    }
    let payload = ConsolidationPayload {
        session_id: session_id.to_owned(),
        since_sequence,
    };
    let bytes = payload
        .to_bytes()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    let job_id = JobId::new(format!("consolidate:{session_id}:{since_sequence}"));
    let req = EnqueueRequest {
        job_id: job_id.clone(),
        kind: JobKind::new(CONSOLIDATION_KIND),
        payload: bytes,
        queue_key: Some(format!("consolidation:{session_id}")),
        dedupe_key: Some(format!("{session_id}:{since_sequence}")),
        not_before_ms: now_ms,
        retry: RetryPolicy::DEFAULT,
    };
    match store.enqueue(req).await {
        Ok(()) | Err(JobStoreError::DuplicateDedupeKey { .. }) => {
            Ok(EnqueueDecision::Enqueued { job_id })
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::SqliteJobStore;
    use crate::sqlite_store::install_for_tests;
    use rusqlite::Connection;

    fn store() -> Arc<dyn JobStore> {
        let conn = Connection::open_in_memory().expect("conn");
        install_for_tests(&conn);
        Arc::new(SqliteJobStore::new(conn).expect("store"))
    }

    #[tokio::test]
    async fn not_due_below_threshold() {
        let s = store();
        let cfg = ConsolidationConfig::default();
        let d = enqueue_if_due(&*s, &cfg, "s1", 2, 0, 1_000)
            .await
            .expect("ok");
        assert!(matches!(d, EnqueueDecision::NotDue { .. }));
    }

    #[tokio::test]
    async fn enqueues_when_due() {
        let s = store();
        let cfg = ConsolidationConfig::default();
        let d = enqueue_if_due(&*s, &cfg, "s1", 10, 0, 1_000)
            .await
            .expect("ok");
        assert!(matches!(d, EnqueueDecision::Enqueued { .. }));
    }

    #[tokio::test]
    async fn second_enqueue_idempotent() {
        let s = store();
        let cfg = ConsolidationConfig::default();
        let _ = enqueue_if_due(&*s, &cfg, "s1", 10, 0, 1_000)
            .await
            .expect("ok");
        let d = enqueue_if_due(&*s, &cfg, "s1", 10, 0, 1_000)
            .await
            .expect("ok");
        assert!(matches!(d, EnqueueDecision::Enqueued { .. }));
    }

    #[tokio::test]
    async fn disabled_returns_disabled() {
        let s = store();
        let cfg = ConsolidationConfig {
            enabled: false,
            ..ConsolidationConfig::default()
        };
        let d = enqueue_if_due(&*s, &cfg, "s1", 10, 0, 1_000)
            .await
            .expect("ok");
        assert_eq!(d, EnqueueDecision::Disabled);
    }
}
