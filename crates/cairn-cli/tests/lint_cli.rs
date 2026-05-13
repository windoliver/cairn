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
    assert_eq!(json["status"], "committed", "full json: {json:#}");
    assert_eq!(json["verb"], "lint");
    assert!(
        json.get("error").is_none(),
        "error: {:?}",
        json.get("error")
    );
    assert_eq!(json["data"]["summary"]["by_kind"]["contradictory_edge"], 1);
    // The test vault was migrated directly via `migrations().to_latest()`
    // without a vault.id / config / projection — `cairn_store_sqlite::open`
    // rejects it, so the binary's lint dispatch falls into the degraded
    // `Err(store_err)` branch which (since codex review round 1 finding 5)
    // emits a `DeferredCheck` Error documenting that vault-level checks
    // did not run. Edge-integrity findings are merged on top.
    assert_eq!(json["data"]["summary"]["by_kind"]["deferred_check"], 1);
    assert_eq!(json["data"]["summary"]["auto_resolved"], 0);
    assert_eq!(json["data"]["summary"]["total"], 2);
    let findings = json["data"]["findings"].as_array().expect("findings array");
    let edge_finding = findings
        .iter()
        .find(|f| f["kind"] == "contradictory_edge")
        .expect("contradictory_edge finding");
    assert_eq!(edge_finding["severity"], "warning");
    assert_eq!(
        edge_finding["entities"],
        serde_json::json!(["edge-a", "edge-b"])
    );
    let deferred = findings
        .iter()
        .find(|f| f["kind"] == "deferred_check")
        .expect("deferred_check finding documenting store-open failure");
    assert_eq!(deferred["severity"], "error");
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
fn lint_write_report_json_writes_report_and_commits() {
    // --write-report is now wired (dispatch through lint_handler);
    // the old "fails-closed" guard was removed in Task 28. Verify the
    // happy path: committed response + report file written under
    // .cairn/lint-report.md.
    //
    // We use a vault with an empty DB (no prior migrations). The binary
    // runs migrations internally via `cairn_store_sqlite::open`, so
    // `verify_schema_fingerprint` passes and `lint_handler` runs fully.
    // vault_with_contradictory_edges drops schema guards (triggers/index),
    // causing the schema fingerprint check to fail; we need an intact
    // schema for --write-report to work.
    let vault = tempfile::tempdir().expect("temp vault");
    let cairn_dir = vault.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn dir");
    // Create an empty DB file so `require_existing_vault` passes. Drop the
    // connection immediately so the binary's `cairn_store_sqlite::open` call
    // can acquire the file without contention.
    {
        let _conn = Connection::open(cairn_dir.join("cairn.db")).expect("open empty cairn db");
    }

    let out = cli()
        .current_dir(vault.path())
        .args(["lint", "--write-report", "--json"])
        .output()
        .expect("cairn lint --write-report --json");

    // Empty vault → no Error findings → exit 0 (deferred_check is Info;
    // broken_source_link for missing purpose/index is Error, so exit 1).
    let json = parse_stdout_json(&out);
    assert_eq!(json["contract"], "cairn.mcp.v1", "full json: {json:#}");
    assert_eq!(json["status"], "committed", "full json: {json:#}");
    assert_eq!(json["verb"], "lint");
    assert!(
        json.get("error").is_none(),
        "error: {:?}",
        json.get("error")
    );
    // report_path is set when --write-report is used.
    assert!(
        json["data"]["report_path"].is_string(),
        "expected report_path string; json: {json:#}"
    );
    // The report file must exist on disk.
    let report_path = vault.path().join(".cairn/lint-report.md");
    assert!(
        report_path.exists(),
        ".cairn/lint-report.md not found after --write-report"
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
