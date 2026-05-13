//! End-to-end tests for `cairn ingest` through filter/classify into storage.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{fs, path::Path, process::Command};

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

fn source_body(vault: &Path, source_ref: &str) -> String {
    fs::read_to_string(vault.join(source_ref)).expect("source artifact should exist")
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
    let source_ids = record["provenance"]["source_ids"]
        .as_array()
        .expect("source_ids array");
    assert_eq!(
        source_ids.len(),
        1,
        "ingest should emit one source artifact"
    );
    let source_ref = source_ids[0].as_str().expect("source ref string");
    assert!(
        source_ref.starts_with("sources/cli/"),
        "expected CLI source artifact path, got {source_ref}"
    );
    assert_eq!(
        source_body(vault.path(), source_ref),
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
    let source_ids = record["provenance"]["source_ids"]
        .as_array()
        .expect("source_ids array");
    assert_eq!(
        source_ids.len(),
        1,
        "ingest should emit one source artifact"
    );
    let source_ref = source_ids[0].as_str().expect("source ref string");
    assert!(
        source_ref.starts_with("sources/cli/"),
        "expected CLI source artifact path, got {source_ref}"
    );
    assert_eq!(
        source_body(vault.path(), source_ref),
        "User prefers compact updates. Body marker should stay out of metrics."
    );
}
