//! Enqueue `Skillify` workflow jobs from scheduled or hook triggers.

use cairn_core::contract::job_store::{
    EnqueueRequest, JobId, JobKind, JobStore, JobStoreError, RetryPolicy,
};
use cairn_core::domain::ScopeTuple;

use super::{SKILLIFY_KIND, SkillifyPayload, SkillifyTrigger};

/// Outcome of a skill emission enqueue attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillifyEnqueueDecision {
    /// A job was accepted or deduplicated.
    Enqueued {
        /// Stable job id used for the enqueue attempt.
        job_id: JobId,
    },
}

/// Enqueue one skill emission job.
///
/// # Errors
/// Returns backend errors from the job store. Duplicate dedupe keys are
/// idempotent success.
pub async fn enqueue_skillify(
    store: &dyn JobStore,
    trigger: SkillifyTrigger,
    key: &str,
    dedupe_token: &str,
    not_before_ms: i64,
    bound_scope: Option<&ScopeTuple>,
    source_record_ids: Vec<String>,
) -> Result<SkillifyEnqueueDecision, JobStoreError> {
    let payload = SkillifyPayload {
        trigger,
        key: key.to_owned(),
        candidate_id: None,
        bound_scope: bound_scope.cloned(),
        source_record_ids,
    };
    let bytes = payload
        .to_bytes()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    let queue_key = payload.recommended_queue_key();
    let job_id = JobId::new(format!(
        "skillify:{}:{key}:{dedupe_token}",
        trigger.as_str()
    ));
    let req = EnqueueRequest {
        job_id: job_id.clone(),
        kind: JobKind::new(SKILLIFY_KIND),
        payload: bytes,
        queue_key: Some(queue_key.clone()),
        dedupe_key: Some(format!("{queue_key}:{dedupe_token}")),
        not_before_ms,
        retry: RetryPolicy::DEFAULT,
    };

    match store.enqueue(req).await {
        Ok(()) | Err(JobStoreError::DuplicateDedupeKey { .. }) => {
            Ok(SkillifyEnqueueDecision::Enqueued { job_id })
        }
        Err(e) => Err(e),
    }
}
