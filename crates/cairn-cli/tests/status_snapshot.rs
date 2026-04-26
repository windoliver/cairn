//! Integration tests for `cairn status`.

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

#[test]
fn status_json_has_required_keys() {
    let out = cli().args(["status", "--json"]).output().expect("cairn status --json");
    assert!(out.status.success(), "cairn status --json failed: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert!(v["server_info"]["version"].is_string());
    assert!(v["server_info"]["incarnation"].is_string());
    assert!(v["server_info"]["started_at"].is_string());
    assert!(v["server_info"]["build"].is_string());
    assert!(v["capabilities"].is_array());
    assert!(v["extensions"].is_array());
}

#[test]
fn status_human_exits_zero() {
    let out = cli().arg("status").output().expect("cairn status");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(stdout.contains("cairn.mcp.v1"), "human output missing contract: {stdout}");
}
