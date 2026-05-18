//! Workflow-facing task-trace canvas projection helper.
//!
//! This module is intentionally narrower than a scheduler handler: it provides
//! the deterministic store mutation primitive that a capture/workflow path can
//! call once jobs are wired.

use std::sync::Arc;

use cairn_core::contract::job_store::{
    EnqueueRequest, FailureClass, JobId, JobKind, JobPayload, JobStore, JobStoreError, RetryPolicy,
};
use cairn_store_sqlite::{
    SqliteMemoryStore, StoreError, TraceCanvasContext, TraceCanvasDraft, TraceCanvasEdgeDraft,
    TraceCanvasEdgeKind, TraceCanvasNodeDraft, TraceCanvasStatus, TraceStepDraft,
};
use serde::{Deserialize, Serialize};

use crate::scheduler::{HandlerOutcome, JobHandler};

/// The `JobKind` discriminator stored in `workflow_jobs.kind`.
pub const TRACE_CANVAS_KIND: &str = "trace_canvas.materialize_step";

/// Deterministic projection input for one trace step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceCanvasProjection {
    /// Trace step to persist.
    pub step: TraceStepDraft,
    /// Active canvas title to create or refresh.
    pub canvas_title: String,
    /// Active canvas goal to create or refresh.
    pub canvas_goal: String,
    /// Projected node label.
    pub node_label: String,
    /// Projected node status. Must satisfy the store schema domain.
    pub node_status: String,
    /// Projected node summary.
    pub node_summary: String,
}

/// One enqueued trace-canvas materialization request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceCanvasPayload {
    /// Projection to apply.
    pub projection: TraceCanvasProjection,
}

/// Outcome of a trace-canvas materialization enqueue attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceCanvasEnqueueDecision {
    /// A materialization job was accepted, or an existing matching job
    /// already covers the same step.
    Enqueued {
        /// Stable job id for the materialization request.
        job_id: JobId,
    },
}

impl TraceCanvasPayload {
    /// Recommended `EnqueueRequest::queue_key` for this payload.
    #[must_use]
    pub fn recommended_queue_key(&self) -> String {
        format!("trace-canvas:{}", self.projection.step.session_id)
    }

    /// Serialize to `JobPayload`.
    ///
    /// # Errors
    /// JSON encoding failure (effectively unreachable for this struct).
    pub fn to_bytes(&self) -> Result<JobPayload, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Deserialize from `JobPayload`.
    ///
    /// # Errors
    /// JSON decoding failure.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

/// Enqueue one trace-step materialization job.
///
/// The request is idempotent on `step_id`: replaying a capture batch will hit
/// the job-store dedupe constraint and report the same [`TraceCanvasEnqueueDecision`].
///
/// # Errors
/// Returns [`JobStoreError::Backend`] on store I/O or payload encoding failures.
pub async fn enqueue_trace_canvas_step(
    store: &dyn JobStore,
    payload: TraceCanvasPayload,
    now_ms: i64,
) -> Result<TraceCanvasEnqueueDecision, JobStoreError> {
    let job_id = JobId::new(format!("trace-canvas:{}", payload.projection.step.step_id));
    let dedupe_key = payload.projection.step.step_id.clone();
    let req = EnqueueRequest {
        job_id: job_id.clone(),
        kind: JobKind::new(TRACE_CANVAS_KIND),
        payload: payload
            .to_bytes()
            .map_err(|e| JobStoreError::Backend(e.to_string()))?,
        queue_key: Some(payload.recommended_queue_key()),
        dedupe_key: Some(dedupe_key),
        not_before_ms: now_ms,
        retry: RetryPolicy::DEFAULT,
    };

    match store.enqueue(req).await {
        Ok(()) | Err(JobStoreError::DuplicateDedupeKey { .. }) => {
            Ok(TraceCanvasEnqueueDecision::Enqueued { job_id })
        }
        Err(err) => Err(err),
    }
}

/// Store-backed task-trace canvas materializer.
pub struct TraceCanvasMaterializer {
    store: Arc<SqliteMemoryStore>,
}

/// Scheduler handler for task-trace canvas materialization jobs.
pub struct TraceCanvasHandler {
    materializer: TraceCanvasMaterializer,
}

impl TraceCanvasHandler {
    /// Create a handler backed by `store`.
    #[must_use]
    pub fn new(store: Arc<SqliteMemoryStore>) -> Self {
        Self {
            materializer: TraceCanvasMaterializer::new(store),
        }
    }
}

#[async_trait::async_trait]
impl JobHandler for TraceCanvasHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(TRACE_CANVAS_KIND)
    }

    async fn handle(&self, payload_bytes: &JobPayload) -> HandlerOutcome {
        let payload = match TraceCanvasPayload::from_bytes(payload_bytes) {
            Ok(payload) => payload,
            Err(err) => {
                return HandlerOutcome::Permanent {
                    reason: format!("trace canvas payload decode failed: {err}"),
                    class: FailureClass::Validation,
                };
            }
        };

        match self.materializer.project_step(payload.projection).await {
            Ok(_) => HandlerOutcome::Done,
            Err(err) => HandlerOutcome::Retry {
                reason: err.to_string(),
                class: FailureClass::Transient,
            },
        }
    }
}

impl TraceCanvasMaterializer {
    /// Create a materializer backed by `store`.
    #[must_use]
    pub fn new(store: Arc<SqliteMemoryStore>) -> Self {
        Self { store }
    }

    /// Project one trace step into the session's active task canvas.
    ///
    /// The method is idempotent for the trace step itself through
    /// `SqliteMemoryStore::upsert_trace_step`. It refreshes the active canvas
    /// row, upserts the projected node, and links the previous active node to
    /// the new node with a `depends_on` edge when they differ.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for store failures or schema-domain violations.
    pub async fn project_step(
        &self,
        projection: TraceCanvasProjection,
    ) -> Result<TraceCanvasContext, StoreError> {
        let previous = self
            .store
            .active_trace_canvas_for_session(&projection.step.session_id)
            .await?;
        let previous_active_node = previous.and_then(|context| context.canvas.active_node_id);

        let canvas_id = active_canvas_id(&projection.step.session_id);
        let node_id = trace_node_id(&projection.step.step_id);
        let result_ref = projection.step.result_ref.clone();
        let step_id = projection.step.step_id.clone();
        let session_id = projection.step.session_id.clone();
        let context_session_id = session_id.clone();
        let timestamp_ms = projection.step.timestamp_ms;
        let canvas_title = projection.canvas_title;
        let canvas_goal = projection.canvas_goal;

        self.store
            .upsert_trace_canvas(TraceCanvasDraft {
                canvas_id: canvas_id.clone(),
                session_id: session_id.clone(),
                title: canvas_title.clone(),
                goal: canvas_goal.clone(),
                status: TraceCanvasStatus::Active,
                summary: "Active task trace canvas.".to_owned(),
                active_node_id: previous_active_node.clone(),
                max_bytes: 8192,
            })
            .await?;

        self.store.upsert_trace_step(projection.step).await?;
        self.store
            .upsert_trace_canvas_node(TraceCanvasNodeDraft {
                node_id: node_id.clone(),
                canvas_id: canvas_id.clone(),
                label: projection.node_label,
                status: projection.node_status,
                summary: projection.node_summary,
                timestamp_ms,
                source_step_ids: vec![step_id],
                evidence_record_ids: result_ref.into_iter().collect(),
            })
            .await?;

        self.store
            .upsert_trace_canvas(TraceCanvasDraft {
                canvas_id: canvas_id.clone(),
                session_id,
                title: canvas_title,
                goal: canvas_goal,
                status: TraceCanvasStatus::Active,
                summary: "Active task trace canvas.".to_owned(),
                active_node_id: Some(node_id.clone()),
                max_bytes: 8192,
            })
            .await?;

        if let Some(previous_node_id) = previous_active_node
            && previous_node_id != node_id
        {
            self.store
                .upsert_trace_canvas_edge(TraceCanvasEdgeDraft {
                    canvas_id: canvas_id.clone(),
                    from_node_id: previous_node_id,
                    to_node_id: node_id,
                    kind: TraceCanvasEdgeKind::DependsOn,
                    label: None,
                })
                .await?;
        }

        self.store
            .refresh_trace_canvas_projection(&canvas_id)
            .await?;

        self.store
            .active_trace_canvas_for_session(&context_session_id)
            .await?
            .ok_or_else(|| StoreError::Invariant {
                what: "trace canvas projection did not leave an active context".into(),
            })
    }
}

fn active_canvas_id(session_id: &str) -> String {
    format!("trace-canvas:{session_id}:active")
}

fn trace_node_id(step_id: &str) -> String {
    format!("trace-node:{step_id}")
}
