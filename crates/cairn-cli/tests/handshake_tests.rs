//! Integration tests for `cairn handshake` — structural assertions.
//! These tests invoke the compiled binary and will pass after Task 7 wires dispatch.

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

#[test]
fn handshake_json_has_challenge_keys() {
    let out = cli()
        .args(["handshake", "--json"])
        .output()
        .expect("cairn handshake --json");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert!(v["challenge"]["nonce"].is_string());
    assert!(v["challenge"]["expires_at"].is_number());
    let expires: u64 = v["challenge"]["expires_at"].as_u64().expect("u64");
    assert!(expires > 0, "expires_at must be a positive epoch-ms value");
}

#[test]
fn two_handshakes_return_different_nonces() {
    let out1 = cli()
        .args(["handshake", "--json"])
        .output()
        .expect("handshake 1");
    let out2 = cli()
        .args(["handshake", "--json"])
        .output()
        .expect("handshake 2");
    let v1: serde_json::Value =
        serde_json::from_str(String::from_utf8(out1.stdout).expect("utf-8").trim()).expect("json");
    let v2: serde_json::Value =
        serde_json::from_str(String::from_utf8(out2.stdout).expect("utf-8").trim()).expect("json");
    assert_ne!(
        v1["challenge"]["nonce"], v2["challenge"]["nonce"],
        "consecutive handshakes must produce distinct nonces (§8.0.a point d)"
    );
}

#[test]
fn handshake_human_exits_zero() {
    let out = cli().arg("handshake").output().expect("cairn handshake");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        stdout.contains("nonce"),
        "human output missing nonce line: {stdout}"
    );
}
