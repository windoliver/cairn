//! End-to-end coverage for the bundled Cairn Nexus sandbox sidecar stub.

use std::process::Command;
use std::time::Duration;

use cairn_cli::nexus::{NexusSupervisor, ProjectionProbe, SupervisorConfig};

fn sidecar_binary() -> String {
    std::env::var("CARGO_BIN_EXE_cairn-nexus-sandbox")
        .expect("cargo must expose the cairn-nexus-sandbox binary path")
}

fn reserve_loopback_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn sidecar_help_exits_zero() {
    let out = Command::new(sidecar_binary())
        .arg("--help")
        .output()
        .expect("spawn cairn-nexus-sandbox --help");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("sandbox serve"), "{stdout}");
}

#[test]
fn bundled_sidecar_serves_health_through_supervisor() {
    let mut last_probe = None;
    for _ in 0..5 {
        let tmp = tempfile::tempdir().unwrap();
        let vault_dir = tmp.path().join("vault");
        let data_dir = vault_dir.join("nexus-data");
        let sqlite_db = vault_dir.join(".cairn").join("cairn.db");
        let endpoint = format!("http://127.0.0.1:{}", reserve_loopback_port());
        let mut supervisor = NexusSupervisor::start(SupervisorConfig {
            command: sidecar_binary(),
            args: vec!["sandbox".to_owned(), "serve".to_owned()],
            endpoint,
            health_path: "/health".to_owned(),
            data_dir: data_dir.clone(),
            sqlite_db,
            health_timeout: Duration::from_secs(5),
            shutdown_timeout: Duration::from_millis(500),
        })
        .unwrap();

        match supervisor.wait_until_healthy() {
            ProjectionProbe::Healthy => {
                assert!(data_dir.is_dir());
                supervisor.stop().unwrap();
                return;
            }
            probe @ ProjectionProbe::Degraded(_) => {
                last_probe = Some(probe);
                let _ = supervisor.stop();
            }
        }
    }

    panic!("bundled sidecar did not become healthy after retries: {last_probe:?}");
}
