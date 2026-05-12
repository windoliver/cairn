//! Integration tests for `cairn status` — structural assertions over
//! time-varying output (no insta snapshots since `incarnation` differs per call).

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
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
fn status_json_reports_screen_default_disabled() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = cli()
        .args(["status", "--json"])
        .current_dir(dir.path())
        .env_remove("CAIRN_SENSORS__SCREEN__ENABLED")
        .env_remove("CAIRN_SENSORS__SCREEN__BACKEND")
        .env_remove("CAIRN_SENSORS__SCREEN__OCR__ENGINE")
        .output()
        .expect("cairn status --json");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["sensors"]["screen"]["backend"], "xcap");
    assert_eq!(v["sensors"]["screen"]["state"], "disabled");
    assert_eq!(v["sensors"]["screen"]["mode"], "off");
    assert_eq!(v["sensors"]["screen"]["permission"], "not_requested");
    assert_eq!(
        v["sensors"]["screen"]["degradation"]["code"],
        "screen.disabled"
    );
    let caps = v["capabilities"].as_array().expect("capabilities array");
    assert!(
        caps.iter().any(|c| c == "cairn.sensor.v1.screen.xcap"),
        "screen xcap capability missing: {caps:?}"
    );
}

#[test]
fn status_json_reports_unavailable_screenpipe_from_env() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = cli()
        .args(["status", "--json"])
        .current_dir(dir.path())
        .env("CAIRN_SENSORS__SCREEN__ENABLED", "true")
        .env("CAIRN_SENSORS__SCREEN__BACKEND", "screenpipe")
        .env_remove("CAIRN_SENSORS__SCREEN__OCR__ENGINE")
        .output()
        .expect("cairn status --json");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["sensors"]["screen"]["backend"], "screenpipe");
    let caps = v["capabilities"].as_array().expect("capabilities array");
    if caps
        .iter()
        .any(|c| c == "cairn.sensor.v1.screen.screenpipe")
    {
        assert_eq!(v["sensors"]["screen"]["state"], "permission_missing");
        assert_eq!(
            v["sensors"]["screen"]["degradation"]["code"],
            "screen.permission_missing"
        );
    } else {
        assert_eq!(v["sensors"]["screen"]["state"], "degraded");
        assert_eq!(
            v["sensors"]["screen"]["degradation"]["code"],
            "screen.backend_unavailable"
        );
    }
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
