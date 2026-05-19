//! Integration tests for `cairn status` — structural assertions over
//! time-varying output (no insta snapshots since `incarnation` differs per call).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

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

fn write_vault_id(dir: &std::path::Path) {
    let config_dir = dir.join(".cairn");
    std::fs::create_dir_all(&config_dir).expect("create .cairn");
    std::fs::write(config_dir.join("vault.id"), "01HQZX9F5N0000000000000000")
        .expect("write vault.id");
}

fn spawn_health_server() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("make accepted health stream blocking");
                    let mut buf = [0_u8; 512];
                    let n = stream.read(&mut buf).expect("read health request");
                    let request = String::from_utf8_lossy(&buf[..n]);
                    assert!(
                        request.starts_with("GET /health HTTP/1.1\r\n"),
                        "unexpected health request: {request:?}"
                    );
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                        .expect("write health response");
                    return;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for health probe"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("accept health probe: {err}"),
            }
        }
    });
    (format!("http://{addr}"), handle)
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
    match v["sensors"]["screen"]["state"].as_str() {
        Some("enabled") => {
            assert_eq!(v["sensors"]["screen"]["mode"], "snapshot");
            assert_eq!(v["sensors"]["screen"]["permission"], "granted");
            assert!(
                v["sensors"]["screen"]["degradation"].is_null()
                    || v["sensors"]["screen"]["degradation"]["code"]
                        == "screen.backend_unavailable"
            );
        }
        Some("permission_missing") => {
            assert_eq!(v["sensors"]["screen"]["mode"], "snapshot");
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
            let expected_mode =
                if v["sensors"]["screen"]["degradation"]["code"] == "screen.backend_unavailable" {
                    "off"
                } else {
                    "snapshot"
                };
            assert_eq!(v["sensors"]["screen"]["mode"], expected_mode);
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

#[test]
fn status_json_reports_missing_authority_db() {
    let dir = tempfile::tempdir().unwrap();
    let out = cli()
        .current_dir(dir.path())
        .args(["status", "--json"])
        .output()
        .expect("cairn status --json");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["health"]["authority_db"]["state"], "missing");
    assert!(
        v["health"]["authority_db"]["path"]
            .as_str()
            .unwrap()
            .ends_with(".cairn/cairn.db")
    );
    assert_eq!(v["health"]["nexus_projection"]["state"], "disabled");
}

#[test]
fn status_json_reports_invalid_authority_db_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let cairn_dir = dir.path().join(".cairn");
    std::fs::create_dir(&cairn_dir).unwrap();
    write_vault_id(dir.path());
    std::fs::write(cairn_dir.join("cairn.db"), "not a sqlite database").unwrap();

    let out = cli()
        .current_dir(dir.path())
        .args(["status", "--json"])
        .output()
        .expect("cairn status --json");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["health"]["authority_db"]["state"], "unavailable");
    assert!(
        v["health"]["authority_db"]["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "missing non-empty reason: {stdout}"
    );
}

#[test]
fn status_human_prints_split_health() {
    let dir = tempfile::tempdir().unwrap();
    let out = cli()
        .current_dir(dir.path())
        .arg("status")
        .output()
        .expect("cairn status");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(stdout.contains("authority_db: missing"), "{stdout}");
    assert!(stdout.contains("nexus_projection: disabled"), "{stdout}");
}

#[test]
fn status_json_reports_degraded_nexus_projection_without_failing_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: http://127.0.0.1:0\n    health_timeout_ms: 25\n    shutdown_timeout_ms: 25\n",
    );

    let out = cli()
        .current_dir(dir.path())
        .args(["status", "--json"])
        .output()
        .expect("cairn status --json");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["health"]["authority_db"]["state"], "missing");
    assert_eq!(v["health"]["nexus_projection"]["state"], "degraded");
    let expected_data_dir = std::fs::canonicalize(dir.path())
        .unwrap()
        .join("nexus-data")
        .display()
        .to_string();
    assert_eq!(
        v["health"]["nexus_projection"]["data_dir"],
        expected_data_dir
    );
    assert_eq!(
        v["health"]["nexus_projection"]["endpoint"],
        "http://127.0.0.1:0"
    );
    assert!(
        v["health"]["nexus_projection"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("cairn nexus setup")
                && reason.contains("store.nexus.command")
                && reason.contains("nexusd")),
        "missing probe reason: {stdout}"
    );
    assert!(
        !dir.path().join("nexus-data").exists(),
        "status must not create projection directories"
    );
}

#[test]
fn status_json_reports_healthy_nexus_projection_from_health_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let (endpoint, server) = spawn_health_server();
    write_config(
        dir.path(),
        &format!(
            "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: {endpoint}\n    health_timeout_ms: 250\n    shutdown_timeout_ms: 25\n"
        ),
    );

    let out = cli()
        .current_dir(dir.path())
        .args(["status", "--json"])
        .output()
        .expect("cairn status --json");

    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert!(
        out.status.success(),
        "exit: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["health"]["authority_db"]["state"], "missing");
    assert_eq!(v["health"]["nexus_projection"]["state"], "healthy");
    let expected_data_dir = std::fs::canonicalize(dir.path())
        .unwrap()
        .join("nexus-data")
        .display()
        .to_string();
    assert_eq!(
        v["health"]["nexus_projection"]["data_dir"],
        expected_data_dir
    );
    assert_eq!(v["health"]["nexus_projection"]["endpoint"], endpoint);
    assert!(v["health"]["nexus_projection"]["reason"].is_null());
    server.join().unwrap();
    assert!(
        !dir.path().join("nexus-data").exists(),
        "status must not create projection directories"
    );
}
