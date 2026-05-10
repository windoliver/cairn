//! CLI coverage for wired `cairn forget --record`.

use std::path::Path;
use std::process::{Command, Output};

use cairn_core::generated::envelope::{Response, ResponseData, ResponseStatus, ResponseVerb};
use serde_json::Value;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn run_in_vault(vault: &Path, args: &[&str]) -> Output {
    cli()
        .current_dir(vault)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run cairn {args:?}: {e}"))
}

fn run_json_ok(vault: &Path, args: &[&str]) -> Value {
    let out = run_in_vault(vault, args);
    assert_eq!(
        out.status.code(),
        Some(0),
        "cairn {args:?} failed\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("cairn {args:?} emitted invalid JSON: {e}"))
}

fn hit_count(vault: &Path, query: &str) -> usize {
    let search = run_json_ok(vault, &["search", "--mode", "keyword", query, "--json"]);
    search["data"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("search hits must be an array: {search}"))
        .len()
}

fn forget_record_wal_operation_id(vault: &Path) -> String {
    let conn = rusqlite::Connection::open(vault.join(".cairn/cairn.db")).expect("open cairn db");
    conn.query_row(
        "SELECT operation_id FROM wal_ops WHERE kind = 'forget_record'",
        [],
        |row| row.get(0),
    )
    .expect("forget_record wal op")
}

#[test]
fn forget_record_json_commits_in_bound_vault() {
    let dir = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(dir.path());

    let body = "issue58forgetunique body survives until record forget";
    let ingest = run_json_ok(
        dir.path(),
        &["ingest", "--kind", "reference", "--body", body, "--json"],
    );
    let record_id = ingest["data"]["record_id"]
        .as_str()
        .expect("ingest record_id")
        .to_owned();
    assert_eq!(hit_count(dir.path(), "issue58forgetunique"), 1);

    let out = run_in_vault(dir.path(), &["forget", "--record", &record_id, "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "forget should commit\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let resp: Response = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("forget envelope parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(resp.contract, "cairn.mcp.v1");
    assert!(matches!(resp.status, ResponseStatus::Committed));
    assert!(matches!(resp.verb, ResponseVerb::Forget));
    let response_operation_id = resp.operation_id.0.clone();
    let data = resp.data.expect("committed forget must carry data");
    let ResponseData::Forget(data) = data else {
        panic!("forget response must carry ForgetData");
    };
    assert_eq!(data.deleted_count, 1);
    assert_eq!(data.plan_ref, None);
    assert_eq!(
        data.tombstones
            .expect("forget should report tombstones")
            .into_iter()
            .map(|id| id.0)
            .collect::<Vec<_>>(),
        vec![record_id]
    );

    assert_eq!(
        forget_record_wal_operation_id(dir.path()),
        format!("forget_record-{response_operation_id}")
    );
    assert_eq!(hit_count(dir.path(), "issue58forgetunique"), 0);
}

fn assert_capability_unavailable(vault: Option<&Path>, args: &[&str], capability: &str) {
    let mut cmd = cli();
    if let Some(vault) = vault {
        cmd.current_dir(vault);
    }
    let out = cmd
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run cairn {args:?}: {e}"));
    assert_eq!(
        out.status.code(),
        Some(69),
        "cairn {args:?} should exit 69\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let v: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("rejection JSON parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "rejected");
    assert_eq!(v["verb"], "forget");
    assert_eq!(v["error"]["code"], "CapabilityUnavailable");
    assert_eq!(v["error"]["data"]["capability"], capability);
}

#[test]
fn forget_session_and_scope_remain_capability_unavailable() {
    assert_capability_unavailable(
        None,
        &[
            "forget",
            "--session",
            "01JXXXXXXXXXXXXXXXXXXXXXXX",
            "--json",
        ],
        "cairn.mcp.v1.forget.session",
    );
    assert_capability_unavailable(
        None,
        &["forget", "--scope", r#"{"tenant":"default"}"#, "--json"],
        "cairn.mcp.v1.forget.scope",
    );
}

#[test]
fn forget_session_and_scope_ignore_malformed_vault_config() {
    let dir = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(dir.path());
    std::fs::write(
        dir.path().join(".cairn/config.yaml"),
        b": :\nnot: [valid yaml",
    )
    .expect("write malformed config");

    assert_capability_unavailable(
        Some(dir.path()),
        &[
            "forget",
            "--session",
            "01JXXXXXXXXXXXXXXXXXXXXXXX",
            "--json",
        ],
        "cairn.mcp.v1.forget.session",
    );
    assert_capability_unavailable(
        Some(dir.path()),
        &["forget", "--scope", r#"{"tenant":"default"}"#, "--json"],
        "cairn.mcp.v1.forget.scope",
    );
}
