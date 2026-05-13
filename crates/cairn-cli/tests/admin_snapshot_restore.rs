//! CLI integration tests for `cairn admin snapshot` / `cairn admin restore`.

use assert_cmd::Command;
use serde_json::Value;

fn bootstrap_vault(vault: &std::path::Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn write_malformed_config(vault: &std::path::Path) {
    std::fs::write(vault.join(".cairn").join("config.yaml"), "not: [valid\n")
        .expect("write malformed config");
}

#[test]
fn admin_snapshot_writes_backup_and_registry_entry() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    let backup_path = vault.path().join("snapshot-backup");
    let registry_dir = vault.path().join(".cairn").join("backups");

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "snapshot",
            "--backup",
            backup_path.to_str().expect("utf-8 backup path"),
            "--json",
        ])
        .output()
        .expect("run admin snapshot");

    assert!(
        output.status.success(),
        "snapshot exited non-zero. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(backup_path.exists(), "expected backup path to exist");
    assert!(registry_dir.is_dir(), "expected backup registry dir to exist");
    assert!(
        registry_dir.read_dir().expect("read registry dir").next().is_some(),
        "expected snapshot to create at least one registry entry"
    );
}

#[test]
fn admin_restore_rejects_missing_backup_argument() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    let restore_target = vault.path().join("restore-target");
    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "restore",
            "--into",
            restore_target.to_str().expect("utf-8 restore path"),
        ])
        .output()
        .expect("run admin restore");

    assert_eq!(
        output.status.code(),
        Some(64),
        "expected clap EX_USAGE. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("--from <PATH>"),
        "expected missing --from diagnostic, got: {stderr}"
    );
}

#[test]
fn admin_restore_json_reports_accepted_restore() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    let backup_path = vault.path().join("snapshot-backup");
    std::fs::create_dir_all(&backup_path).expect("create backup path");

    let restore_target = vault.path().join("restore-target");
    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "restore",
            "--from",
            backup_path.to_str().expect("utf-8 backup path"),
            "--into",
            restore_target.to_str().expect("utf-8 restore path"),
            "--json",
        ])
        .output()
        .expect("run admin restore");

    assert!(
        output.status.success(),
        "restore exited non-zero. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout: Value = serde_json::from_slice(&output.stdout).expect("restore json");
    assert_eq!(stdout["from"], backup_path.display().to_string());
    assert_eq!(stdout["into"], restore_target.display().to_string());
    assert_eq!(stdout["status"], "accepted");
}

#[test]
fn admin_snapshot_and_restore_ignore_malformed_config() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());
    write_malformed_config(vault.path());

    let backup_path = vault.path().join("snapshot-backup");
    let snapshot_output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "snapshot",
            "--backup",
            backup_path.to_str().expect("utf-8 backup path"),
        ])
        .output()
        .expect("run admin snapshot");

    assert!(
        snapshot_output.status.success(),
        "snapshot should ignore malformed config. stderr: {}",
        String::from_utf8_lossy(&snapshot_output.stderr)
    );
    assert!(backup_path.exists(), "expected snapshot backup path to exist");

    let restore_target = vault.path().join("restore-target");
    let restore_output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "restore",
            "--from",
            backup_path.to_str().expect("utf-8 backup path"),
            "--into",
            restore_target.to_str().expect("utf-8 restore path"),
        ])
        .output()
        .expect("run admin restore");

    assert!(
        restore_output.status.success(),
        "restore should ignore malformed config. stderr: {}",
        String::from_utf8_lossy(&restore_output.stderr)
    );
    let stdout = String::from_utf8(restore_output.stdout).expect("utf8 stdout");
    assert!(
        stdout.contains("accepted restore"),
        "expected restore success output, got: {stdout}"
    );
}
