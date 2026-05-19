//! Schema-level guarantees for the task-trace canvas substrate.
//!
//! Issue #134's trace-canvas addendum extends the existing `capture_trace`
//! record projection (brief §5.7, §7, §10) with durable step and graph
//! tables. These tests pin the storage shape before workflow materialization
//! starts depending on it.

use cairn_store_sqlite::open_in_memory_sync as open_in_memory;
use rusqlite::params;

#[test]
fn trace_canvas_tables_are_created() {
    let conn = open_in_memory().expect("open");

    for table in [
        "trace_steps",
        "trace_canvases",
        "trace_canvas_nodes",
        "trace_canvas_edges",
    ] {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_schema
                     WHERE type = 'table' AND name = ?1
                )",
                [table],
                |row| row.get(0),
            )
            .expect("query sqlite_schema");
        assert!(exists, "missing table {table}");
    }
}

#[test]
fn trace_steps_are_idempotent_by_source_hash_and_tool_call() {
    let conn = open_in_memory().expect("open");
    insert_trace_step(&conn, "step-1", Some("tool-call-1")).expect("insert first step");

    let err = insert_trace_step(&conn, "step-2", Some("tool-call-1")).unwrap_err();
    assert!(
        format!("{err}").contains("UNIQUE"),
        "expected duplicate source/tool key rejection, got: {err}"
    );
}

#[test]
fn trace_canvas_nodes_require_source_steps() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO trace_canvases
            (canvas_id, session_id, title, goal, status, summary, created_at_ms, updated_at_ms)
         VALUES
            ('canvas-1', 'session-1', 'Plan', 'Finish task', 'active', 'summary', 1, 1)",
        [],
    )
    .expect("insert canvas");

    let err = conn
        .execute(
            "INSERT INTO trace_canvas_nodes
                (node_id, canvas_id, label, status, summary, timestamp_ms,
                 source_step_ids, evidence_record_ids)
             VALUES
                ('node-1', 'canvas-1', 'Node', 'active', 'summary', 1, '[]', '[]')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("CHECK"),
        "expected empty source-step rejection, got: {err}"
    );
}

#[test]
fn trace_canvas_edges_reject_unknown_kind() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO trace_canvases
            (canvas_id, session_id, title, goal, status, summary, created_at_ms, updated_at_ms)
         VALUES
            ('canvas-1', 'session-1', 'Plan', 'Finish task', 'active', 'summary', 1, 1)",
        [],
    )
    .expect("insert canvas");
    insert_canvas_node(&conn, "node-1", "canvas-1").expect("insert node 1");
    insert_canvas_node(&conn, "node-2", "canvas-1").expect("insert node 2");

    let err = conn
        .execute(
            "INSERT INTO trace_canvas_edges
                (canvas_id, from_node_id, to_node_id, kind, label)
             VALUES
                ('canvas-1', 'node-1', 'node-2', 'mystery', NULL)",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("CHECK"),
        "expected edge-kind rejection, got: {err}"
    );
}

fn insert_trace_step(
    conn: &rusqlite::Connection,
    step_id: &str,
    tool_call_id: Option<&str>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO trace_steps
            (step_id, trace_id, session_id, turn_id, tool_call_id, timestamp_ms,
             tool_name, call_summary, result_summary, result_ref, salience,
             replaceability_score, node_id, source_hash, created_at_ms, updated_at_ms)
         VALUES
            (?1, 'trace-1', 'session-1', 'turn-1', ?2, 1,
             'shell', 'call', 'result', 'record-1', 0.5, 0.5, NULL,
             'source-hash-1', 1, 1)",
        params![step_id, tool_call_id],
    )
}

fn insert_canvas_node(
    conn: &rusqlite::Connection,
    node_id: &str,
    canvas_id: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO trace_canvas_nodes
            (node_id, canvas_id, label, status, summary, timestamp_ms,
             source_step_ids, evidence_record_ids)
         VALUES
            (?1, ?2, 'Node', 'active', 'summary', 1, '[\"step-1\"]', '[]')",
        params![node_id, canvas_id],
    )
}
