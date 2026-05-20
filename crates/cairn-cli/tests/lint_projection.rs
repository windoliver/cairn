//! Integration tests for `cairn lint` projection diagnostics.

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn write_nexus_config(vault: &std::path::Path) {
    let dir = vault.join(".cairn");
    std::fs::create_dir_all(&dir).expect("create .cairn");
    std::fs::write(
        dir.join("config.yaml"),
        "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: http://127.0.0.1:1\n",
    )
    .expect("write config");
}

#[test]
fn lint_json_reports_projection_sidecar_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_nexus_config(dir.path());

    let out = cli()
        .current_dir(dir.path())
        .args(["lint", "--json"])
        .output()
        .expect("cairn lint --json");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("projection_sidecar_unavailable"),
        "{stdout}"
    );
}

#[test]
fn lint_fix_mentions_projection_rebuild() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_nexus_config(dir.path());

    let out = cli()
        .current_dir(dir.path())
        .args(["lint", "--fix", "--json"])
        .output()
        .expect("cairn lint --fix --json");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("\"rebuildable\":true"), "{stdout}");
}

#[test]
fn status_json_reports_projection_detail_for_nexus() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_nexus_config(dir.path());

    let out = cli()
        .current_dir(dir.path())
        .args(["status", "--json"])
        .output()
        .expect("cairn status --json");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert!(
        value["health"]["nexus_projection"]["projection_detail"].is_object(),
        "{stdout}"
    );
}
