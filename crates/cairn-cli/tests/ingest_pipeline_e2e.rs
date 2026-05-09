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

#[test]
fn ingest_body_commits_record_through_signed_store() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let body = "Agent chose SQLite because P0 must stay offline.";

    let out = cli()
        .current_dir(vault.path())
        .args(["ingest", "--kind", "reference", "--body", body, "--json"])
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
    assert!(
        !serde_json::to_string(&response)
            .expect("response json")
            .contains(body),
        "ingest response must not echo raw body"
    );
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
    let record_id = response["data"]["record_id"]
        .as_str()
        .expect("record_id is string");
    assert!(
        !vault.path().join(".cairn/metrics.jsonl").exists(),
        "signed ingest path should not write the legacy metrics sidecar"
    );

    let record = active_record_json(vault.path(), record_id);
    assert_eq!(record["id"], record_id);
    assert_eq!(record["target_id"], record_id);
    assert_eq!(record["kind"], "reference");
    assert_eq!(record["class"], "semantic");
    assert_eq!(record["visibility"], "private");
    assert_eq!(record["scope"]["agent"], "agt:cairn-cli:default:writer:v1");
    assert_eq!(record["scope"]["entity"], "ingest");
    assert_eq!(record["body"], body);
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
fn ingest_file_runs_signed_pipeline_without_leaking_file_body() {
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
    assert!(
        !serde_json::to_string(&response)
            .expect("response json")
            .contains("Body marker"),
        "ingest response must not leak raw file body"
    );
    let record_id = response["data"]["record_id"]
        .as_str()
        .expect("record_id is string");

    assert!(
        !vault.path().join(".cairn/metrics.jsonl").exists(),
        "signed ingest path should not write the legacy metrics sidecar"
    );
    let record = active_record_json(vault.path(), record_id);
    assert_eq!(record["kind"], "user");
    assert_eq!(
        record["body"],
        "User prefers compact updates. Body marker should stay out of metrics."
    );
}
