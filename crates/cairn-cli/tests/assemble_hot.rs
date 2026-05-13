//! CLI behavior tests for `cairn assemble_hot`.

use std::fs;
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
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
