//! End-to-end tests for `cairn sensor` control commands.

use std::path::Path;
use std::process::Command;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd.env_remove("CAIRN_ISSUER");
    cmd
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn json_stdout(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
        panic!("stdout was not valid JSON: {err}\nstdout: {stdout:?}");
    })
}

#[test]
fn sensor_enable_updates_config_journal_and_log() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());

    let enable = cli()
        .current_dir(vault.path())
        .args([
            "sensor",
            "enable",
            "screen",
            "--reason",
            "operator_on",
            "--json",
        ])
        .output()
        .expect("cairn sensor enable");
    assert_eq!(
        enable.status.code(),
        Some(0),
        "enable stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&enable.stderr),
        String::from_utf8_lossy(&enable.stdout)
    );
    let enable_json = json_stdout(&enable);
    assert_eq!(enable_json["status"], "enabled");
    assert_eq!(enable_json["sensor"], "screen");

    let status = cli()
        .current_dir(vault.path())
        .args(["sensor", "status", "screen", "--json"])
        .output()
        .expect("cairn sensor status");
    assert_eq!(
        status.status.code(),
        Some(0),
        "status stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&status.stderr),
        String::from_utf8_lossy(&status.stdout)
    );
    let status_json = json_stdout(&status);
    assert_eq!(status_json["sensor"], "screen");
    assert_eq!(status_json["enabled"], true);
    assert_eq!(status_json["consent"], "enabled");

    let consent_log = vault.path().join(".cairn/consent.log");
    assert!(consent_log.exists(), "consent.log should exist");
    let log = std::fs::read_to_string(&consent_log).expect("read consent.log");
    assert!(
        log.contains("sensor_enable"),
        "missing sensor_enable: {log}"
    );
}

#[test]
fn status_json_reports_sensor_consent_state() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());

    let enable = cli()
        .current_dir(vault.path())
        .args([
            "sensor",
            "enable",
            "recording",
            "--reason",
            "operator_on",
            "--json",
        ])
        .output()
        .expect("cairn sensor enable recording");
    assert_eq!(
        enable.status.code(),
        Some(0),
        "enable stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&enable.stderr),
        String::from_utf8_lossy(&enable.stdout)
    );

    let status = cli()
        .current_dir(vault.path())
        .args(["status", "--json"])
        .output()
        .expect("cairn status");
    assert_eq!(
        status.status.code(),
        Some(0),
        "status stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&status.stderr),
        String::from_utf8_lossy(&status.stdout)
    );
    let response = json_stdout(&status);
    let sensors = response["sensors"]["local"]
        .as_array()
        .expect("local sensors");
    let recording = sensors
        .iter()
        .find(|row| row["sensor"] == "recording")
        .expect("recording row");
    assert_eq!(recording["enabled"], true);
    assert_eq!(recording["consent"], "enabled");
    assert_eq!(recording["gate"], "allowed");
}

#[test]
fn sensor_status_rejects_unbound_explicit_vault() {
    let vault = tempfile::tempdir().expect("tempdir");

    let out = cli()
        .args(["--vault"])
        .arg(vault.path())
        .args(["sensor", "status", "--json"])
        .output()
        .expect("cairn sensor status");

    assert_eq!(
        out.status.code(),
        Some(78),
        "status should reject unbound vault; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no Cairn vault"),
        "stderr should explain missing vault binding: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn sensor_enable_rejects_unbound_explicit_vault_without_creating_state() {
    let vault = tempfile::tempdir().expect("tempdir");

    let out = cli()
        .args(["--vault"])
        .arg(vault.path())
        .args([
            "sensor",
            "enable",
            "terminal",
            "--reason",
            "operator_on",
            "--json",
        ])
        .output()
        .expect("cairn sensor enable");

    assert_eq!(
        out.status.code(),
        Some(78),
        "enable should reject unbound vault; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no Cairn vault"),
        "stderr should explain missing vault binding: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!vault.path().join(".cairn/config.yaml").exists());
    assert!(!vault.path().join(".cairn/cairn.db").exists());
    assert!(!vault.path().join(".cairn/consent.log").exists());
}
