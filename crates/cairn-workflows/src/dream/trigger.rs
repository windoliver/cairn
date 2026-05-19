//! Enqueue tiered `DreamWorkflow` jobs from scheduled or hook triggers.

use cairn_core::config::{DreamConfig, DreamTier};
use cairn_core::contract::job_store::{
    EnqueueRequest, JobId, JobKind, JobStore, JobStoreError, RetryPolicy,
};

use super::{DREAM_KIND, DreamPayload};

/// Result of evaluating a dream tier trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DreamEnqueueDecision {
    /// A job was accepted or deduplicated.
    Enqueued {
        /// Stable job id used for the enqueue attempt.
        job_id: JobId,
    },
    /// Dream workflow is disabled.
    Disabled,
}

/// Enqueue one tier run.
///
/// # Errors
/// Returns backend errors from the job store. Duplicate dedupe keys are
/// idempotent success.
pub async fn enqueue_tier(
    store: &dyn JobStore,
    config: &DreamConfig,
    tier: DreamTier,
    key: &str,
    now_ms: i64,
    bound_scope: Option<&cairn_core::domain::ScopeTuple>,
) -> Result<DreamEnqueueDecision, JobStoreError> {
    enqueue_tier_with_dedupe_token(
        store,
        config,
        tier,
        key,
        &now_ms.to_string(),
        now_ms,
        bound_scope,
    )
    .await
}

/// Same as [`enqueue_tier`], but lets hook-triggered callers provide a stable
/// token such as a closed-turn sequence while retaining wall-clock
/// `not_before_ms` scheduling.
///
/// # Errors
/// Returns backend errors from the job store. Duplicate dedupe keys are
/// idempotent success.
pub async fn enqueue_tier_with_dedupe_token(
    store: &dyn JobStore,
    config: &DreamConfig,
    tier: DreamTier,
    key: &str,
    dedupe_token: &str,
    not_before_ms: i64,
    bound_scope: Option<&cairn_core::domain::ScopeTuple>,
) -> Result<DreamEnqueueDecision, JobStoreError> {
    if !config.enabled {
        return Ok(DreamEnqueueDecision::Disabled);
    }

    let payload = DreamPayload {
        tier,
        key: key.to_owned(),
        bound_scope: bound_scope.cloned(),
    };
    let bytes = payload
        .to_bytes()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    let queue_key = payload.recommended_queue_key();
    let job_id = JobId::new(format!("dream:{}:{key}:{dedupe_token}", tier.as_str()));
    let req = EnqueueRequest {
        job_id: job_id.clone(),
        kind: JobKind::new(DREAM_KIND),
        payload: bytes,
        queue_key: Some(queue_key.clone()),
        dedupe_key: Some(format!("{queue_key}:{dedupe_token}")),
        not_before_ms,
        retry: RetryPolicy::DEFAULT,
    };
    match store.enqueue(req).await {
        Ok(()) | Err(JobStoreError::DuplicateDedupeKey { .. }) => {
            Ok(DreamEnqueueDecision::Enqueued { job_id })
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
    async fn disabled_returns_disabled() {
        let s = store();
        let cfg = DreamConfig::default();
        let decision = enqueue_tier(&*s, &cfg, DreamTier::LightSleep, "session", 1_000, None)
            .await
            .expect("ok");
        assert_eq!(decision, DreamEnqueueDecision::Disabled);
    }

    #[tokio::test]
    async fn enqueues_each_tier_with_distinct_job_ids() {
        let s = store();
        let cfg = DreamConfig {
            enabled: true,
            ..DreamConfig::default()
        };
        let light = enqueue_tier(&*s, &cfg, DreamTier::LightSleep, "session", 1_000, None)
            .await
            .expect("light");
        let rem = enqueue_tier(&*s, &cfg, DreamTier::RemSleep, "session", 1_000, None)
            .await
            .expect("rem");
        let deep = enqueue_tier(&*s, &cfg, DreamTier::DeepDreaming, "vault", 1_000, None)
            .await
            .expect("deep");

        assert_ne!(light, rem);
        assert_ne!(rem, deep);
        assert!(matches!(light, DreamEnqueueDecision::Enqueued { .. }));
        assert!(matches!(rem, DreamEnqueueDecision::Enqueued { .. }));
        assert!(matches!(deep, DreamEnqueueDecision::Enqueued { .. }));
    }

    #[tokio::test]
    async fn second_enqueue_is_idempotent() {
        let s = store();
        let cfg = DreamConfig {
            enabled: true,
            ..DreamConfig::default()
        };
        let first = enqueue_tier(&*s, &cfg, DreamTier::RemSleep, "session", 1_000, None)
            .await
            .expect("first");
        let second = enqueue_tier(&*s, &cfg, DreamTier::RemSleep, "session", 1_000, None)
            .await
            .expect("second");
        assert_eq!(first, second);
    }
}
