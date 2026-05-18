//! Workflow-facing task-trace canvas projection helper.
//!
//! This module is intentionally narrower than a scheduler handler: it provides
//! the deterministic store mutation primitive that a capture/workflow path can
//! call once jobs are wired.

use std::sync::Arc;

use cairn_store_sqlite::{
    SqliteMemoryStore, StoreError, TraceCanvasContext, TraceCanvasDraft, TraceCanvasEdgeDraft,
    TraceCanvasEdgeKind, TraceCanvasNodeDraft, TraceCanvasStatus, TraceStepDraft,
};

/// Deterministic projection input for one trace step.
#[derive(Debug, Clone)]
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

/// Store-backed task-trace canvas materializer.
pub struct TraceCanvasMaterializer {
    store: Arc<SqliteMemoryStore>,
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
                    canvas_id,
                    from_node_id: previous_node_id,
                    to_node_id: node_id,
                    kind: TraceCanvasEdgeKind::DependsOn,
                    label: None,
                })
                .await?;
        }

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
