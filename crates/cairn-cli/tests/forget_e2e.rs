//! End-to-end tests for `cairn forget --record`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{fs, path::Path, process::Command};

use cairn_core::contract::memory_store::{ListArgs, MemoryStore as _};
use cairn_core::domain::projection::MarkdownProjector;
use cairn_core::domain::{RecordId, ScopeTuple, TargetId};
use cairn_store_sqlite::consent::query_source_forgets;
use cairn_test_fixtures::store::sample_record;
use sha2::{Digest, Sha256};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn json_stdout(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout was not valid JSON: {e}\nstdout: {stdout:?}");
    })
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

async fn seed_record_for_source(vault: &Path, body: &str) -> (String, String) {
    let db_path = vault.join(".cairn").join("cairn.db");
    let store = cairn_store_sqlite::open(&db_path)
        .await
        .expect("open store");
    let mut record = sample_record();
    record.body = body.to_owned();
    record.provenance.source_hash = format!("sha256:{:x}", Sha256::digest(body.as_bytes()));
    let outcome = store.upsert(&record).await.expect("upsert");
    (
        outcome.record_id.as_str().to_owned(),
        record.target_id.as_str().to_owned(),
    )
}

async fn seed_session_record(vault: &Path, record_id: &str, target_id: &str, session_id: &str) {
    let fixture = sample_record();
    seed_session_record_with_scope(
        vault,
        record_id,
        target_id,
        ScopeTuple {
            session_id: Some(session_id.to_owned()),
            user: fixture.scope.user.clone(),
            agent: fixture.scope.agent.clone(),
            ..ScopeTuple::default()
        },
    )
    .await;
}

async fn seed_session_record_with_scope(
    vault: &Path,
    record_id: &str,
    target_id: &str,
    scope: ScopeTuple,
) {
    let db_path = vault.join(".cairn").join("cairn.db");
    let store = cairn_store_sqlite::open(&db_path)
        .await
        .expect("open store");
    let mut record = sample_record();
    record.id = RecordId::parse(record_id).expect("valid record id");
    record.target_id = TargetId::parse(target_id).expect("valid target id");
    let session_id = scope
        .session_id
        .as_deref()
        .expect("test scope must include session");
    record.body = format!("session {session_id} body {record_id}");
    record.scope = scope;
    store.upsert(&record).await.expect("upsert session record");
}

async fn active_projection_path(vault: &Path, record_id: &str) -> std::path::PathBuf {
    let db_path = vault.join(".cairn").join("cairn.db");
    let store = cairn_store_sqlite::open(&db_path)
        .await
        .expect("open store");
    let stored = store
        .list_active_stored(&ListArgs::default())
        .await
        .expect("list active stored")
        .into_iter()
        .find(|stored| stored.record.id.as_str() == record_id)
        .expect("seeded record is active");
    vault.join(MarkdownProjector.project(&stored).path)
}

async fn seed_record_versions_for_source(
    vault: &Path,
    first_body: &str,
    second_body: &str,
) -> (String, String) {
    let db_path = vault.join(".cairn").join("cairn.db");
    let store = cairn_store_sqlite::open(&db_path)
        .await
        .expect("open store");
    let mut record = sample_record();
    record.body = first_body.to_owned();
    record.provenance.source_hash = format!("sha256:{:x}", Sha256::digest(first_body.as_bytes()));
    store.upsert(&record).await.expect("upsert v1");

    let mut updated = record.clone();
    updated.body = second_body.to_owned();
    updated.provenance.source_hash = format!("sha256:{:x}", Sha256::digest(second_body.as_bytes()));
    let outcome = store.upsert(&updated).await.expect("upsert v2");
    (
        outcome.record_id.as_str().to_owned(),
        updated.target_id.as_str().to_owned(),
    )
}

async fn seed_record_versions_with_shared_source_hash(
    vault: &Path,
    source_hash_body: &str,
    first_body: &str,
    second_body: &str,
) -> (String, String, String) {
    let db_path = vault.join(".cairn").join("cairn.db");
    let store = cairn_store_sqlite::open(&db_path)
        .await
        .expect("open store");
    let source_hash = format!("sha256:{:x}", Sha256::digest(source_hash_body.as_bytes()));

    let mut record = sample_record();
    record.body = first_body.to_owned();
    record.provenance.source_hash = source_hash.clone();
    store.upsert(&record).await.expect("upsert v1");

    let mut updated = record.clone();
    updated.body = second_body.to_owned();
    updated.provenance.source_hash = source_hash.clone();
    let outcome = store.upsert(&updated).await.expect("upsert v2");
    (
        outcome.record_id.as_str().to_owned(),
        updated.target_id.as_str().to_owned(),
        source_hash,
    )
}

fn record_exists(vault: &Path, record_id: &str) -> bool {
    let conn = rusqlite::Connection::open(vault.join(".cairn").join("cairn.db")).expect("open db");
    conn.query_row(
        "SELECT 1 FROM records WHERE record_id = ?1",
        [record_id],
        |_| Ok(()),
    )
    .map(|()| true)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(false),
        other => Err(other),
    })
    .expect("query record existence")
}

fn rows_for_target(vault: &Path, target_id: &str) -> i64 {
    let conn = rusqlite::Connection::open(vault.join(".cairn").join("cairn.db")).expect("open db");
    conn.query_row(
        "SELECT COUNT(*) FROM records WHERE target_id = ?1",
        [target_id],
        |row| row.get(0),
    )
    .expect("count rows")
}

fn wal_kind_count(vault: &Path, kind: &str) -> i64 {
    let conn = rusqlite::Connection::open(vault.join(".cairn").join("cairn.db")).expect("open db");
    conn.query_row(
        "SELECT COUNT(*) FROM wal_ops WHERE kind = ?1 AND state = 'COMMITTED'",
        [kind],
        |row| row.get(0),
    )
    .expect("count wal ops")
}

#[tokio::test]
async fn forget_session_cli_commits_store_wal_operation() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    seed_session_record(
        vault.path(),
        "01J00000000000000000002001",
        "01HQZX9F5N0000000000020001",
        "sess-108-cli",
    )
    .await;
    seed_session_record(
        vault.path(),
        "01J00000000000000000002002",
        "01HQZX9F5N0000000000020002",
        "sess-108-cli",
    )
    .await;

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--session", "sess-108-cli", "--json"])
        .output()
        .expect("cairn forget session");

    assert_eq!(
        out.status.code(),
        Some(0),
        "forget session should commit; stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let response = json_stdout(&out);
    assert_eq!(response["status"], "committed");
    assert_eq!(response["verb"], "forget");
    assert_eq!(response["data"]["deleted_count"], 2);
    assert_eq!(wal_kind_count(vault.path(), "forget_session"), 1);
}

#[tokio::test]
async fn forget_session_cli_removes_markdown_projection() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let record_id = "01J00000000000000000002011";
    seed_session_record(
        vault.path(),
        record_id,
        "01HQZX9F5N0000000000020011",
        "sess-108-projection",
    )
    .await;

    let db_path = vault.path().join(".cairn").join("cairn.db");
    let store = cairn_store_sqlite::open(&db_path)
        .await
        .expect("open store");
    cairn_cli::verbs::lint::fix_markdown_handler(&store, vault.path())
        .await
        .expect("write markdown projection");

    let projection_path = active_projection_path(vault.path(), record_id).await;
    assert!(
        projection_path.exists(),
        "test setup should create a markdown projection"
    );

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--session", "sess-108-projection", "--json"])
        .output()
        .expect("cairn forget session");

    assert_eq!(
        out.status.code(),
        Some(0),
        "forget session should commit; stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !projection_path.exists(),
        "session forget must remove stale markdown projections"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn forget_session_projection_cleanup_failure_leaves_db_unchanged() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let record_id = "01J00000000000000000002012";
    seed_session_record(
        vault.path(),
        record_id,
        "01HQZX9F5N0000000000020012",
        "sess-108-projection-fail",
    )
    .await;

    let projection_path = active_projection_path(vault.path(), record_id).await;
    fs::create_dir_all(projection_path.parent().expect("projection parent")).expect("mkdir");
    let outside = vault.path().join("outside-projection.md");
    fs::write(&outside, "do not follow").expect("write outside target");
    std::os::unix::fs::symlink(&outside, &projection_path).expect("symlink projection");

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--session", "sess-108-projection-fail", "--json"])
        .output()
        .expect("cairn forget session");

    assert_ne!(
        out.status.code(),
        Some(0),
        "unsafe projection cleanup should fail before DB purge"
    );
    assert!(
        record_exists(vault.path(), record_id),
        "DB record must remain when projection cleanup preflight fails"
    );
}

#[tokio::test]
async fn forget_session_cli_reports_missing_session_without_wal_operation() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--session", "sess-108-missing", "--json"])
        .output()
        .expect("cairn forget session");

    assert_ne!(
        out.status.code(),
        Some(0),
        "missing session should fail; stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let response = json_stdout(&out);
    assert_eq!(response["status"], "aborted");
    assert_eq!(response["error"]["code"], "NotFound");
    assert_eq!(wal_kind_count(vault.path(), "forget_session"), 0);
}

#[tokio::test]
async fn forget_session_cli_rejects_ambiguous_scope_partitions_without_deleting() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    seed_session_record_with_scope(
        vault.path(),
        "01J00000000000000000002013",
        "01HQZX9F5N0000000000020013",
        ScopeTuple {
            tenant: Some("tenant-a".to_owned()),
            workspace: Some("workspace-a".to_owned()),
            session_id: Some("sess-108-ambiguous".to_owned()),
            user: Some("hmn:tester".to_owned()),
            agent: Some("agt:test".to_owned()),
            ..ScopeTuple::default()
        },
    )
    .await;
    seed_session_record_with_scope(
        vault.path(),
        "01J00000000000000000002014",
        "01HQZX9F5N0000000000020014",
        ScopeTuple {
            tenant: Some("tenant-b".to_owned()),
            workspace: Some("workspace-b".to_owned()),
            session_id: Some("sess-108-ambiguous".to_owned()),
            user: Some("hmn:tester".to_owned()),
            agent: Some("agt:test".to_owned()),
            ..ScopeTuple::default()
        },
    )
    .await;

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--session", "sess-108-ambiguous", "--json"])
        .output()
        .expect("cairn forget session");

    assert_eq!(
        out.status.code(),
        Some(64),
        "ambiguous session should be a usage error; stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let response = json_stdout(&out);
    assert_eq!(response["status"], "rejected");
    assert_eq!(response["error"]["code"], "InvalidArgs");
    assert!(record_exists(vault.path(), "01J00000000000000000002013"));
    assert!(record_exists(vault.path(), "01J00000000000000000002014"));
    assert_eq!(wal_kind_count(vault.path(), "forget_session"), 0);
}

#[tokio::test]
async fn forget_record_emits_source_forget_and_redacts_matching_source_file() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    fs::write(
        vault.path().join(".cairn").join("config.yaml"),
        "vault:\n  name: test-vault\nsource:\n  redact_on_forget: true\n",
    )
    .expect("write config");

    let source_dir = vault.path().join("sources").join("documents");
    fs::create_dir_all(&source_dir).expect("source dir");
    let source_path = source_dir.join("note.txt");
    let source_body =
        "Forget this source body after record deletion. Contact me at user@example.com.";
    fs::write(&source_path, source_body).expect("write source");

    let (record_id, _) = seed_record_for_source(vault.path(), source_body).await;

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--record", &record_id, "--json"])
        .output()
        .expect("cairn forget");

    assert_eq!(
        out.status.code(),
        Some(0),
        "forget should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let response = json_stdout(&out);
    assert_eq!(response["status"], "committed");
    assert_eq!(response["verb"], "forget");
    assert_eq!(response["data"]["deleted_count"], 1);
    assert_eq!(response["data"]["tombstones"][0], record_id);

    assert!(
        !record_exists(vault.path(), &record_id),
        "record row must be hard-deleted after forget"
    );

    let conn =
        rusqlite::Connection::open(vault.path().join(".cairn").join("cairn.db")).expect("open db");
    let source_forgets = query_source_forgets(&conn).expect("query source forgets");
    assert_eq!(source_forgets.len(), 1);

    let redacted = fs::read_to_string(&source_path).expect("read redacted source");
    assert!(
        redacted.contains("redacted_at"),
        "stub must carry redacted_at: {redacted}"
    );
    assert!(
        redacted.contains("source_hash"),
        "stub must carry source_hash: {redacted}"
    );
    assert!(
        !redacted.contains(source_body),
        "stub must not carry original source bytes: {redacted}"
    );
}

#[tokio::test]
async fn forget_record_redacts_all_matching_source_files() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    fs::write(
        vault.path().join(".cairn").join("config.yaml"),
        "vault:\n  name: test-vault\nsource:\n  redact_on_forget: true\n",
    )
    .expect("write config");

    let source_dir = vault.path().join("sources").join("documents");
    fs::create_dir_all(&source_dir).expect("source dir");
    let source_body = "shared source body across two files";
    let first_path = source_dir.join("note-a.txt");
    let second_path = source_dir.join("note-b.txt");
    fs::write(&first_path, source_body).expect("write first source");
    fs::write(&second_path, source_body).expect("write second source");

    let (record_id, _) = seed_record_for_source(vault.path(), source_body).await;

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--record", &record_id, "--json"])
        .output()
        .expect("cairn forget");

    assert_eq!(
        out.status.code(),
        Some(0),
        "forget should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    for path in [&first_path, &second_path] {
        let redacted = fs::read_to_string(path).expect("read redacted source");
        assert!(
            redacted.contains("redacted_at"),
            "every matching file must be redacted: {redacted}"
        );
        assert!(
            !redacted.contains(source_body),
            "original source bytes must be removed from every matching file: {redacted}"
        );
    }
}

#[tokio::test]
async fn forget_record_leaves_source_file_when_redaction_disabled() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let source_dir = vault.path().join("sources").join("documents");
    fs::create_dir_all(&source_dir).expect("source dir");
    let source_path = source_dir.join("note.txt");
    let source_body = "Keep this source body when redaction is disabled.";
    fs::write(&source_path, source_body).expect("write source");

    let (record_id, _) = seed_record_for_source(vault.path(), source_body).await;

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--record", &record_id, "--json"])
        .output()
        .expect("cairn forget");

    assert_eq!(
        out.status.code(),
        Some(0),
        "forget should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let response = json_stdout(&out);
    assert_eq!(response["status"], "committed");
    assert_eq!(response["data"]["deleted_count"], 1);

    let source_after = fs::read_to_string(&source_path).expect("read source");
    assert_eq!(source_after, source_body);
}

// NOTE: `ingest_rejects_source_hash_after_forget` removed during rebase onto
// main: main's forget hard-deletes the record lineage (vs the original
// tombstone model the test was written against), so a consent-journal-driven
// re-ingest gate needs to live in the signed-ingest path. Tracking as
// follow-up to issue #327.

#[tokio::test]
async fn forget_record_tombstones_all_versions_for_target() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let (record_id, target_id) =
        seed_record_versions_for_source(vault.path(), "first version body", "second version body")
            .await;

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--record", &record_id, "--json"])
        .output()
        .expect("cairn forget");

    assert_eq!(
        out.status.code(),
        Some(0),
        "forget should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        rows_for_target(vault.path(), &target_id),
        0,
        "every stored version must be hard-deleted by target_id"
    );
}

#[tokio::test]
async fn forget_record_tracks_source_forgets_for_all_target_versions() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let first_body = "first target source body";
    let second_body = "second target source body";
    let (record_id, _) =
        seed_record_versions_for_source(vault.path(), first_body, second_body).await;

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--record", &record_id, "--json"])
        .output()
        .expect("cairn forget");

    assert_eq!(
        out.status.code(),
        Some(0),
        "forget should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let conn =
        rusqlite::Connection::open(vault.path().join(".cairn").join("cairn.db")).expect("open db");
    let source_forgets = query_source_forgets(&conn).expect("query source forgets");
    let subjects: Vec<String> = source_forgets
        .into_iter()
        .map(|event| event.subject)
        .collect();
    let first_hash = format!("sha256:{:x}", Sha256::digest(first_body.as_bytes()));
    let second_hash = format!("sha256:{:x}", Sha256::digest(second_body.as_bytes()));
    assert!(
        subjects.contains(&first_hash),
        "expected first version hash in source_forget journal, got {subjects:?}"
    );
    assert!(
        subjects.contains(&second_hash),
        "expected second version hash in source_forget journal, got {subjects:?}"
    );
}

#[tokio::test]
async fn forget_record_deduplicates_source_forget_events_for_shared_hash_versions() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let (record_id, _target_id, shared_hash) = seed_record_versions_with_shared_source_hash(
        vault.path(),
        "shared source body",
        "first version body",
        "second version body",
    )
    .await;

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--record", &record_id, "--json"])
        .output()
        .expect("cairn forget");

    assert_eq!(
        out.status.code(),
        Some(0),
        "forget should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let conn =
        rusqlite::Connection::open(vault.path().join(".cairn").join("cairn.db")).expect("open db");
    let source_forgets = query_source_forgets(&conn).expect("query source forgets");
    assert_eq!(
        source_forgets.len(),
        1,
        "shared source hash should produce a single journal event"
    );
    assert_eq!(source_forgets[0].subject, shared_hash);
}

#[tokio::test]
async fn forget_record_reports_all_tombstoned_versions() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let (record_id, _) =
        seed_record_versions_for_source(vault.path(), "first version body", "second version body")
            .await;

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--record", &record_id, "--json"])
        .output()
        .expect("cairn forget");

    assert_eq!(
        out.status.code(),
        Some(0),
        "forget should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let response = json_stdout(&out);
    assert_eq!(response["data"]["deleted_count"], 2);
    let tombstones = response["data"]["tombstones"]
        .as_array()
        .expect("tombstones array");
    assert_eq!(tombstones.len(), 2);
}

#[test]
fn reconcile_pending_source_redactions_restores_uncommitted_files() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let source_dir = vault.path().join("sources").join("documents");
    fs::create_dir_all(&source_dir).expect("source dir");
    let source_path = source_dir.join("note.txt");
    let source_body = "original source body";
    fs::write(&source_path, source_body).expect("write source");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (record_id, target_id) = rt.block_on(seed_record_for_source(vault.path(), source_body));
    assert!(!record_id.is_empty());

    let op_dir = vault
        .path()
        .join(".cairn")
        .join("source-redactions")
        .join("op-restore");
    let backup_dir = op_dir.join("backups");
    fs::create_dir_all(&backup_dir).expect("backup dir");
    fs::write(backup_dir.join("0.bak"), source_body).expect("write backup");
    fs::write(&source_path, "redacted_at: now\n").expect("write redacted stub");
    let target_hash = format!("sha256:{:x}", Sha256::digest(target_id.as_bytes()));
    fs::write(
        op_dir.join("manifest.json"),
        format!(
            "{{\"op_id\":\"op-restore\",\"target_hash\":\"{target_hash}\",\"expected_event_count\":1,\"files\":[{{\"source_rel\":\"sources/documents/note.txt\",\"backup_rel\":\"backups/0.bak\"}}]}}"
        ),
    )
    .expect("write manifest");

    cairn_cli::verbs::forget::reconcile_pending_source_redactions(vault.path())
        .expect("reconcile redactions");

    let restored = fs::read_to_string(&source_path).expect("read restored source");
    assert_eq!(restored, source_body);
    assert!(
        !op_dir.exists(),
        "recovery directory must be removed after restore"
    );
}

#[tokio::test]
async fn status_reconciles_committed_pending_source_redactions() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    fs::write(
        vault.path().join(".cairn").join("config.yaml"),
        "vault:\n  name: test-vault\nsource:\n  redact_on_forget: true\n",
    )
    .expect("write config");

    let source_dir = vault.path().join("sources").join("documents");
    fs::create_dir_all(&source_dir).expect("source dir");
    let source_path = source_dir.join("note.txt");
    let source_body = "strict source body";
    fs::write(&source_path, source_body).expect("write source");

    let (record_id, target_id) = seed_record_for_source(vault.path(), source_body).await;

    let forget_out = cli()
        .current_dir(vault.path())
        .args(["forget", "--record", &record_id, "--json"])
        .output()
        .expect("cairn forget");
    assert_eq!(forget_out.status.code(), Some(0));
    let forget_response = json_stdout(&forget_out);
    let op_id = forget_response["operation_id"]
        .as_str()
        .expect("forget operation id");

    let op_dir = vault
        .path()
        .join(".cairn")
        .join("source-redactions")
        .join(op_id);
    let backup_dir = op_dir.join("backups");
    fs::create_dir_all(&backup_dir).expect("backup dir");
    fs::write(backup_dir.join("0.bak"), source_body).expect("write backup");
    let target_hash = format!("sha256:{:x}", Sha256::digest(target_id.as_bytes()));
    fs::write(
        op_dir.join("manifest.json"),
        format!(
            "{{\"op_id\":\"{op_id}\",\"target_hash\":\"{target_hash}\",\"expected_event_count\":1,\"files\":[{{\"source_rel\":\"sources/documents/note.txt\",\"backup_rel\":\"backups/0.bak\"}}]}}"
        ),
    )
    .expect("write manifest");

    let status_out = cli()
        .current_dir(vault.path())
        .args(["status", "--json"])
        .output()
        .expect("cairn status");
    assert_eq!(
        status_out.status.code(),
        Some(0),
        "status should succeed; stderr: {}",
        String::from_utf8_lossy(&status_out.stderr)
    );
    assert!(
        !op_dir.exists(),
        "status must clear committed redaction recovery state"
    );
    let source_after = fs::read_to_string(&source_path).expect("read source");
    assert!(
        source_after.contains("redacted_at"),
        "committed source redaction must stay applied"
    );
}
