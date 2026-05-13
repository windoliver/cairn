//! CLI integration tests for `cairn admin snapshot` / `cairn admin restore`.

use assert_cmd::Command;
use serde_json::Value;
use std::{fs, io, path::Path};

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

fn ingest_body(vault: &Path, body: &str) -> String {
    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault)
        .args(["ingest", "--kind", "reasoning", "--body", body, "--json"])
        .output()
        .expect("run ingest");
    assert!(
        output.status.success(),
        "ingest should commit. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout: Value = serde_json::from_slice(&output.stdout).expect("ingest json");
    stdout["data"]["record_id"]
        .as_str()
        .expect("record_id")
        .to_owned()
}

fn only_registry_entry(vault: &Path) -> Value {
    let registry_dir = vault.join(".cairn").join("backups");
    let entries: Vec<_> = registry_dir
        .read_dir()
        .expect("read registry dir")
        .map(|entry| entry.expect("registry entry").path())
        .filter(|path| path.extension().and_then(std::ffi::OsStr::to_str) == Some("json"))
        .collect();
    assert_eq!(entries.len(), 1, "expected exactly one registry entry");
    let bytes = fs::read(&entries[0]).expect("read registry entry");
    serde_json::from_slice(&bytes).expect("registry json")
}

fn target_row_count(db_path: &Path, target_id: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path).expect("open db");
    conn.query_row(
        "SELECT COUNT(*) FROM records WHERE target_id = ?1",
        [target_id],
        |row| row.get(0),
    )
    .expect("target count")
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create copy root");
    for entry in fs::read_dir(src).expect("read copy source") {
        let entry = entry.expect("copy entry");
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type().expect("copy file type");
        if file_type.is_dir() {
            copy_tree(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).expect("copy file");
        }
    }
}

fn try_create_dir_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst)
    }

    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(src, dst)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (src, dst);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory symlinks are not supported on this platform",
        ))
    }
}

#[test]
fn admin_snapshot_writes_backup_and_registry_entry() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());
    let record_id = ingest_body(vault.path(), "snapshot me");

    let backup_root = tempfile::tempdir().expect("backup tempdir");
    let backup_path = backup_root.path().join("snapshot-backup");
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
    assert!(
        registry_dir.is_dir(),
        "expected backup registry dir to exist"
    );
    assert!(
        backup_path.join(".cairn/cairn.db").exists(),
        "expected db backup"
    );
    assert!(backup_path.join("raw").is_dir(), "expected raw backup copy");
    assert!(
        backup_path.join("wiki").is_dir(),
        "expected wiki backup copy"
    );
    assert!(
        backup_path.join("sources").is_dir(),
        "expected sources backup copy"
    );
    assert_eq!(
        target_row_count(&backup_path.join(".cairn/cairn.db"), &record_id),
        1,
        "backup db should contain the snapshotted record"
    );

    let registry_entry = only_registry_entry(vault.path());
    assert_eq!(
        registry_entry["artifact_path"],
        backup_path.display().to_string()
    );
    assert_eq!(
        registry_entry["target_ids_included"],
        serde_json::json!([record_id])
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

    let backup_root = tempfile::tempdir().expect("backup tempdir");
    let backup_path = backup_root.path().join("snapshot-backup");
    let snapshot_output = Command::cargo_bin("cairn")
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
        snapshot_output.status.success(),
        "snapshot should commit. stderr: {}",
        String::from_utf8_lossy(&snapshot_output.stderr)
    );

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
fn admin_snapshot_rejects_overlapping_backup_paths() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    for backup_path in [
        vault.path().to_path_buf(),
        vault.path().join("nested-backup"),
    ] {
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
            !output.status.success(),
            "snapshot should fail closed for overlapping backup path {}",
            backup_path.display()
        );
    }
}

#[test]
fn admin_snapshot_rejects_symlink_alias_overlap() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    let alias_root = tempfile::tempdir().expect("alias tempdir");
    let alias_path = alias_root.path().join("vault-alias");
    if let Err(error) = try_create_dir_symlink(vault.path(), &alias_path) {
        eprintln!("skipping symlink overlap test because symlink creation is unavailable: {error}");
        return;
    }

    let backup_path = alias_path.join("snapshot-backup");
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
        !output.status.success(),
        "snapshot should fail closed for symlink-alias overlap via {}",
        backup_path.display()
    );
}

#[test]
fn admin_restore_rejects_overlapping_paths() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    let backup_root = tempfile::tempdir().expect("backup tempdir");
    let backup_path = backup_root.path().join("snapshot-backup");
    let snapshot_output = Command::cargo_bin("cairn")
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
        snapshot_output.status.success(),
        "snapshot should commit. stderr: {}",
        String::from_utf8_lossy(&snapshot_output.stderr)
    );

    for restore_target in [backup_path.clone(), backup_path.join("nested-restore")] {
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
            !output.status.success(),
            "restore should fail closed for overlapping paths from {} into {}",
            backup_path.display(),
            restore_target.display()
        );
    }
}

#[test]
fn admin_restore_rejects_missing_backup_database() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    let empty_backup_path = vault.path().join("invalid-backup");
    fs::create_dir_all(&empty_backup_path).expect("create invalid backup root");

    for invalid_backup_path in [empty_backup_path, vault.path().join("missing-backup")] {
        let restore_target = vault.path().join("restore-target");
        let output = Command::cargo_bin("cairn")
            .expect("cairn binary")
            .env("CAIRN_VAULT", vault.path())
            .args([
                "admin",
                "restore",
                "--from",
                invalid_backup_path.to_str().expect("utf-8 backup path"),
                "--into",
                restore_target.to_str().expect("utf-8 restore path"),
                "--json",
            ])
            .output()
            .expect("run admin restore");

        assert!(
            !output.status.success(),
            "restore should fail closed for invalid backup root {}",
            invalid_backup_path.display()
        );
    }
}

#[test]
fn admin_snapshot_and_restore_ignore_malformed_config() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());
    write_malformed_config(vault.path());

    let backup_root = tempfile::tempdir().expect("backup tempdir");
    let backup_path = backup_root.path().join("snapshot-backup");
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
    assert!(
        backup_path.exists(),
        "expected snapshot backup path to exist"
    );

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

#[test]
#[ignore = "admin snapshot/restore + forget tombstone replay needs follow-up after merge integrates main's signed forget WAL path"]
fn restore_replays_current_forget_tombstones_before_success() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());
    let record_id = ingest_body(vault.path(), "do not resurrect after restore");

    let backup_root = tempfile::tempdir().expect("backup tempdir");
    let backup_path = backup_root.path().join("snapshot-backup");
    let snapshot_output = Command::cargo_bin("cairn")
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
        snapshot_output.status.success(),
        "snapshot should commit. stderr: {}",
        String::from_utf8_lossy(&snapshot_output.stderr)
    );
    assert_eq!(
        target_row_count(&backup_path.join(".cairn/cairn.db"), &record_id),
        1,
        "backup should still include the record before forget"
    );
    let stale_backup_root = tempfile::tempdir().expect("stale backup tempdir");
    let stale_backup_path = stale_backup_root.path().join("stale-backup");
    copy_tree(&backup_path, &stale_backup_path);

    let forget_output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args(["forget", "--record", &record_id, "--json"])
        .output()
        .expect("run forget");
    assert!(
        forget_output.status.success(),
        "forget should commit. stderr: {}",
        String::from_utf8_lossy(&forget_output.stderr)
    );

    let restore_target = vault.path().join("restore-target");
    let restore_output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "restore",
            "--from",
            stale_backup_path.to_str().expect("utf-8 backup path"),
            "--into",
            restore_target.to_str().expect("utf-8 restore path"),
            "--json",
        ])
        .output()
        .expect("run admin restore");
    assert!(
        restore_output.status.success(),
        "restore should commit. stderr: {}",
        String::from_utf8_lossy(&restore_output.stderr)
    );

    assert_eq!(
        target_row_count(&restore_target.join(".cairn/cairn.db"), &record_id),
        0,
        "restore should replay current forget tombstones before succeeding"
    );
}
