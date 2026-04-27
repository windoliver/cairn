//! Integration tests for `cairn handshake` — P0 stub behaviour.
//!
//! Challenge authentication requires persisting the nonce server-side. Until
//! the store is wired (#9), handshake returns `Internal aborted` rather than
//! emitting an ephemeral challenge that can never be validated.

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

#[test]
fn handshake_json_returns_internal_aborted() {
    let out = cli()
        .args(["handshake", "--json"])
        .output()
        .expect("cairn handshake --json");
    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "aborted");
    assert_eq!(v["error"]["code"], "Internal");
    assert!(v["operation_id"].is_string());
}

#[test]
fn handshake_human_exits_one_with_internal() {
    let out = cli().arg("handshake").output().expect("cairn handshake");
    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert!(
        stderr.contains("Internal"),
        "human output missing Internal error code: {stderr}"
    );
}
