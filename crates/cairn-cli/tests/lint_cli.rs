// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::process::Command;

use cairn_store_sqlite::migrate;
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

    let conn = Connection::open(cairn_dir.join("cairn.db")).expect("open cairn db");
    migrate(&conn).expect("migrate cairn db");
    insert_edge(&conn, "edge-a", "INFERRED", 0.7);
    insert_edge(&conn, "edge-b", "EXTRACTED", 1.0);

    vault
}

fn insert_edge(conn: &Connection, id: &str, confidence: &str, confidence_score: f64) {
    conn.execute(
        "INSERT INTO entity_edges (
            id, source_id, target_id, relation, valid_at, invalid_at,
            expired_at, confidence, confidence_score, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
    assert_eq!(json["data"]["summary"]["contradictions"], 1);
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
    assert_eq!(json["data"]["summary"]["contradictions"], 0);
    assert_eq!(json["data"]["summary"]["auto_resolved"], 1);
    assert_eq!(json["data"]["summary"]["total"], 0);
    assert_eq!(live_edge_ids(&vault), ["edge-b"]);
}
