// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::process::Command;

use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::TempDir;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn vault_with_contradictory_edges() -> TempDir {
    let vault = tempfile::tempdir().expect("temp vault");
    let cairn_dir = vault.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn dir");

    cairn_store_sqlite::vec_ext::register_vec0();
    let mut conn = Connection::open(cairn_dir.join("cairn.db")).expect("open cairn db");
    cairn_store_sqlite::migrations::migrations()
        .to_latest(&mut conn)
        .expect("migrate cairn db");
    seed_nodes(&conn);
    allow_corrupt_overlaps(&conn);
    insert_edge(&conn, "edge-a", "INFERRED", 0.7);
    insert_edge(&conn, "edge-b", "EXTRACTED", 1.0);

    vault
}

fn seed_nodes(conn: &Connection) {
    conn.execute_batch(
        "INSERT OR IGNORE INTO entity_nodes (id, name, name_norm, created_at) VALUES
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

fn insert_edge(conn: &Connection, id: &str, confidence: &str, confidence_score: f32) {
    conn.execute(
        "INSERT INTO entity_edges (
            id, source_id, target_id, relation, valid_at, invalid_at,
            expired_at, confidence, confidence_score, created_at, body_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            id,
            "AuthService",
            "OAuthFlow",
            "implements",
            1_i64,
            Option::<i64>::None,
            Option::<i64>::None,
            confidence,
            confidence_score,
            1_i64,
            vec![id.as_bytes().first().copied().unwrap_or_default(); 32],
        ],
    )
    .expect("insert edge");
}

fn parse_stdout_json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "expected JSON stdout: {err}; stdout: {:?}; stderr: {:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn live_edge_ids(vault: &TempDir) -> Vec<String> {
    let conn = Connection::open(vault.path().join(".cairn/cairn.db")).expect("open cairn db");
    let mut stmt = conn
        .prepare(
            "SELECT id FROM entity_edges
             WHERE invalid_at IS NULL AND expired_at IS NULL
             ORDER BY id",
        )
        .expect("prepare live edge query");
    stmt.query_map([], |row| row.get(0))
        .expect("query live edges")
        .collect::<Result<_, _>>()
        .expect("collect live edges")
}

fn empty_db_without_lint_schema() -> TempDir {
    let vault = tempfile::tempdir().expect("temp vault");
    let cairn_dir = vault.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn dir");
    let _conn = Connection::open(cairn_dir.join("cairn.db")).expect("open empty cairn db");
    vault
}

#[test]
fn lint_json_reports_contradiction_without_mutating_live_edges() {
    let vault = vault_with_contradictory_edges();

    let out = cli()
        .current_dir(vault.path())
        .args(["lint", "--json"])
        .output()
        .expect("cairn lint --json");

    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let json = parse_stdout_json(&out);
    assert_eq!(json["contract"], "cairn.mcp.v1");
    assert_eq!(json["status"], "committed");
    assert_eq!(json["verb"], "lint");
    assert!(json.get("error").is_none(), "error: {:?}", json["error"]);
    assert_eq!(json["data"]["summary"]["by_kind"]["contradictory_edge"], 1);
    assert_eq!(json["data"]["summary"]["auto_resolved"], 0);
    assert_eq!(json["data"]["summary"]["total"], 1);
    assert_eq!(json["data"]["findings"][0]["kind"], "contradictory_edge");
    assert_eq!(json["data"]["findings"][0]["severity"], "warning");
    assert_eq!(
        json["data"]["findings"][0]["entities"],
        serde_json::json!(["edge-a", "edge-b"])
    );
    assert_eq!(live_edge_ids(&vault), ["edge-a", "edge-b"]);
}

#[test]
fn lint_fix_json_resolves_lower_confidence_duplicate_edge() {
    let vault = vault_with_contradictory_edges();

    let out = cli()
        .current_dir(vault.path())
        .args(["lint", "--fix", "--json"])
        .output()
        .expect("cairn lint --fix --json");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let json = parse_stdout_json(&out);
    assert_eq!(json["contract"], "cairn.mcp.v1");
    assert_eq!(json["status"], "committed");
    assert_eq!(json["verb"], "lint");
    assert!(json.get("error").is_none(), "error: {:?}", json["error"]);
    assert_eq!(
        json["data"]["summary"]["by_kind"]["contradictory_edge"],
        Value::Null
    );
    assert_eq!(json["data"]["summary"]["auto_resolved"], 1);
    assert_eq!(json["data"]["summary"]["total"], 0);
    assert_eq!(live_edge_ids(&vault), ["edge-b"]);

    let conn = Connection::open(vault.path().join(".cairn/cairn.db")).expect("open cairn db");
    let wal_entry: (String, String, String) = conn
        .query_row(
            "SELECT state, kind, reason FROM wal_ops WHERE reason = 'lint:contradiction_resolution'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("lint fix wal entry");
    assert_eq!(
        wal_entry,
        (
            "COMMITTED".to_owned(),
            "graph_contradict".to_owned(),
            "lint:contradiction_resolution".to_owned(),
        )
    );
}

#[test]
fn lint_fix_folders_json_keeps_legacy_dispatch() {
    let vault = vault_with_contradictory_edges();

    let out = cli()
        .current_dir(vault.path())
        .args(["lint", "--fix-folders", "--json"])
        .output()
        .expect("cairn lint --fix-folders --json");

    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let json = parse_stdout_json(&out);
    assert_eq!(json["contract"], "cairn.mcp.v1");
    assert_eq!(json["status"], "aborted");
    assert_eq!(json["verb"], "lint");
    assert!(json.get("data").is_none(), "data: {:?}", json["data"]);
    assert_eq!(json["error"]["code"], "Internal");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("store not wired")),
        "message: {:?}",
        json["error"]["message"]
    );
}

#[test]
fn lint_fix_markdown_json_keeps_legacy_dispatch() {
    let vault = tempfile::tempdir().expect("temp vault");

    let out = cli()
        .current_dir(vault.path())
        .args(["lint", "--fix-markdown", "--json"])
        .output()
        .expect("cairn lint --fix-markdown --json");

    assert_eq!(out.status.code(), Some(78), "exit: {:?}", out.status);
    let json = parse_stdout_json(&out);
    assert_eq!(json["code"], "Internal");
    assert!(
        json["message"]
            .as_str()
            .is_some_and(|message| message.contains("no Cairn vault")),
        "message: {:?}",
        json["message"]
    );
}

#[test]
fn lint_write_report_json_fails_closed() {
    let vault = vault_with_contradictory_edges();

    let out = cli()
        .current_dir(vault.path())
        .args(["lint", "--write-report", "--json"])
        .output()
        .expect("cairn lint --write-report --json");

    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let json = parse_stdout_json(&out);
    assert_eq!(json["contract"], "cairn.mcp.v1");
    assert_eq!(json["status"], "aborted");
    assert_eq!(json["verb"], "lint");
    assert!(json.get("data").is_none(), "data: {:?}", json["data"]);
    assert_eq!(json["error"]["code"], "Internal");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("lint --write-report is not wired")),
        "message: {:?}",
        json["error"]["message"]
    );
}

#[test]
fn lint_fix_json_aborts_when_lint_schema_is_missing() {
    let vault = empty_db_without_lint_schema();

    let out = cli()
        .current_dir(vault.path())
        .args(["lint", "--fix", "--json"])
        .output()
        .expect("cairn lint --fix --json");

    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let json = parse_stdout_json(&out);
    assert_eq!(json["contract"], "cairn.mcp.v1");
    assert_eq!(json["status"], "aborted");
    assert_eq!(json["verb"], "lint");
    assert!(json.get("data").is_none(), "data: {:?}", json["data"]);
    assert_eq!(json["error"]["code"], "Internal");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("entity_edges")),
        "message: {:?}",
        json["error"]["message"]
    );
}
