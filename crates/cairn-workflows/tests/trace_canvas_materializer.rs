//! Workflow-facing projection helper for task-trace canvases.

use std::sync::Arc;

use cairn_store_sqlite::{TraceCanvasEdgeKind, TraceStepDraft, open_in_memory};
use cairn_workflows::{
    TRACE_CANVAS_KIND, TraceCanvasHandler, TraceCanvasMaterializer, TraceCanvasPayload,
    TraceCanvasProjection,
    scheduler::{HandlerOutcome, JobHandler},
};

#[tokio::test]
async fn materializer_creates_active_canvas_node_and_step() {
    let store = Arc::new(open_in_memory().await.expect("open"));
    let materializer = TraceCanvasMaterializer::new(Arc::clone(&store));

    let context = materializer
        .project_step(TraceCanvasProjection {
            step: step("step-1", "turn-1", 100, Some("record-1")),
            canvas_title: "Current task".into(),
            canvas_goal: "finish issue 134".into(),
            node_label: "Run check".into(),
            node_status: "completed".into(),
            node_summary: "The focused check passed.".into(),
        })
        .await
        .expect("project step");

    assert_eq!(context.canvas.canvas_id, "trace-canvas:session-1:active");
    assert_eq!(
        context.canvas.active_node_id.as_deref(),
        Some("trace-node:step-1")
    );
    assert_eq!(context.nodes.len(), 1);
    assert_eq!(context.nodes[0].source_step_ids, vec!["step-1"]);
    assert_eq!(context.nodes[0].evidence_record_ids, vec!["record-1"]);
    assert!(context.edges.is_empty());

    let steps = store
        .find_trace_steps_by_result_ref("record-1")
        .await
        .expect("lookup result ref");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].step_id, "step-1");
}

#[tokio::test]
async fn materializer_links_previous_active_node_to_new_node() {
    let store = Arc::new(open_in_memory().await.expect("open"));
    let materializer = TraceCanvasMaterializer::new(Arc::clone(&store));

    materializer
        .project_step(TraceCanvasProjection {
            step: step("step-1", "turn-1", 100, None),
            canvas_title: "Current task".into(),
            canvas_goal: "finish issue 134".into(),
            node_label: "Implement".into(),
            node_status: "completed".into(),
            node_summary: "Implementation landed.".into(),
        })
        .await
        .expect("project first step");

    let context = materializer
        .project_step(TraceCanvasProjection {
            step: step("step-2", "turn-2", 200, Some("record-2")),
            canvas_title: "Current task".into(),
            canvas_goal: "finish issue 134".into(),
            node_label: "Verify".into(),
            node_status: "active".into(),
            node_summary: "Verification is running.".into(),
        })
        .await
        .expect("project second step");

    assert_eq!(
        context.canvas.active_node_id.as_deref(),
        Some("trace-node:step-2")
    );
    assert_eq!(context.nodes.len(), 2);
    assert_eq!(context.edges.len(), 1);
    assert_eq!(context.edges[0].from_node_id, "trace-node:step-1");
    assert_eq!(context.edges[0].to_node_id, "trace-node:step-2");
    assert_eq!(context.edges[0].kind, TraceCanvasEdgeKind::DependsOn);
}

#[tokio::test]
async fn handler_decodes_payload_and_materializes_context() {
    let store = Arc::new(open_in_memory().await.expect("open"));
    let handler = TraceCanvasHandler::new(Arc::clone(&store));
    assert_eq!(
        handler.kind(),
        cairn_core::contract::job_store::JobKind::new(TRACE_CANVAS_KIND)
    );

    let payload = TraceCanvasPayload {
        projection: TraceCanvasProjection {
            step: step("step-1", "turn-1", 100, Some("record-1")),
            canvas_title: "Current task".into(),
            canvas_goal: "finish issue 134".into(),
            node_label: "Run check".into(),
            node_status: "completed".into(),
            node_summary: "The focused check passed.".into(),
        },
    };

    let outcome = handler
        .handle(&payload.to_bytes().expect("encode payload"))
        .await;
    assert_eq!(outcome, HandlerOutcome::Done);

    let context = store
        .active_trace_canvas_for_session("session-1")
        .await
        .expect("load context")
        .expect("context exists");
    assert_eq!(
        context.canvas.active_node_id.as_deref(),
        Some("trace-node:step-1")
    );
}

fn step(
    step_id: &str,
    turn_id: &str,
    timestamp_ms: i64,
    result_ref: Option<&str>,
) -> TraceStepDraft {
    TraceStepDraft {
        step_id: step_id.into(),
        trace_id: "trace-1".into(),
        session_id: "session-1".into(),
        turn_id: turn_id.into(),
        tool_call_id: Some(format!("tool-call-{step_id}")),
        timestamp_ms,
        tool_name: Some("shell".into()),
        call_summary: "run command".into(),
        result_summary: "command completed".into(),
        result_ref: result_ref.map(Into::into),
        salience: 0.5,
        replaceability_score: 0.5,
        node_id: None,
        source_hash: format!("source-hash-{step_id}"),
    }
}
