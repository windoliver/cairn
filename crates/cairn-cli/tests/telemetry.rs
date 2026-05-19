//! Telemetry fixture tests for issue #116.

use std::process::Command;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd
}

fn bootstrap_vault(vault: &std::path::Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

#[test]
fn local_verb_metric_is_body_free_by_default() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let secret = "very secret body text";
    let out = cli()
        .current_dir(vault.path())
        .args(["ingest", "--kind", "reference", "--body", secret, "--json"])
        .output()
        .expect("cairn ingest");
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let metrics =
        std::fs::read_to_string(vault.path().join(".cairn/metrics.jsonl")).expect("read metrics");
    assert!(metrics.contains("\"event\":\"verb_invocation\""));
    assert!(metrics.contains("\"verb\":\"ingest\""));
    assert!(metrics.contains("\"status\":\"committed\""));
    assert!(metrics.contains("\"mode\":\"body\""));
    assert!(!metrics.contains(secret));
    assert!(!metrics.contains("\"body\":"));
    assert!(!metrics.contains("body_text"));
}

#[test]
fn local_verb_metric_records_file_mode_without_panicking() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let source = vault.path().join("note.md");
    std::fs::write(&source, "file body text").expect("write source");

    let out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--file",
            source.to_str().expect("utf8 path"),
            "--json",
        ])
        .output()
        .expect("cairn ingest");
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let metrics =
        std::fs::read_to_string(vault.path().join(".cairn/metrics.jsonl")).expect("read metrics");
    assert!(metrics.contains("\"event\":\"verb_invocation\""));
    assert!(metrics.contains("\"verb\":\"ingest\""));
    assert!(metrics.contains("\"mode\":\"file\""));
    assert!(!metrics.contains("file body text"));
}

#[test]
fn forget_record_mode_does_not_panic() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--record", "01ARZ3NDEKTSV4RRFFQ69G5FAV", "--json"])
        .output()
        .expect("cairn forget");

    assert_ne!(
        out.status.code(),
        Some(101),
        "forget telemetry mode detection must not panic\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let metrics =
        std::fs::read_to_string(vault.path().join(".cairn/metrics.jsonl")).expect("read metrics");
    assert!(metrics.contains("\"event\":\"verb_invocation\""));
    assert!(metrics.contains("\"verb\":\"forget\""));
    assert!(metrics.contains("\"mode\":\"record\""));
}

#[test]
fn local_verb_metrics_can_be_disabled() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    std::fs::write(
        vault.path().join(".cairn/config.yaml"),
        "observability:\n  enabled: false\n",
    )
    .expect("write config");

    let out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "not exported",
            "--json",
        ])
        .output()
        .expect("cairn ingest");
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let metrics_path = vault.path().join(".cairn/metrics.jsonl");
    if metrics_path.exists() {
        let metrics = std::fs::read_to_string(metrics_path).expect("read metrics");
        assert!(!metrics.contains("\"event\":\"verb_invocation\""));
    }
}
