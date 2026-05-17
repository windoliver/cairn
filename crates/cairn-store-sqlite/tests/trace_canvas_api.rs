//! Public store helpers for the task-trace canvas substrate.

use cairn_store_sqlite::{
    TraceCanvasDraft, TraceCanvasNodeDraft, TraceCanvasStatus, TraceStepDraft, open_in_memory,
};

#[tokio::test]
async fn trace_step_upsert_is_replay_idempotent_and_result_ref_lookupable() {
    let store = open_in_memory().await.expect("open");

    let first = store
        .upsert_trace_step(TraceStepDraft {
            step_id: "step-1".into(),
            trace_id: "trace-1".into(),
            session_id: "session-1".into(),
            turn_id: "turn-1".into(),
            tool_call_id: Some("tool-call-1".into()),
            timestamp_ms: 100,
            tool_name: Some("shell".into()),
            call_summary: "run check".into(),
            result_summary: "check passed".into(),
            result_ref: Some("record-1".into()),
            salience: 0.7,
            replaceability_score: 0.2,
            node_id: None,
            source_hash: "source-hash-1".into(),
        })
        .await
        .expect("insert first step");

    let replay = store
        .upsert_trace_step(TraceStepDraft {
            step_id: "step-2".into(),
            trace_id: "trace-1".into(),
            session_id: "session-1".into(),
            turn_id: "turn-1".into(),
            tool_call_id: Some("tool-call-1".into()),
            timestamp_ms: 200,
            tool_name: Some("shell".into()),
            call_summary: "run check again".into(),
            result_summary: "duplicate".into(),
            result_ref: Some("record-1".into()),
            salience: 0.1,
            replaceability_score: 0.9,
            node_id: None,
            source_hash: "source-hash-1".into(),
        })
        .await
        .expect("replay step");

    assert!(first.inserted);
    assert!(!replay.inserted);
    assert_eq!(replay.row.step_id, "step-1");
    assert_eq!(replay.row.call_summary, "run check");

    let hits = store
        .find_trace_steps_by_result_ref("record-1")
        .await
        .expect("lookup by result ref");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].step_id, "step-1");
}

#[tokio::test]
async fn active_trace_canvas_context_returns_nodes_in_timestamp_order() {
    let store = open_in_memory().await.expect("open");

    store
        .upsert_trace_canvas(TraceCanvasDraft {
            canvas_id: "canvas-1".into(),
            session_id: "session-1".into(),
            title: "Ship issue".into(),
            goal: "finish the current task".into(),
            status: TraceCanvasStatus::Active,
            summary: "working summary".into(),
            active_node_id: None,
            max_bytes: 4096,
        })
        .await
        .expect("insert canvas");

    store
        .upsert_trace_canvas_node(TraceCanvasNodeDraft {
            node_id: "node-2".into(),
            canvas_id: "canvas-1".into(),
            label: "Verify".into(),
            status: "planned".into(),
            summary: "verify the work".into(),
            timestamp_ms: 20,
            source_step_ids: vec!["step-2".into()],
            evidence_record_ids: Vec::new(),
        })
        .await
        .expect("insert later node");

    store
        .upsert_trace_canvas_node(TraceCanvasNodeDraft {
            node_id: "node-1".into(),
            canvas_id: "canvas-1".into(),
            label: "Implement".into(),
            status: "active".into(),
            summary: "implement the slice".into(),
            timestamp_ms: 10,
            source_step_ids: vec!["step-1".into()],
            evidence_record_ids: vec!["record-1".into()],
        })
        .await
        .expect("insert earlier node");

    let context = store
        .active_trace_canvas_for_session("session-1")
        .await
        .expect("load active context")
        .expect("context exists");

    assert_eq!(context.canvas.canvas_id, "canvas-1");
    assert_eq!(context.nodes.len(), 2);
    assert_eq!(context.nodes[0].node_id, "node-1");
    assert_eq!(context.nodes[1].node_id, "node-2");
}
