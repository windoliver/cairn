//! End-to-end smoke for `cairn forget --record <ulid>` (issue #58).
//!
//! Bootstraps a vault, ingests a fact, then runs `forget --record` against
//! the stored `target_id` and asserts:
//! 1. The committed envelope shape (status=committed, verb=forget,
//!    `deleted_count=1`).
//! 2. The record disappears from `cairn search` afterwards.
//!
//! Bypasses the binary-level `cairn bootstrap` to avoid the BGE embedding
//! model download that the CLI bootstrap triggers when
//! `search.local_embeddings: true`. The library helper
//! `cairn_cli::vault::bootstrap` performs the same vault layout creation
//! without fetching the model.

use std::process::Command;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd.env_remove("CAIRN_ISSUER");
    cmd
}

#[test]
fn forget_record_commits_and_search_no_longer_returns_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    // 1. Ingest a record.
    let ingest_out = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "hello forget",
            "--json",
        ])
        .output()
        .expect("cairn ingest --json");
    assert_eq!(
        ingest_out.status.code(),
        Some(0),
        "ingest must succeed; stderr: {}",
        String::from_utf8_lossy(&ingest_out.stderr)
    );
    let ingest_stdout = String::from_utf8(ingest_out.stdout).expect("utf-8");
    let ingest_json: serde_json::Value = serde_json::from_str(ingest_stdout.trim())
        .unwrap_or_else(|e| panic!("ingest JSON parse failed: {e}\nstdout: {ingest_stdout:?}"));
    assert_eq!(ingest_json["status"], "committed");

    // Resolve the target_id by reading directly from the SQLite DB. The
    // ingest envelope only carries the version-specific record_id; the
    // forget verb takes the supersession lineage key (target_id) which
    // are independent ULIDs (brief §3 / §3.0).
    let db_path = dir.path().join(".cairn").join("cairn.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open cairn.db");
    let target_id: String = conn
        .query_row("SELECT target_id FROM records LIMIT 1", [], |row| {
            row.get(0)
        })
        .expect("read target_id from records table");
    drop(conn);
    assert_eq!(target_id.len(), 26, "target_id must be a 26-char ULID");

    // 2. Forget the record by target_id.
    let forget_out = cli()
        .current_dir(dir.path())
        .args(["forget", "--record", target_id.as_str(), "--json"])
        .output()
        .expect("cairn forget --record --json");
    assert_eq!(
        forget_out.status.code(),
        Some(0),
        "forget must succeed; stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&forget_out.stderr),
        String::from_utf8_lossy(&forget_out.stdout)
    );
    let forget_stdout = String::from_utf8(forget_out.stdout).expect("utf-8");
    let forget_json: serde_json::Value = serde_json::from_str(forget_stdout.trim())
        .unwrap_or_else(|e| panic!("forget JSON parse failed: {e}\nstdout: {forget_stdout:?}"));
    assert_eq!(forget_json["contract"], "cairn.mcp.v1");
    assert_eq!(forget_json["status"], "committed");
    assert_eq!(forget_json["verb"], "forget");
    assert!(forget_json["error"].is_null(), "no error on success");
    assert_eq!(
        forget_json["data"]["deleted_count"], 1,
        "deleted_count must be 1 after committing a single forget"
    );
    assert!(forget_json["operation_id"].is_string());
    assert!(forget_json["policy_trace"].is_array());

    // 3. Confirm the record is gone via `cairn search --mode keyword`.
    let search_out = cli()
        .current_dir(dir.path())
        .args(["search", "--mode", "keyword", "hello forget", "--json"])
        .output()
        .expect("cairn search --json");
    assert_eq!(
        search_out.status.code(),
        Some(0),
        "search must exit 0; stderr: {}",
        String::from_utf8_lossy(&search_out.stderr)
    );
    let search_stdout = String::from_utf8(search_out.stdout).expect("utf-8");
    let search_json: serde_json::Value = serde_json::from_str(search_stdout.trim())
        .unwrap_or_else(|e| panic!("search JSON parse failed: {e}\nstdout: {search_stdout:?}"));
    let hits = search_json["data"]["hits"]
        .as_array()
        .expect("search must return data.hits array");
    assert!(
        hits.is_empty(),
        "search after forget must return no hits, got {hits:?}"
    );
}
