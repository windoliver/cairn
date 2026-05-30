//! CLI integration tests for the v0.2 `cairn.admin.v1` subcommands
//! introduced by issue #161: `grant`, `replay-wal`, `connector enable`,
//! `connector disable`.
//!
//! These tests drive the built `cairn` binary against a temp vault and
//! assert exit codes + stdout/stderr. The capability pre-check lives at
//! the SDK/MCP boundary (spec §7.4); CLI dispatches directly, so these
//! tests do NOT need to flip `ADMIN_EXTENSION_WIRED` first.

use assert_cmd::Command;

fn bootstrap_vault(vault: &std::path::Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

#[test]
fn admin_grant_succeeds_and_persists_operator_role() {
    use cairn_core::contract::admin_state::AdminStateStore;
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args(["admin", "grant", "hmn:alice"])
        .output()
        .expect("run admin grant");

    assert!(
        output.status.success(),
        "grant exited non-zero. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Persistence check: query the admin_roles table directly via
    // SqliteAdminStateStore. has_any_operator() must return true after grant.
    let db_path = vault.path().join(".cairn").join("cairn.db");
    let admin =
        cairn_store_sqlite::SqliteAdminStateStore::open(&db_path).expect("open admin store");
    assert!(
        admin.has_any_operator().expect("query operators"),
        "grant should persist an operator row"
    );
}

#[test]
fn admin_replay_wal_dry_run_succeeds_without_actor() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args(["admin", "replay-wal", "--kind", "upsert", "--from", "0"])
        .output()
        .expect("run replay-wal");

    assert!(
        output.status.success(),
        "dry-run replay should succeed without --actor. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("visited") && stdout.contains("applied"),
        "stdout should report visited/applied counts; got: {stdout}"
    );
}

#[test]
fn admin_replay_wal_apply_without_actor_errors() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "replay-wal",
            "--kind",
            "upsert",
            "--from",
            "0",
            "--apply",
        ])
        .output()
        .expect("run replay-wal --apply");

    assert!(
        !output.status.success(),
        "--apply without --actor must fail"
    );
}

#[test]
fn admin_replay_wal_apply_with_operator_escalates_not_yet_wired() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    // Grant operator first.
    Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args(["admin", "grant", "hmn:operator"])
        .output()
        .expect("grant");

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "replay-wal",
            "--kind",
            "upsert",
            "--from",
            "0",
            "--apply",
            "--actor",
            "hmn:operator",
        ])
        .output()
        .expect("run replay-wal --apply --actor");

    // Apply mode currently escalates with exit 75 (ReplayEscalated:
    // "apply-mode-not-yet-wired" — adapter ships in a follow-up issue).
    assert_eq!(
        output.status.code(),
        Some(75),
        "apply mode must escalate; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn admin_connector_enable_writes_row_with_operator_role() {
    use cairn_core::contract::admin_state::AdminStateStore;
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args(["admin", "grant", "hmn:operator"])
        .output()
        .expect("grant");

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "connector",
            "enable",
            "github",
            "--actor",
            "hmn:operator",
        ])
        .output()
        .expect("run connector enable");

    assert!(
        output.status.success(),
        "enable exited non-zero. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let db_path = vault.path().join(".cairn").join("cairn.db");
    let admin =
        cairn_store_sqlite::SqliteAdminStateStore::open(&db_path).expect("open admin store");
    let row = admin
        .get_connector_state("github")
        .expect("query state")
        .expect("row exists");
    assert!(row.enabled, "enable should persist enabled=true");
}

#[test]
fn admin_connector_disable_writes_row_with_reason() {
    use cairn_core::contract::admin_state::AdminStateStore;
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args(["admin", "grant", "hmn:operator"])
        .output()
        .expect("grant");

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "connector",
            "disable",
            "github",
            "--actor",
            "hmn:operator",
            "--reason",
            "test-disable",
        ])
        .output()
        .expect("run connector disable");

    assert!(
        output.status.success(),
        "disable exited non-zero. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let db_path = vault.path().join(".cairn").join("cairn.db");
    let admin =
        cairn_store_sqlite::SqliteAdminStateStore::open(&db_path).expect("open admin store");
    let row = admin
        .get_connector_state("github")
        .expect("query state")
        .expect("row exists");
    assert!(!row.enabled, "disable should persist enabled=false");
    assert_eq!(row.reason.as_deref(), Some("test-disable"));
}

#[test]
fn admin_connector_enable_without_operator_returns_64() {
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    // No grant — caller is not an operator.
    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "connector",
            "enable",
            "github",
            "--actor",
            "hmn:nobody",
        ])
        .output()
        .expect("run connector enable as non-operator");

    assert_eq!(
        output.status.code(),
        Some(64),
        "non-operator must get NotAuthorized exit code; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn admin_connector_backfill_fails_closed_without_spawner() {
    // Round-4 review #4: no real BackfillSpawner is wired, so the CLI backfill
    // must fail closed (non-zero exit) and must NOT print a misleading workflow
    // id, rather than reporting a success for a no-op.
    let vault = tempfile::tempdir().expect("vault tempdir");
    bootstrap_vault(vault.path());

    Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args(["admin", "grant", "hmn:operator"])
        .output()
        .expect("grant");

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault.path())
        .args([
            "admin",
            "connector",
            "backfill",
            "github",
            "--actor",
            "hmn:operator",
            "--from",
            "2026-01-01T00:00:00Z",
            "--to",
            "2026-01-02T00:00:00Z",
        ])
        .output()
        .expect("run connector backfill");

    assert!(
        !output.status.success(),
        "backfill must exit non-zero while no spawner is wired; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("workflow_id"),
        "backfill must not mint a misleading workflow_id; stdout: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unavailable") || stderr.contains("BackfillSpawner"),
        "backfill should explain it is unavailable; stderr: {stderr}"
    );
}
