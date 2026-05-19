//! End-to-end tests for the operator-facing `cairn nexus` UX.

use std::process::Command;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
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
fn nexus_help_lists_setup_and_doctor() {
    let out = cli()
        .args(["nexus", "--help"])
        .output()
        .expect("cairn nexus --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("setup"), "missing setup: {stdout}");
    assert!(stdout.contains("doctor"), "missing doctor: {stdout}");
}

#[test]
fn nexus_setup_json_is_guided_and_non_mutating_by_default() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = cli()
        .current_dir(dir.path())
        .args(["nexus", "setup", "--json"])
        .output()
        .expect("cairn nexus setup --json");

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        out.status.success(),
        "exit: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["status"], "guidance");
    assert_eq!(v["auto_install"], false);
    assert_eq!(v["recommended"]["data_dir"], "nexus-data");
    assert!(
        v["recommended"]["command"]
            .as_str()
            .is_some_and(|command| command.ends_with("nexusd")),
        "recommended command should point at nexusd: {stdout}"
    );
    assert_eq!(v["recommended"]["health_timeout_ms"], 120_000);
    assert_eq!(v["recommended"]["shutdown_timeout_ms"], 2_000);
    assert!(
        v["message"]
            .as_str()
            .is_some_and(|message| message.contains("No changes made")),
        "missing no-mutation message: {stdout}"
    );
    let install_steps = v["install_steps"].as_array().expect("install_steps array");
    assert!(
        install_steps.iter().any(|step| step
            .as_str()
            .is_some_and(|step| step.contains("nexus-ai-fs[sandbox]"))),
        "missing explicit install guidance: {stdout}"
    );
    assert!(
        install_steps.iter().any(|step| step
            .as_str()
            .is_some_and(|step| step.contains("~/nexus/.venv"))),
        "missing ~/nexus venv guidance: {stdout}"
    );
    let remediation = v["remediation"].as_array().expect("remediation array");
    assert!(
        remediation
            .iter()
            .any(|step| step.as_str().is_some_and(|step| step.contains("nexusd"))),
        "missing nexusd remediation: {stdout}"
    );
    assert!(
        remediation.iter().any(|step| step
            .as_str()
            .is_some_and(|step| step.contains("store.nexus.command"))),
        "missing config remediation: {stdout}"
    );
    assert!(
        !dir.path().join(".cairn").exists(),
        "setup guidance must not create vault config"
    );
}

#[test]
fn nexus_doctor_json_reports_degraded_with_setup_hint() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_config(
        dir.path(),
        "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: http://127.0.0.1:0\n    health_timeout_ms: 25\n    shutdown_timeout_ms: 25\n",
    );

    let out = cli()
        .current_dir(dir.path())
        .args(["nexus", "doctor", "--json"])
        .output()
        .expect("cairn nexus doctor --json");

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert_eq!(
        out.status.code(),
        Some(69),
        "exit: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["status"], "degraded");
    assert_eq!(v["command"], "nexusd");
    assert_eq!(v["endpoint"], "http://127.0.0.1:0");
    assert!(
        v["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("cairn nexus setup")
                && reason.contains("store.nexus.command")
                && reason.contains("nexusd")),
        "missing setup hint: {stdout}"
    );
    assert!(
        !dir.path().join("nexus-data").exists(),
        "doctor must not create projection directories"
    );
}
