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

fn write_config(dir: &std::path::Path, content: &str) {
    let config_dir = dir.join(".cairn");
    std::fs::create_dir_all(&config_dir).expect("create .cairn");
    std::fs::write(config_dir.join("vault.id"), "01HQZX9F5N0000000000000000")
        .expect("write vault.id");
    std::fs::write(config_dir.join("config.yaml"), content).expect("write config");
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
fn status_json_uses_screen_config_file_e2e() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_config(
        dir.path(),
        "sensors:\n  screen:\n    enabled: true\n    backend: xcap\n",
    );

    let out = cli()
        .args(["status", "--json"])
        .current_dir(dir.path())
        .env_remove("CAIRN_SENSORS__SCREEN__ENABLED")
        .env_remove("CAIRN_SENSORS__SCREEN__BACKEND")
        .env_remove("CAIRN_SENSORS__SCREEN__OCR__ENGINE")
        .output()
        .expect("cairn status --json");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");

    assert_eq!(v["sensors"]["screen"]["backend"], "xcap");
    assert_eq!(v["sensors"]["screen"]["mode"], "snapshot");
    match v["sensors"]["screen"]["state"].as_str() {
        Some("enabled") => {
            assert_eq!(v["sensors"]["screen"]["permission"], "granted");
            assert!(
                v["sensors"]["screen"]["degradation"].is_null()
                    || v["sensors"]["screen"]["degradation"]["code"]
                        == "screen.backend_unavailable"
            );
        }
        Some("permission_missing") => {
            assert_eq!(v["sensors"]["screen"]["permission"], "denied");
            assert_eq!(
                v["sensors"]["screen"]["degradation"]["code"],
                "screen.permission_missing"
            );
        }
        Some("degraded") => {
            assert!(
                matches!(
                    v["sensors"]["screen"]["degradation"]["code"].as_str(),
                    Some("screen.backend_unavailable" | "screen.degraded")
                ),
                "unexpected screen degradation: {}",
                v["sensors"]["screen"]["degradation"]
            );
        }
        other => panic!("unexpected screen state: {other:?}"),
    }
}

#[test]
fn status_json_rejects_invalid_screen_config_e2e() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_config(
        dir.path(),
        "sensors:\n  screen:\n    budget:\n      max_frames_per_minute: 0\n",
    );

    let out = cli()
        .args(["status", "--json"])
        .current_dir(dir.path())
        .env_remove("CAIRN_SENSORS__SCREEN__ENABLED")
        .env_remove("CAIRN_SENSORS__SCREEN__BACKEND")
        .env_remove("CAIRN_SENSORS__SCREEN__OCR__ENGINE")
        .output()
        .expect("cairn status --json");

    assert_eq!(out.status.code(), Some(78), "exit: {:?}", out.status);
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("cairn status: config error"),
        "missing config error: {stderr}"
    );
    assert!(
        stderr.contains("sensors.screen.budget.max_frames_per_minute"),
        "warning should name invalid field: {stderr}"
    );
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
