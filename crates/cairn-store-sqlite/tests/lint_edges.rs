// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use cairn_core::domain::{LintKind, Severity};
use cairn_store_sqlite::{lint_edges, migrate, resolve_edge_contradictions};
use rusqlite::{Connection, params};

fn conn() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory sqlite");
    migrate(&conn).expect("migration succeeds");
    conn
}

#[allow(clippy::too_many_arguments)]
fn insert_edge(
    conn: &Connection,
    id: &str,
    source: &str,
    target: &str,
    relation: &str,
    valid_at: i64,
    invalid_at: Option<i64>,
    expired_at: Option<i64>,
    confidence: &str,
    score: f64,
) {
    conn.execute(
        "INSERT INTO entity_edges (
            id, source_id, target_id, relation, valid_at, invalid_at,
            expired_at, confidence, confidence_score, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id, source, target, relation, valid_at, invalid_at, expired_at, confidence, score,
            valid_at
        ],
    )
    .expect("insert edge");
}

#[test]
fn read_only_lint_surfaces_one_contradiction_for_two_live_edges() {
    let conn = conn();
    insert_edge(
        &conn,
        "edge-a",
        "AuthService",
        "OAuthFlow",
        "implements",
        1,
        None,
        None,
        "INFERRED",
        0.7,
    );
    insert_edge(
        &conn,
        "edge-b",
        "AuthService",
        "OAuthFlow",
        "implements",
        2,
        None,
        None,
        "EXTRACTED",
        1.0,
    );

    let report = lint_edges(&conn).expect("lint report");

    assert_eq!(report.contradictions, 1);
    assert_eq!(report.auto_resolved, 0);
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].kind, LintKind::ContradictoryEdge);
    assert_eq!(report.findings[0].severity, Severity::Warning);
    assert_eq!(report.findings[0].entities, ["edge-a", "edge-b"]);
}

#[test]
fn read_only_lint_ignores_expired_and_invalidated_edges() {
    let conn = conn();
    insert_edge(
        &conn,
        "edge-a",
        "A",
        "B",
        "relates",
        1,
        None,
        None,
        "EXTRACTED",
        1.0,
    );
    insert_edge(
        &conn,
        "edge-b",
        "A",
        "B",
        "relates",
        2,
        Some(10),
        None,
        "EXTRACTED",
        1.0,
    );
    insert_edge(
        &conn,
        "edge-c",
        "A",
        "B",
        "relates",
        3,
        None,
        Some(11),
        "EXTRACTED",
        1.0,
    );

    let report = lint_edges(&conn).expect("lint report");

    assert_eq!(report.contradictions, 0);
    assert!(report.findings.is_empty());
}

#[test]
fn read_only_lint_surfaces_ambiguous_live_edges_as_info() {
    let conn = conn();
    insert_edge(
        &conn,
        "edge-a",
        "A",
        "B",
        "relates",
        1,
        None,
        None,
        "AMBIGUOUS",
        0.2,
    );

    let report = lint_edges(&conn).expect("lint report");

    assert_eq!(report.ambiguous_edges, 1);
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].kind, LintKind::AmbiguousEdge);
    assert_eq!(report.findings[0].severity, Severity::Info);
    assert_eq!(report.findings[0].entities, ["edge-a"]);
}

#[test]
fn read_only_lint_does_not_mutate_edges() {
    let conn = conn();
    insert_edge(
        &conn, "edge-a", "A", "B", "relates", 1, None, None, "INFERRED", 0.7,
    );
    insert_edge(
        &conn,
        "edge-b",
        "A",
        "B",
        "relates",
        2,
        None,
        None,
        "EXTRACTED",
        1.0,
    );

    let before: Vec<Option<i64>> = conn
        .prepare("SELECT invalid_at FROM entity_edges ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    let _ = lint_edges(&conn).expect("lint report");

    let after: Vec<Option<i64>> = conn
        .prepare("SELECT invalid_at FROM entity_edges ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(before, after);
}

#[test]
fn fix_keeps_one_live_edge_and_records_wal_reason() {
    let mut conn = conn();
    insert_edge(
        &conn, "edge-a", "A", "B", "relates", 1, None, None, "INFERRED", 0.7,
    );
    insert_edge(
        &conn,
        "edge-b",
        "A",
        "B",
        "relates",
        2,
        None,
        None,
        "EXTRACTED",
        1.0,
    );
    insert_edge(
        &conn,
        "edge-c",
        "A",
        "B",
        "relates",
        3,
        None,
        None,
        "AMBIGUOUS",
        0.1,
    );

    let report = resolve_edge_contradictions(&mut conn, 42, "01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .expect("fix report");

    assert_eq!(report.auto_resolved, 2);

    let live_ids: Vec<String> = conn
        .prepare(
            "SELECT id FROM entity_edges
             WHERE invalid_at IS NULL AND expired_at IS NULL
             ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(live_ids, ["edge-b"]);

    let edge_temporal_state: Vec<(String, Option<i64>, Option<i64>)> = conn
        .prepare("SELECT id, invalid_at, expired_at FROM entity_edges ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        edge_temporal_state,
        [
            ("edge-a".to_owned(), Some(42), None),
            ("edge-b".to_owned(), None, None),
            ("edge-c".to_owned(), Some(42), None),
        ]
    );

    let wal_reason: String = conn
        .query_row(
            "SELECT reason FROM wal_ops WHERE operation_id = ?1",
            ["01ARZ3NDEKTSV4RRFFQ69G5FAV"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(wal_reason, "lint:contradiction_resolution");

    let replay_reason: String = conn
        .query_row(
            "SELECT reason FROM replay_ledger WHERE operation_id = ?1",
            ["01ARZ3NDEKTSV4RRFFQ69G5FAV"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(replay_reason, "lint:contradiction_resolution");
}

#[test]
fn fix_noops_without_contradictions_and_writes_no_wal_row() {
    let mut conn = conn();
    insert_edge(
        &conn,
        "edge-a",
        "A",
        "B",
        "relates",
        1,
        None,
        None,
        "EXTRACTED",
        1.0,
    );

    let report = resolve_edge_contradictions(&mut conn, 42, "01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .expect("fix report");

    assert_eq!(report.auto_resolved, 0);
    let wal_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM wal_ops", [], |row| row.get(0))
        .unwrap();
    assert_eq!(wal_count, 0);
}
