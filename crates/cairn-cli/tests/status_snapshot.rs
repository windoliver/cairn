//! Integration tests for `cairn status` — structural assertions over
//! time-varying output (no insta snapshots since `incarnation` differs per call).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn write_yaml(vault: &std::path::Path, content: &str) {
    let dir = vault.join(".cairn");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.yaml"), content).unwrap();
}

fn spawn_health_server() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
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
    write_yaml(
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
            .is_some_and(|reason| !reason.is_empty()),
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
    write_yaml(
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
    server.join().unwrap();

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
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
    assert!(
        !dir.path().join("nexus-data").exists(),
        "status must not create projection directories"
    );
}
