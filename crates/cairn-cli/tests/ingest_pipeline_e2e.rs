//! End-to-end tests for `cairn ingest` through filter/classify into storage.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{fs, path::Path, process::Command};

use cairn_core::{
    contract::memory_store::StoredRecord,
    domain::{MemoryRecord, projection::MarkdownProjector},
};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn json_stdout(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout was not valid JSON: {e}\nstdout: {stdout:?}");
    })
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn metric_rows(vault: &std::path::Path) -> Vec<serde_json::Value> {
    let metrics = fs::read_to_string(vault.join(".cairn/metrics.jsonl"))
        .expect("metrics file should be written");
    metrics
        .lines()
        .map(|line| serde_json::from_str(line).expect("metric row JSON"))
        .collect()
}

fn active_record_json(vault: &Path, record_id: &str) -> serde_json::Value {
    let conn = rusqlite::Connection::open(vault.join(".cairn/cairn.db")).expect("open db");
    let active_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE active = 1 AND tombstoned = 0",
            [],
            |row| row.get(0),
        )
        .expect("count active records");
    assert_eq!(active_count, 1, "ingest should create one active record");

    let record_json: String = conn
        .query_row(
            "SELECT record_json FROM records WHERE record_id = ?1 AND active = 1",
            [record_id],
            |row| row.get(0),
        )
        .expect("record_json for committed record");
    serde_json::from_str(&record_json).expect("record_json parses")
}

fn active_stored_record_by_target(vault: &Path, target_id: &str) -> StoredRecord {
    let conn = rusqlite::Connection::open(vault.join(".cairn/cairn.db")).expect("open db");
    let (record_json, version): (String, i64) = conn
        .query_row(
            "SELECT record_json, version FROM records \
             WHERE target_id = ?1 AND active = 1 AND tombstoned = 0",
            [target_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("active record for target");
    let record: MemoryRecord = serde_json::from_str(&record_json).expect("record_json parses");
    StoredRecord {
        record,
        version: u32::try_from(version).expect("version fits u32"),
        schema_version: None,
    }
}

fn keyword_search_hits(vault: &Path, query: &str) -> Vec<serde_json::Value> {
    let out = cli()
        .current_dir(vault)
        .args(["search", query, "--mode", "keyword", "--json"])
        .output()
        .expect("cairn search");
    assert_eq!(
        out.status.code(),
        Some(0),
        "keyword search should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let response = json_stdout(&out);
    response["data"]["hits"]
        .as_array()
        .expect("hits array")
        .clone()
}

#[test]
fn ingest_body_commits_record_and_writes_accepted_metric() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "reasoning",
            "--session",
            "01HQZX9F5N0000000000000000",
            "--body",
            "Agent chose SQLite because P0 must stay offline.",
            "--json",
        ])
        .output()
        .expect("cairn ingest");

    assert_eq!(
        out.status.code(),
        Some(0),
        "ingest should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let response = json_stdout(&out);
    assert_eq!(response["contract"], "cairn.mcp.v1");
    assert_eq!(response["status"], "committed");
    assert!(response.get("error").is_none());
    assert_eq!(response["policy_trace"][0]["gate"], "presidio_redaction");
    assert_eq!(response["policy_trace"][0]["result"], "pass");
    assert_eq!(
        response["policy_trace"][1]["gate"],
        "prompt_injection_fence"
    );
    assert_eq!(
        response["policy_trace"][2]["gate"],
        "filter_should_memorize"
    );
    assert_eq!(response["policy_trace"][3]["gate"], "visibility_floor");
    assert_eq!(response["policy_trace"][3]["detail"], "floor:private");
    assert_eq!(response["policy_trace"][4]["gate"], "scope_check");
    let record_id = response["data"]["record_id"]
        .as_str()
        .expect("record_id is string");

    let rows = metric_rows(vault.path());
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["event"], "accepted");
    assert_eq!(row["record_id"], record_id);
    assert_eq!(row["kind"], "reasoning");
    assert_eq!(row["class"], "episodic");
    assert_eq!(row["visibility"], "private");
    assert_eq!(row["scope"]["session_id"], "01HQZX9F5N0000000000000000");
    assert_eq!(row["scope"]["agent"], "agt:cairn-cli:p0:v1");
    assert_eq!(row["rank"], 1);
    assert!(row.get("body").is_none(), "metric row must not leak body");

    let record = active_record_json(vault.path(), record_id);
    assert_eq!(record["id"], record_id);
    assert_eq!(record["target_id"], record_id);
    assert_eq!(record["kind"], "reasoning");
    assert_eq!(record["class"], "episodic");
    assert_eq!(record["visibility"], "private");
    assert_eq!(record["scope"]["session_id"], "01HQZX9F5N0000000000000000");
    assert_eq!(record["scope"]["agent"], "agt:cairn-cli:p0:v1");
    assert_eq!(
        record["body"],
        "Agent chose SQLite because P0 must stay offline."
    );
}

#[test]
fn ingest_rejects_invented_kind_before_store_dispatch() {
    let vault = tempfile::tempdir().expect("temp vault");

    let out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "invented_kind",
            "--body",
            "This must not classify into an invented taxonomy value.",
            "--json",
        ])
        .output()
        .expect("cairn ingest");

    assert_eq!(out.status.code(), Some(64));
    let response = json_stdout(&out);
    assert_eq!(response["status"], "rejected");
    assert_eq!(response["error"]["code"], "InvalidArgs");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("invented_kind")
    );
    assert_eq!(response["policy_trace"][0]["gate"], "scope_check");
    assert_eq!(response["policy_trace"][0]["result"], "error");
    assert!(
        !vault.path().join(".cairn/metrics.jsonl").exists(),
        "invalid taxonomy should reject before metric append"
    );
}

#[test]
fn ingest_file_runs_pipeline_without_leaking_file_body_to_metrics() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let note = vault.path().join("note.md");
    fs::write(
        &note,
        "User prefers compact updates. Body marker should stay out of metrics.",
    )
    .expect("write note");

    let out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "user",
            "--file",
            note.to_str().expect("utf-8 path"),
            "--json",
        ])
        .output()
        .expect("cairn ingest --file");

    assert_eq!(
        out.status.code(),
        Some(0),
        "ingest --file should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let response = json_stdout(&out);
    assert_eq!(response["status"], "committed");
    assert_eq!(response["policy_trace"][0]["result"], "pass");
    let record_id = response["data"]["record_id"]
        .as_str()
        .expect("record_id is string");

    let metrics = fs::read_to_string(vault.path().join(".cairn/metrics.jsonl"))
        .expect("metrics file should be written");
    assert!(metrics.contains(r#""event":"accepted""#));
    assert!(
        !metrics.contains("Body marker"),
        "metric rows must not include raw file body"
    );
    let record = active_record_json(vault.path(), record_id);
    assert_eq!(record["kind"], "user");
    assert_eq!(
        record["body"],
        "User prefers compact updates. Body marker should stay out of metrics."
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn ingest_resync_updates_record_through_cow_wal_and_searches_active_body() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let initial_body = "The preupdatewalmarker note documents CLI resync COW WAL behavior.";
    let updated_body = "The postupdatewalmarker note documents CLI resync COW WAL behavior.";

    let ingest_out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            initial_body,
            "--json",
        ])
        .output()
        .expect("cairn ingest");

    assert_eq!(
        ingest_out.status.code(),
        Some(0),
        "initial ingest should commit; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&ingest_out.stdout),
        String::from_utf8_lossy(&ingest_out.stderr)
    );
    let ingest_response = json_stdout(&ingest_out);
    assert_eq!(ingest_response["status"], "committed");
    let target_id = ingest_response["data"]["record_id"]
        .as_str()
        .expect("record_id is string");

    let stored_v1 = active_stored_record_by_target(vault.path(), target_id);
    assert_eq!(stored_v1.version, 1);
    assert_eq!(stored_v1.record.body, initial_body);

    let projected = MarkdownProjector.project(&stored_v1);
    let projected_path = vault.path().join(&projected.path);
    fs::create_dir_all(projected_path.parent().expect("projected parent"))
        .expect("create projected parent");
    let edited_projection = projected.content.replace(initial_body, updated_body);
    assert_ne!(
        edited_projection, projected.content,
        "projection edit must change the markdown body"
    );
    fs::write(&projected_path, edited_projection).expect("write projected markdown");

    let resync_out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--resync",
            projected_path.to_str().expect("utf-8 projection path"),
            "--json",
        ])
        .output()
        .expect("cairn ingest --resync");

    assert_eq!(
        resync_out.status.code(),
        Some(0),
        "resync should commit; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&resync_out.stdout),
        String::from_utf8_lossy(&resync_out.stderr)
    );
    let resync_response = json_stdout(&resync_out);
    assert_eq!(resync_response["status"], "committed");
    assert_eq!(resync_response["data"]["record_id"], target_id);

    let conn = rusqlite::Connection::open(vault.path().join(".cairn/cairn.db")).expect("open db");
    let active_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE active = 1 AND tombstoned = 0",
            [],
            |row| row.get(0),
        )
        .expect("count active records");
    assert_eq!(active_count, 1, "resync should leave one visible record");

    let target_row_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1",
            [target_id],
            |row| row.get(0),
        )
        .expect("count target rows");
    assert_eq!(target_row_count, 2, "resync should keep v1 and create v2");

    let (active_record_id, active_version, active_body, active_cow_staged): (
        String,
        i64,
        String,
        i64,
    ) = conn
        .query_row(
            "SELECT record_id, version, body, cow_staged FROM records \
             WHERE target_id = ?1 AND active = 1 AND tombstoned = 0",
            [target_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("active row");
    assert_ne!(
        active_record_id, target_id,
        "content-changing resync should mint a new version row"
    );
    assert_eq!(active_version, 2);
    assert_eq!(active_body, updated_body);
    assert_eq!(active_cow_staged, 0, "activated row must not stay staged");

    let staged_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND cow_staged = 1",
            [target_id],
            |row| row.get(0),
        )
        .expect("count staged rows");
    assert_eq!(staged_count, 0, "committed resync must drain COW staging");

    let committed_upserts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wal_ops WHERE kind = 'upsert' AND state = 'COMMITTED'",
            [],
            |row| row.get(0),
        )
        .expect("committed upsert wal ops");
    assert_eq!(committed_upserts, 2);

    let done_upsert_steps: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wal_steps ws \
              JOIN wal_ops wo ON wo.operation_id = ws.operation_id \
             WHERE wo.kind = 'upsert' AND ws.state = 'DONE'",
            [],
            |row| row.get(0),
        )
        .expect("done upsert wal steps");
    assert!(
        done_upsert_steps >= 6,
        "resync should complete at least one COW upsert step graph"
    );

    let stepped_upserts: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT wo.operation_id) FROM wal_ops wo \
              JOIN wal_steps ws ON wo.operation_id = ws.operation_id \
             WHERE wo.kind = 'upsert'",
            [],
            |row| row.get(0),
        )
        .expect("upsert wal ops with steps");
    assert!(
        stepped_upserts >= 1,
        "resync should produce a stepped WAL upsert"
    );

    let unfinished_upsert_steps: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wal_steps ws \
              JOIN wal_ops wo ON wo.operation_id = ws.operation_id \
             WHERE wo.kind = 'upsert' AND ws.state != 'DONE'",
            [],
            |row| row.get(0),
        )
        .expect("unfinished upsert wal steps");
    assert_eq!(unfinished_upsert_steps, 0);

    let payload_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wal_payloads wp \
              JOIN wal_ops wo ON wo.operation_id = wp.operation_id \
             WHERE wo.kind = 'upsert'",
            [],
            |row| row.get(0),
        )
        .expect("upsert wal payloads");
    assert!(
        payload_count >= 1,
        "resync should persist an upsert WAL payload"
    );
    assert_eq!(
        payload_count, stepped_upserts,
        "each stepped upsert WAL op should have a payload"
    );

    let old_hits = keyword_search_hits(vault.path(), "preupdatewalmarker");
    assert!(
        old_hits.is_empty(),
        "inactive v1 body must not be searchable"
    );

    let new_hits = keyword_search_hits(vault.path(), "postupdatewalmarker");
    assert_eq!(new_hits.len(), 1);
    assert_eq!(new_hits[0]["record_id"], active_record_id);
}
