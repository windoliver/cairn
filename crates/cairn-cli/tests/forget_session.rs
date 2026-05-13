//! End-to-end tests for `cairn forget --session`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{path::Path, process::Command, time::Duration};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn ingest_body(vault: &Path, body: &str, session_id: &str) -> String {
    let out = cli()
        .current_dir(vault)
        .args([
            "ingest",
            "--kind",
            "reasoning",
            "--body",
            body,
            "--session",
            session_id,
            "--json",
        ])
        .output()
        .expect("cairn ingest");
    assert_eq!(
        out.status.code(),
        Some(0),
        "ingest should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let response: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json envelope");
    response["data"]["record_id"]
        .as_str()
        .expect("record_id")
        .to_owned()
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

fn ordered_session_targets(db_path: &Path, session_id: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(db_path).expect("open db");
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT target_id FROM records \
              WHERE json_extract(scope, '$.session_id') = ?1 \
              ORDER BY target_id",
        )
        .expect("prepare session target query");
    stmt.query_map([session_id], |row| row.get::<_, String>(0))
        .expect("query session targets")
        .map(|row| row.expect("target row"))
        .collect()
}

fn session_row_count(db_path: &Path, session_id: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path).expect("open db");
    conn.query_row(
        "SELECT COUNT(*) FROM records \
          WHERE json_extract(scope, '$.session_id') = ?1",
        [session_id],
        |row| row.get(0),
    )
    .expect("session count")
}

#[test]
fn forget_session_purges_all_records_in_the_session() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let first = ingest_body(vault.path(), "turn one", "sess-42");
    let second = ingest_body(vault.path(), "turn two", "sess-42");

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--session", "sess-42", "--json"])
        .output()
        .expect("forget session");

    assert_eq!(
        out.status.code(),
        Some(0),
        "forget should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let response: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json envelope");
    assert_eq!(response["status"], "committed");
    assert_eq!(response["verb"], "forget");
    assert_eq!(response["data"]["deleted_count"], 2);

    let db_path = vault.path().join(".cairn/cairn.db");
    assert_eq!(target_row_count(&db_path, &first), 0);
    assert_eq!(target_row_count(&db_path, &second), 0);
}

#[test]
fn forget_session_keeps_live_targets_when_phase_b_fails_mid_session() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let first = ingest_body(vault.path(), "turn one", "sess-42");
    let second = ingest_body(vault.path(), "turn two", "sess-42");
    let ordered = ordered_session_targets(&vault.path().join(".cairn/cairn.db"), "sess-42");
    assert_eq!(ordered.len(), 2, "expected two session targets");
    let doomed_second = ordered[1].clone();

    let registry_dir = vault.path().join(".cairn/backups");
    std::fs::create_dir_all(&registry_dir).expect("create registry dir");
    std::fs::write(
        registry_dir.join("broken.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "backup_id": "bkp_session_retry_safe",
            "created_at": "2026-05-12T12:00:00Z",
            "artifact_path": vault.path().join("missing-backup").display().to_string(),
            "target_ids_included": [doomed_second],
        }))
        .expect("registry json"),
    )
    .expect("write broken registry entry");

    let forget = cli()
        .current_dir(vault.path())
        .args(["forget", "--session", "sess-42", "--json"])
        .output()
        .expect("forget session");
    assert_ne!(
        forget.status.code(),
        Some(0),
        "forget should fail when session Phase B fails"
    );

    let db_path = vault.path().join(".cairn/cairn.db");
    assert_eq!(
        target_row_count(&db_path, &first),
        1,
        "first target should remain present so the operator can retry safely"
    );
    assert_eq!(
        target_row_count(&db_path, &second),
        1,
        "second target should remain present so the operator can retry safely"
    );
}

#[test]
fn ingest_session_rejects_while_exclusive_session_lock_is_held() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let store = rt
        .block_on(cairn_store_sqlite::open(
            vault.path().join(".cairn/cairn.db"),
        ))
        .expect("open store");
    let conn = std::sync::Arc::clone(store.raw_conn_for_admin().expect("raw conn"));
    let inc = store.incarnation().cloned().expect("incarnation");
    let resource = cairn_store_sqlite::locks::ResourceKey::session("default", "default", "sess-42");
    let lock = rt
        .block_on(cairn_store_sqlite::locks::acquire(
            &conn,
            &resource,
            cairn_store_sqlite::locks::LockMode::Exclusive,
            "test-holder",
            Duration::from_secs(30),
            &inc,
            "forget --session test lock",
        ))
        .expect("acquire lock");

    let ingest = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "reasoning",
            "--body",
            "blocked",
            "--session",
            "sess-42",
            "--json",
        ])
        .output()
        .expect("cairn ingest");

    rt.block_on(lock.release()).expect("release lock");

    assert_ne!(
        ingest.status.code(),
        Some(0),
        "ingest should fail while the exclusive session lock is held; stderr: {}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    assert_eq!(
        session_row_count(&vault.path().join(".cairn/cairn.db"), "sess-42"),
        0,
        "blocked ingest must not persist any live rows for the session"
    );
}
