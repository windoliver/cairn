//! Lint coverage for local sensor drop metrics.

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

fn initialize_store(vault: &Path) {
    let out = cli()
        .current_dir(vault)
        .args([
            "sensor",
            "enable",
            "hook",
            "--reason",
            "test_enable",
            "--json",
        ])
        .output()
        .expect("cairn sensor enable hook");
    assert_eq!(
        out.status.code(),
        Some(0),
        "sensor enable failed; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn json_stdout(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
        panic!("stdout was not valid JSON: {err}\nstdout: {stdout:?}");
    })
}

#[test]
fn lint_reports_sensor_drop_metrics() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());
    initialize_store(vault.path());
    let metrics_path = vault.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics_path,
        concat!(
            r#"{"event":"sensor_drop","sensor":"clipboard","reason":"privacy_denied","stage":"pre_extraction"}"#,
            "\n",
            r#"{"event":"sensor_drop","sensor":"screen","reason":"budget_exceeded","stage":"pre_capture"}"#,
            "\n",
        ),
    )
    .expect("write metrics");

    let out = cli()
        .current_dir(vault.path())
        .args(["lint", "--json"])
        .output()
        .expect("cairn lint --json");
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "lint failed unexpectedly; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let response = json_stdout(&out);
    let findings = response["data"]["findings"]
        .as_array()
        .expect("findings array");
    assert!(
        findings
            .iter()
            .any(|finding| finding["kind"] == "sensor_privacy_denied"),
        "missing sensor_privacy_denied finding: {findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding["kind"] == "sensor_budget_exceeded"),
        "missing sensor_budget_exceeded finding: {findings:?}"
    );
}
