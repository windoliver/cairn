// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use cairn_core::generated::verbs::lint::{Kind, Severity};
use cairn_store_sqlite::{lint_edges, resolve_edge_contradictions};
use rusqlite::{Connection, params};

fn conn() -> Connection {
    let conn = cairn_store_sqlite::open_in_memory_sync().expect("open in-memory store");
    seed_nodes(&conn);
    conn
}

fn seed_nodes(conn: &Connection) {
    conn.execute_batch(
        "INSERT OR IGNORE INTO entity_nodes (id, name, name_norm, created_at) VALUES
           ('A', 'A', 'a', 1),
           ('B', 'B', 'b', 1),
           ('AuthService', 'AuthService', 'authservice', 1),
           ('OAuthFlow', 'OAuthFlow', 'oauthflow', 1);",
    )
    .expect("seed nodes");
}

fn allow_corrupt_overlaps(conn: &Connection) {
    conn.execute_batch(
        "DROP INDEX IF EXISTS entity_edges_live_triple;
         DROP TRIGGER IF EXISTS entity_edges_no_overlap_insert;
         DROP TRIGGER IF EXISTS entity_edges_no_overlap_update;",
    )
    .expect("drop overlap guards for corruption fixture");
}

fn insert_record_stub(conn: &Connection, record_id: &str, target_id: &str) {
    let record_json = serde_json::json!({
        "id": record_id,
        "target_id": target_id,
        "body": "record link review fixture",
        "extra_frontmatter": {}
    })
    .to_string();
    conn.execute(
        "INSERT INTO records (
            record_id, target_id, version, path, kind, class, visibility,
            scope, actor_chain, body, body_hash, created_at, updated_at,
            active, tombstoned, is_static, record_json, confidence, salience,
            tags_json
         ) VALUES (
            ?1, ?2, 1, 'raw/link-review.md', 'note', 'episodic', 'session',
            '{}', '[]', 'record link review fixture', 'sha256:lint', 1, 1,
            1, 0, 0, ?3, 0.5, 0.5, '[]'
         )",
        params![record_id, target_id, record_json],
    )
    .expect("insert record stub");
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
    score: f32,
) {
    conn.execute(
        "INSERT INTO entity_edges (
            id, source_id, target_id, relation, valid_at, invalid_at,
            expired_at, tombstone_reason, confidence, confidence_score,
            created_at, body_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id,
            source,
            target,
            relation,
            valid_at,
            invalid_at,
            expired_at,
            expired_at.map(|_| "test tombstone"),
            confidence,
            score,
            valid_at,
            vec![id.as_bytes().first().copied().unwrap_or_default(); 32],
        ],
    )
    .expect("insert edge");
}

#[test]
fn read_only_lint_surfaces_record_link_review_conflicts() {
    let conn = conn();
    insert_record_stub(
        &conn,
        "01J00000000000000000001901",
        "01HQZX9F5N0000000000019001",
    );
    conn.execute(
        "INSERT INTO record_link_review (
            record_id, reason, scope_session_id, trace_session_id, detail_json, created_at
         ) VALUES (
            '01J00000000000000000001901', 'session_id_mismatch',
            'scope-session', 'trace-session',
            '{\"migration_id\":64}', 109
         )",
        [],
    )
    .expect("insert review row");

    let report = lint_edges(&conn).expect("lint report");

    let finding = report
        .findings
        .iter()
        .find(|finding| finding.tracking_issue == Some(109))
        .expect("issue 109 lint finding");
    assert_eq!(finding.kind, Kind::DeferredCheck);
    assert_eq!(finding.severity, Severity::Warning);
    assert_eq!(
        finding.entities.as_deref(),
        Some(&["01J00000000000000000001901".to_owned()][..])
    );
    assert!(
        finding.message.contains("scope-session")
            && finding.message.contains("trace-session")
            && finding.message.contains("manual review")
    );
}

#[test]
fn read_only_lint_surfaces_one_contradiction_for_two_live_edges() {
    let conn = conn();
    allow_corrupt_overlaps(&conn);
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
    assert_eq!(report.findings[0].kind, Kind::ContradictoryEdge);
    assert_eq!(report.findings[0].severity, Severity::Warning);
    assert_eq!(
        report.findings[0].entities.as_deref(),
        Some(&["edge-a".to_owned(), "edge-b".to_owned()][..])
    );
}

#[test]
fn read_only_lint_ignores_expired_and_invalidated_edges() {
    let conn = conn();
    allow_corrupt_overlaps(&conn);
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
    assert_eq!(report.findings[0].kind, Kind::AmbiguousEdge);
    assert_eq!(report.findings[0].severity, Severity::Info);
    assert_eq!(
        report.findings[0].entities.as_deref(),
        Some(&["edge-a".to_owned()][..])
    );
}

#[test]
fn read_only_lint_does_not_mutate_edges() {
    let conn = conn();
    allow_corrupt_overlaps(&conn);
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
    allow_corrupt_overlaps(&conn);
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

    let wal_entry: (String, String, String) = conn
        .query_row(
            "SELECT state, kind, reason FROM wal_ops WHERE operation_id = ?1",
            ["01ARZ3NDEKTSV4RRFFQ69G5FAV"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        wal_entry,
        (
            "COMMITTED".to_owned(),
            "graph_contradict".to_owned(),
            "lint:contradiction_resolution".to_owned(),
        )
    );

    let step_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wal_steps WHERE operation_id = ?1 AND step_kind = 'invalidate_edge'",
            ["01ARZ3NDEKTSV4RRFFQ69G5FAV"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(step_count, 2);
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
