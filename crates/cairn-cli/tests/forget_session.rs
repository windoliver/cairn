//! End-to-end tests for `cairn forget --session`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{path::Path, process::Command};

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
    ingest_body_scoped(vault, body, session_id, None, None)
}

fn ingest_body_scoped(
    vault: &Path,
    body: &str,
    session_id: &str,
    tenant: Option<&str>,
    workspace: Option<&str>,
) -> String {
    let mut cmd = cli();
    cmd.current_dir(vault).args([
        "ingest",
        "--kind",
        "reasoning",
        "--body",
        body,
        "--session",
        session_id,
    ]);
    if let Some(tenant) = tenant {
        cmd.args(["--scope-tenant", tenant]);
    }
    if let Some(workspace) = workspace {
        cmd.args(["--scope-workspace", workspace]);
    }
    let out = cmd.arg("--json").output().expect("cairn ingest");
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

fn rewrite_target_scope(
    db_path: &Path,
    target_id: &str,
    tenant: Option<&str>,
    workspace: Option<&str>,
) {
    let conn = rusqlite::Connection::open(db_path).expect("open db");
    conn.execute(
        "UPDATE records
            SET scope = json_set(
                json_set(scope, '$.tenant', json(?2)),
                '$.workspace',
                json(?3)
            )
          WHERE target_id = ?1",
        rusqlite::params![
            target_id,
            tenant
                .map_or_else(|| "null".to_owned(), |value| format!("\"{value}\"")),
            workspace
                .map_or_else(|| "null".to_owned(), |value| format!("\"{value}\""))
        ],
    )
    .expect("rewrite record scope");
}

#[test]
#[ignore = "depends on --session ingest path which conflicts with merged signed-ingest flow"]
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
#[ignore = "depends on --session ingest path which conflicts with merged signed-ingest flow"]
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
    let third = ingest_body(vault.path(), "turn three", "sess-42");
    assert_eq!(
        target_row_count(&db_path, &third),
        1,
        "failed forget must release session locks so follow-up ingest can proceed immediately"
    );
}

#[test]
#[ignore = "depends on --session ingest path which conflicts with merged signed-ingest flow"]
fn forget_session_rejects_ambiguous_cross_scope_session_ids() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let first = ingest_body_scoped(vault.path(), "tenant a turn", "sess-42", None, None);
    let second = ingest_body_scoped(vault.path(), "tenant b turn", "sess-42", None, None);
    let db_path = vault.path().join(".cairn/cairn.db");
    rewrite_target_scope(&db_path, &first, Some("tenant-a"), Some("workspace-a"));
    rewrite_target_scope(&db_path, &second, Some("tenant-b"), Some("workspace-b"));

    let forget = cli()
        .current_dir(vault.path())
        .args(["forget", "--session", "sess-42", "--json"])
        .output()
        .expect("forget session");

    assert_eq!(
        forget.status.code(),
        Some(64),
        "ambiguous session forget should fail closed as invalid args; stderr: {}",
        String::from_utf8_lossy(&forget.stderr)
    );

    let stdout = String::from_utf8(forget.stdout).expect("utf-8 stdout");
    let response: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json envelope");
    assert_eq!(response["status"], "rejected");
    assert_eq!(response["error"]["code"], "InvalidArgs");

    assert_eq!(target_row_count(&db_path, &first), 1);
    assert_eq!(target_row_count(&db_path, &second), 1);
    assert_eq!(session_row_count(&db_path, "sess-42"), 2);
}

#[test]
#[ignore = "depends on --session ingest path which conflicts with merged signed-ingest flow"]
fn forget_session_handles_real_all_scope_values_without_namespace_collision() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let first = ingest_body_scoped(vault.path(), "turn one", "sess-42", None, None);
    let second = ingest_body_scoped(vault.path(), "turn two", "sess-42", None, None);
    let db_path = vault.path().join(".cairn/cairn.db");
    rewrite_target_scope(&db_path, &first, Some("__all__"), Some("__all__"));
    rewrite_target_scope(&db_path, &second, Some("__all__"), Some("__all__"));

    let forget = cli()
        .current_dir(vault.path())
        .args(["forget", "--session", "sess-42", "--json"])
        .output()
        .expect("forget session");

    assert_eq!(
        forget.status.code(),
        Some(0),
        "forget should still commit when a real scope uses '__all__'; stderr: {}",
        String::from_utf8_lossy(&forget.stderr)
    );

    let stdout = String::from_utf8(forget.stdout).expect("utf-8 stdout");
    let response: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json envelope");
    assert_eq!(response["status"], "committed");
    assert_eq!(response["data"]["deleted_count"], 2);

    assert_eq!(target_row_count(&db_path, &first), 0);
    assert_eq!(target_row_count(&db_path, &second), 0);
}

#[test]
#[ignore = "depends on --session ingest path which conflicts with merged signed-ingest flow"]
fn forget_session_rejects_null_and_empty_scope_components_as_distinct_partitions() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let first = ingest_body_scoped(vault.path(), "null scope", "sess-42", None, None);
    let second = ingest_body_scoped(vault.path(), "empty scope", "sess-42", None, None);
    let db_path = vault.path().join(".cairn/cairn.db");
    rewrite_target_scope(&db_path, &first, Some("tenant-a"), None);
    rewrite_target_scope(&db_path, &second, Some("tenant-a"), Some(""));

    let forget = cli()
        .current_dir(vault.path())
        .args(["forget", "--session", "sess-42", "--json"])
        .output()
        .expect("forget session");

    assert_eq!(
        forget.status.code(),
        Some(64),
        "null and empty-string scope components should remain distinct; stderr: {}",
        String::from_utf8_lossy(&forget.stderr)
    );

    let stdout = String::from_utf8(forget.stdout).expect("utf-8 stdout");
    let response: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json envelope");
    assert_eq!(response["status"], "rejected");
    assert_eq!(response["error"]["code"], "InvalidArgs");

    assert_eq!(target_row_count(&db_path, &first), 1);
    assert_eq!(target_row_count(&db_path, &second), 1);
}
