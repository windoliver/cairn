//! Opt-in end-to-end test for a real Cairn-compatible Nexus sidecar.
//!
//! Run with:
//!
//! ```text
//! CAIRN_REAL_NEXUS_SIDECAR=1 \
//! CAIRN_REAL_NEXUS_SIDECAR_COMMAND=/path/to/sidecar \
//! CAIRN_REAL_NEXUS_SIDECAR_ARGS='["sandbox","serve"]' \
//! CAIRN_REAL_NEXUS_SIDECAR_ENDPOINT=http://127.0.0.1:8765 \
//! cargo nextest run -p cairn-cli --test nexus_real_sidecar --ignored --locked
//! ```

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use cairn_cli::nexus::{NexusSupervisor, ProjectionProbe, SupervisorConfig};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn require_opt_in() {
    assert_eq!(
        std::env::var("CAIRN_REAL_NEXUS_SIDECAR").as_deref(),
        Ok("1"),
        "set CAIRN_REAL_NEXUS_SIDECAR=1 to run the real sidecar e2e test"
    );
}

fn sidecar_command() -> String {
    std::env::var("CAIRN_REAL_NEXUS_SIDECAR_COMMAND").unwrap_or_else(|_| "nexus".to_owned())
}

fn sidecar_args() -> Vec<String> {
    let raw = std::env::var("CAIRN_REAL_NEXUS_SIDECAR_ARGS")
        .unwrap_or_else(|_| r#"["sandbox","serve"]"#.to_owned());
    if raw.trim_start().starts_with('[') {
        serde_json::from_str(&raw).expect("CAIRN_REAL_NEXUS_SIDECAR_ARGS must be a JSON array")
    } else {
        raw.split_whitespace().map(str::to_owned).collect()
    }
}

fn sidecar_endpoint() -> String {
    std::env::var("CAIRN_REAL_NEXUS_SIDECAR_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:8765".to_owned())
}

fn sidecar_health_path() -> String {
    std::env::var("CAIRN_REAL_NEXUS_SIDECAR_HEALTH_PATH").unwrap_or_else(|_| "/health".to_owned())
}

fn write_config(vault: &Path, endpoint: &str, health_path: &str) {
    let config_dir = vault.join(".cairn");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.yaml"),
        format!(
            "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: {endpoint:?}\n    health_path: {health_path:?}\n    health_timeout_ms: 5000\n    shutdown_timeout_ms: 2000\n"
        ),
    )
    .unwrap();
}

fn create_authority_db(vault: &Path) {
    let db_dir = vault.join(".cairn");
    std::fs::create_dir_all(&db_dir).unwrap();
    let conn = rusqlite::Connection::open(db_dir.join("cairn.db")).unwrap();
    conn.execute_batch("PRAGMA user_version = 1;").unwrap();
}

#[test]
#[ignore = "requires a real Cairn-compatible Nexus sidecar binary"]
fn real_sidecar_lifecycle_and_cli_status_e2e() {
    require_opt_in();

    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let data_dir = vault.join("nexus-data");
    let sqlite_db = vault.join(".cairn").join("cairn.db");
    let endpoint = sidecar_endpoint();
    let health_path = sidecar_health_path();
    create_authority_db(&vault);
    write_config(&vault, &endpoint, &health_path);

    let mut supervisor = NexusSupervisor::start(SupervisorConfig {
        command: sidecar_command(),
        args: sidecar_args(),
        endpoint: endpoint.clone(),
        health_path: health_path.clone(),
        data_dir: data_dir.clone(),
        sqlite_db: sqlite_db.clone(),
        health_timeout: Duration::from_secs(5),
        shutdown_timeout: Duration::from_secs(2),
    })
    .unwrap();

    match supervisor.wait_until_healthy() {
        ProjectionProbe::Healthy => {}
        ProjectionProbe::Degraded(reason) => {
            panic!("real sidecar did not become healthy: {reason}")
        }
    }

    let out = cli()
        .current_dir(&vault)
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
    assert_eq!(v["health"]["authority_db"]["state"], "healthy");
    assert_eq!(v["health"]["nexus_projection"]["state"], "healthy");
    assert_eq!(v["health"]["nexus_projection"]["endpoint"], endpoint);
    let expected_data_dir = std::fs::canonicalize(&vault)
        .unwrap()
        .join("nexus-data")
        .display()
        .to_string();
    assert_eq!(
        v["health"]["nexus_projection"]["data_dir"],
        expected_data_dir
    );

    supervisor.stop().unwrap();
}
