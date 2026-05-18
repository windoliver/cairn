//! CLI integration tests for `cairn admin snapshot` / `cairn admin restore`.

use assert_cmd::Command;
use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::{RecordId, ScopeTuple, TargetId};
use cairn_test_fixtures::store::sample_record;
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

async fn seed_session_record(vault: &Path, target_id: &str, session_id: &str) {
    let db_path = vault.join(".cairn").join("cairn.db");
    let store = cairn_store_sqlite::open(&db_path)
        .await
        .expect("open store");
    let fixture = sample_record();
    let mut record = sample_record();
    record.id = RecordId::parse(target_id).expect("valid record id");
    record.target_id = TargetId::parse(target_id).expect("valid target id");
    record.body = format!("session {session_id} body {target_id}");
    record.scope = ScopeTuple {
        session_id: Some(session_id.to_owned()),
        user: fixture.scope.user.clone(),
        agent: fixture.scope.agent.clone(),
        ..ScopeTuple::default()
    };
    store.upsert(&record).await.expect("upsert session record");
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

fn registry_entries(vault: &Path) -> Vec<Value> {
    let registry_dir = vault.join(".cairn").join("backups");
    let mut entries: Vec<_> = registry_dir
        .read_dir()
        .expect("read registry dir")
        .map(|entry| entry.expect("registry entry").path())
        .filter(|path| path.extension().and_then(std::ffi::OsStr::to_str) == Some("json"))
        .collect();
    entries.sort();
    entries
        .into_iter()
        .map(|path| serde_json::from_slice(&fs::read(path).expect("read registry entry")).unwrap())
        .collect()
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

fn digest_string(value: &Value) -> String {
    value["file_digest"]
        .as_str()
        .expect("registry entry must include file_digest")
        .to_owned()
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
    assert_eq!(registry_entry["backup_kind"], "snapshot");
    assert!(
        registry_entry["file_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "registry entry must include a sha256 digest: {registry_entry}"
    );
}

#[test]
fn backup_cli_lists_registers_and_forgets_registry_entries() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());
    let snapshot_record = ingest_body(vault.path(), "snapshot registry entry");

    let backup_root = tempfile::tempdir().expect("backup tempdir");
    let snapshot_path = backup_root.path().join("snapshot-backup");
    let snapshot_output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "snapshot",
            "--backup",
            snapshot_path.to_str().expect("utf-8 backup path"),
            "--json",
        ])
        .output()
        .expect("run admin snapshot");
    assert!(
        snapshot_output.status.success(),
        "snapshot should commit. stderr: {}",
        String::from_utf8_lossy(&snapshot_output.stderr)
    );

    let imported_path = backup_root.path().join("imported-backup");
    copy_tree(&snapshot_path, &imported_path);
    fs::write(
        imported_path.join("wiki/imported-marker.txt"),
        "operator import marker",
    )
    .expect("make imported backup digest distinct");
    let register_output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "backup",
            "register",
            imported_path.to_str().expect("utf-8 backup path"),
            "--kind",
            "export",
            "--json",
        ])
        .output()
        .expect("run backup register");
    assert!(
        register_output.status.success(),
        "backup register should commit. stderr: {}",
        String::from_utf8_lossy(&register_output.stderr)
    );

    let list_output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args(["backup", "list", "--json"])
        .output()
        .expect("run backup list");
    assert!(
        list_output.status.success(),
        "backup list should commit. stderr: {}",
        String::from_utf8_lossy(&list_output.stderr)
    );
    let listed: Value = serde_json::from_slice(&list_output.stdout).expect("backup list json");
    assert_eq!(
        listed["backups"].as_array().expect("backups array").len(),
        2
    );
    assert!(
        listed["backups"].as_array().unwrap().iter().any(|entry| {
            entry["artifact_path"] == imported_path.display().to_string()
                && entry["backup_kind"] == "export"
                && entry["target_ids_included"] == serde_json::json!([snapshot_record])
        }),
        "registered backup must appear in list output: {listed}"
    );

    let imported_digest = digest_string(
        listed["backups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["artifact_path"] == imported_path.display().to_string())
            .expect("imported backup entry"),
    );
    let forget_output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args(["backup", "forget", &imported_digest, "--json"])
        .output()
        .expect("run backup forget");
    assert!(
        forget_output.status.success(),
        "backup forget should commit. stderr: {}",
        String::from_utf8_lossy(&forget_output.stderr)
    );

    let entries = registry_entries(vault.path());
    assert_eq!(
        entries.len(),
        1,
        "backup forget must remove one registry entry"
    );
    assert_ne!(digest_string(&entries[0]), imported_digest);
}

#[test]
fn forget_record_replays_tombstone_into_registered_backup() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());
    let record_id = ingest_body(vault.path(), "backup replay must remove me");

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
        "backup should contain record before forget"
    );

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

    assert_eq!(
        target_row_count(&backup_path.join(".cairn/cairn.db"), &record_id),
        0,
        "forget Phase B must replay tombstones into registered backups"
    );
    let registry_entry = only_registry_entry(vault.path());
    assert_eq!(registry_entry["target_ids_included"], serde_json::json!([]));
    assert!(
        vault.path().join(".cairn/backups/shredded.log").is_file(),
        "forget must leave an audit receipt for the superseded backup"
    );

    let restore_target = vault.path().join("restore-after-forget");
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
        "restoring a registered backup after forget must not resurrect forgotten content"
    );
}

#[tokio::test]
async fn forget_session_replays_tombstones_into_registered_backup() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());
    let first = "01HQZX9F5N0000000000000001";
    let second = "01HQZX9F5N0000000000000002";
    seed_session_record(vault.path(), first, "sess-backup-replay").await;
    seed_session_record(vault.path(), second, "sess-backup-replay").await;

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

    let forget_output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args(["forget", "--session", "sess-backup-replay", "--json"])
        .output()
        .expect("run forget session");
    assert!(
        forget_output.status.success(),
        "session forget should commit. stderr: {}",
        String::from_utf8_lossy(&forget_output.stderr)
    );

    assert_eq!(
        target_row_count(&backup_path.join(".cairn/cairn.db"), first),
        0
    );
    assert_eq!(
        target_row_count(&backup_path.join(".cairn/cairn.db"), second),
        0
    );
    let registry_entry = only_registry_entry(vault.path());
    assert_eq!(registry_entry["target_ids_included"], serde_json::json!([]));
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
#[ignore = "tracks stale unregistered backup replay; issue 160 covers registered backup tombstone replay"]
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
