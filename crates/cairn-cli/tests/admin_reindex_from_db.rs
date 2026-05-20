//! CLI integration test for `cairn admin reindex --from-db`.
//!
//! Destructive test: builds a vault with 3 records, nukes both the FTS5
//! mirror and the vector table via raw SQL, then runs the CLI command to
//! verify they are rebuilt. Asserts `fts_rebuilt == 3` and `drained >= 3`
//! in the JSON output, then snapshots the result.
//!
//! `CAIRN_MOCK_EMBEDDER=1` is set so the CLI subprocess uses `MockEmbedder`
//! (matching the vectors in the DB) rather than requiring real model
//! weights on disk.

use assert_cmd::Command;
use cairn_test_fixtures::{RecordSpec, build_hybrid_test_vault};

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_from_db_snapshot() {
    let vault = build_hybrid_test_vault(&[
        RecordSpec::from_body("alpha"),
        RecordSpec::from_body("bravo"),
        RecordSpec::from_body("charlie"),
    ])
    .await;

    let root = vault.root.clone();
    let db_path = vault.db_path.clone();
    let dir = vault.dir;
    // Drop store + embedder before opening the DB with rusqlite directly
    // and before spawning the CLI subprocess.
    drop(vault.store);
    drop(vault.embedder);

    // Destruction pass: open the DB and nuke both indexes via raw SQL.
    {
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        conn.execute("DELETE FROM records_fts", [])
            .expect("delete from records_fts");
        conn.execute("DELETE FROM record_vectors", [])
            .expect("delete from record_vectors");
        // conn is dropped here, releasing the file lock.
    }

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", &root)
        .env("CAIRN_MOCK_EMBEDDER", "1")
        .args(["admin", "reindex", "--from-db", "--json"])
        .output()
        .expect("run cli");
    assert!(
        output.status.success(),
        "exit non-zero. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("valid json output from admin reindex");
    assert_eq!(
        parsed["fts_rebuilt"].as_u64().expect("fts_rebuilt field"),
        3,
        "expected 3 FTS rows rebuilt; got: {stdout}"
    );
    assert!(
        parsed["drained"].as_u64().expect("drained field") >= 3,
        "expected at least 3 records drained (embedded); got: {stdout}"
    );

    let metrics = std::fs::read_to_string(root.join(".cairn/metrics.jsonl"))
        .expect("projection rebuild metric file");
    let projection_metric = metrics
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("metric json"))
        .find(|row| row["event"] == "projection_rebuild")
        .expect("projection_rebuild metric");
    assert_eq!(projection_metric["projection"], "sqlite.from_db");
    assert_eq!(projection_metric["status"], "committed");
    assert_eq!(projection_metric["records_rebuilt"], 3);
    assert!(projection_metric["latency_ms"].as_u64().is_some());
    assert_eq!(projection_metric["retry_count"], 0);
    assert_eq!(projection_metric["queue_lag_ms"], 0);
    assert_eq!(projection_metric["error"], serde_json::Value::Null);
    assert!(
        !metrics.contains("alpha") && !metrics.contains("bravo") && !metrics.contains("charlie"),
        "projection metric must not export record bodies: {metrics}"
    );

    insta::assert_snapshot!("admin_reindex_from_db", stdout);
    drop(dir);
}
