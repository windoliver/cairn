//! CLI behavior tests for `cairn assemble_hot`.

use std::fs;
use std::process::Command;

use cairn_store_sqlite::{HotRecordSeed, SqliteMemoryStore};
use serde_json::Value;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn write_yaml(vault: &std::path::Path, content: &str) {
    let dir = vault.join(".cairn");
    fs::create_dir_all(&dir).expect("config dir");
    fs::write(dir.join("config.yaml"), content).expect("config");
}

fn run_json(vault: &std::path::Path, args: &[&str]) -> Value {
    let out = cli().current_dir(vault).args(args).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    serde_json::from_str(stdout.trim()).expect("json")
}

#[test]
fn assemble_hot_json_returns_committed_prefix_and_metadata() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join(".cairn")).expect("config dir");
    fs::write(dir.path().join("purpose.md"), "purpose from cli").expect("purpose");
    let out = cli()
        .current_dir(dir.path())
        .args(["assemble_hot", "--json"])
        .output()
        .expect("run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["verb"], "assemble_hot");
    assert_eq!(v["status"], "committed");
    assert!(
        v["data"]["prefix"]
            .as_str()
            .expect("prefix")
            .contains("purpose from cli")
    );
    assert!(v["data"]["bytes"].as_u64().expect("bytes") > 0);
    assert!(v["data"]["sources"].is_array());
    assert!(v["data"]["truncation"].is_array());
    assert!(v["data"]["cache"]["status"].is_string());
}

#[test]
fn assemble_hot_budget_zero_returns_empty_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("purpose.md"), "purpose from cli").expect("purpose");
    let out = cli()
        .current_dir(dir.path())
        .args(["assemble_hot", "--budget", "0", "--json"])
        .output()
        .expect("run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(v["data"]["prefix"], "");
    assert_eq!(v["data"]["bytes"], 0);
}

#[test]
fn assemble_hot_budget_above_hard_cap_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = cli()
        .current_dir(dir.path())
        .args(["assemble_hot", "--budget", "4194305", "--json"])
        .output()
        .expect("run");
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(v["status"], "rejected");
    assert_eq!(v["error"]["code"], "InvalidArgs");
    assert_eq!(v["error"]["data"]["field"], "budget");
}

#[test]
fn assemble_hot_json_honors_recipe_session_scope_and_persistent_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_yaml(
        dir.path(),
        "vault:\n  hot_memory:\n    max_bytes: 512\n    recipe:\n      - purpose\n      - pinned_feedback\n",
    );
    fs::write(dir.path().join("purpose.md"), "purpose e2e").expect("purpose");
    fs::write(dir.path().join("profile.md"), "profile e2e").expect("profile");
    fs::write(dir.path().join("index.md"), "index should be disabled").expect("index");
    {
        let store = SqliteMemoryStore::open(dir.path()).expect("store");
        store
            .insert_hot_record(
                HotRecordSeed::new("01J0000000000000000000001", "user", "session one pinned")
                    .session("session-one")
                    .tag("pinned")
                    .salience(0.9),
            )
            .expect("session one record");
        store
            .insert_hot_record(
                HotRecordSeed::new("01J0000000000000000000002", "user", "session two pinned")
                    .session("session-two")
                    .tag("pinned")
                    .salience(0.9),
            )
            .expect("session two record");
    }

    let first = run_json(
        dir.path(),
        &["assemble_hot", "--session", "session-one", "--json"],
    );
    assert_eq!(first["status"], "committed");
    assert_eq!(first["data"]["cache"]["status"], "refreshed");
    let prefix = first["data"]["prefix"].as_str().expect("prefix");
    assert!(prefix.contains("purpose e2e"));
    assert!(prefix.contains("profile e2e"));
    assert!(prefix.contains("session one pinned"));
    assert!(!prefix.contains("index should be disabled"));
    assert!(!prefix.contains("session two pinned"));
    assert!(
        prefix.find("## purpose").expect("purpose section")
            < prefix.find("## profile").expect("profile section")
    );
    assert!(
        prefix.find("## profile").expect("profile section")
            < prefix.find("## pinned").expect("pinned section")
    );
    let source_kinds = first["data"]["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .map(|source| source["kind"].as_str().expect("kind"))
        .collect::<Vec<_>>();
    assert_eq!(source_kinds, ["purpose", "profile", "pinned"]);

    let second = run_json(
        dir.path(),
        &["assemble_hot", "--session", "session-one", "--json"],
    );
    assert_eq!(second["data"]["cache"]["status"], "hit");
    assert_eq!(second["data"]["prefix"], first["data"]["prefix"]);
}

#[test]
fn assemble_hot_json_reports_large_attempted_bytes_without_large_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("purpose.md"),
        "x".repeat((4 * 1024 * 1024) + 64),
    )
    .expect("purpose");

    let v = run_json(dir.path(), &["assemble_hot", "--budget", "32", "--json"]);
    assert_eq!(v["status"], "committed");
    assert!(v["data"]["bytes"].as_u64().expect("bytes") <= 32);
    assert!(v["data"]["prefix"].as_str().expect("prefix").len() <= 32);
    let truncation = v["data"]["truncation"]
        .as_array()
        .expect("truncation")
        .first()
        .expect("truncation entry");
    assert_eq!(truncation["kind"], "purpose");
    assert_eq!(truncation["reason"], "section_truncated");
    assert!(
        truncation["attempted_bytes"]
            .as_u64()
            .expect("attempted bytes")
            > 4 * 1024 * 1024
    );
    assert!(
        truncation["included_bytes"]
            .as_u64()
            .expect("included bytes")
            <= 32
    );
}
