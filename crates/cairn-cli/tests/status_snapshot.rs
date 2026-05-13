//! Integration tests for `cairn status` — structural assertions over
//! time-varying output (no insta snapshots since `incarnation` differs per call).

use std::process::Command;

use cairn_core::generated::status::StatusResponse;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    // Pin CWD to a clean tempdir so the workspace root's transient
    // `.cairn/` (left by other tests downloading embedding models) does
    // not trip the vault resolver into reporting a half-bootstrapped
    // vault and exiting EX_CONFIG.
    cmd.current_dir(std::env::temp_dir());
    cmd.env_remove("CAIRN_VAULT");
    cmd
}

#[test]
fn status_json_has_required_keys() {
    let out = cli()
        .args(["status", "--json"])
        .output()
        .expect("cairn status --json");
    assert!(
        out.status.success(),
        "cairn status --json failed: {:?}",
        out.status
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let response: StatusResponse =
        serde_json::from_str(stdout.trim()).expect("status must parse as generated type");
    assert_eq!(response.contract, "cairn.mcp.v1");
    assert!(!response.server_info.version.is_empty());
    assert!(!response.server_info.incarnation.0.is_empty());
    assert!(!response.server_info.started_at.is_empty());
    assert!(!response.server_info.build.is_empty());
}

#[test]
fn status_human_exits_zero() {
    let out = cli().arg("status").output().expect("cairn status");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        stdout.contains("cairn.mcp.v1"),
        "human output missing contract: {stdout}"
    );
}
