//! Integration tests for vault bootstrap (brief §3.1, issue #41).

// 19 = VAULT_DIRS.len() in vault.rs (brief §3.1 directory tree)
// 4  = placeholder files: .cairn/config.yaml, purpose.md, index.md, log.md

use std::path::Path;

use cairn_cli::vault::{BootstrapOpts, bootstrap};
use cairn_core::domain::identity::keys::VaultId;

fn opts(dir: &Path) -> BootstrapOpts {
    BootstrapOpts {
        vault_path: dir.to_path_buf(),
        force: false,
    }
}

fn forced(dir: &Path) -> BootstrapOpts {
    BootstrapOpts {
        vault_path: dir.to_path_buf(),
        force: true,
    }
}

#[test]
fn bootstrap_creates_full_tree() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&opts(dir.path())).unwrap();

    let expected_dirs = [
        "sources",
        "sources/articles",
        "sources/papers",
        "sources/transcripts",
        "sources/documents",
        "sources/chat",
        "sources/assets",
        "raw",
        "wiki",
        "wiki/entities",
        "wiki/concepts",
        "wiki/summaries",
        "wiki/synthesis",
        "wiki/prompts",
        "skills",
        ".cairn",
        ".cairn/evolution",
        ".cairn/cache",
        ".cairn/models",
    ];
    for rel in &expected_dirs {
        assert!(dir.path().join(rel).is_dir(), "expected dir missing: {rel}");
    }
}

#[test]
fn bootstrap_receipt_counts_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = bootstrap(&opts(dir.path())).unwrap();
    assert_eq!(
        receipt.dirs_created.len(),
        19,
        "first run: all 19 dirs should be created"
    );
    assert_eq!(receipt.dirs_existing.len(), 0);
}

#[test]
fn bootstrap_creates_placeholder_files() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&opts(dir.path())).unwrap();

    assert!(
        dir.path().join(".cairn/config.yaml").is_file(),
        "config.yaml missing"
    );
    assert!(
        dir.path().join("purpose.md").is_file(),
        "purpose.md missing"
    );
    assert!(dir.path().join("index.md").is_file(), "index.md missing");
    assert!(dir.path().join("log.md").is_file(), "log.md missing");
}

#[test]
fn bootstrap_receipt_counts_files() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = bootstrap(&opts(dir.path())).unwrap();
    assert_eq!(
        receipt.files_created.len(),
        4,
        "first run: all 4 placeholder files should be created"
    );
    assert_eq!(
        receipt.files_skipped.len(),
        0,
        "first run: no files should be skipped"
    );
}

#[test]
fn bootstrap_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&opts(dir.path())).unwrap();

    // second run
    let receipt = bootstrap(&opts(dir.path())).unwrap();

    // dirs: all existing, none created
    assert_eq!(
        receipt.dirs_created.len(),
        0,
        "second run: no new dirs should be created"
    );
    assert_eq!(
        receipt.dirs_existing.len(),
        19,
        "second run: all 19 dirs should already exist"
    );

    // files: all skipped, none created
    assert_eq!(
        receipt.files_created.len(),
        0,
        "second run: no files should be created"
    );
    assert_eq!(
        receipt.files_skipped.len(),
        4,
        "second run: all 4 files should be skipped"
    );

    // vault is still intact
    assert!(dir.path().join(".cairn/config.yaml").is_file());
    assert!(dir.path().join("purpose.md").is_file());
}

#[test]
fn bootstrap_skips_user_edited_purpose() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&opts(dir.path())).unwrap();

    // user edits purpose.md
    let purpose = dir.path().join("purpose.md");
    std::fs::write(&purpose, "# My vault\n\nPersonal knowledge base.\n").unwrap();

    // second run without --force
    let receipt = bootstrap(&opts(dir.path())).unwrap();

    // user's content must survive
    let content = std::fs::read_to_string(&purpose).unwrap();
    assert_eq!(content, "# My vault\n\nPersonal knowledge base.\n");

    // receipt must report all 4 files skipped
    assert_eq!(
        receipt.files_skipped.len(),
        4,
        "second run: all 4 files should be skipped"
    );
}

#[test]
fn bootstrap_force_overwrites_files() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&opts(dir.path())).unwrap();

    // user edits purpose.md
    let purpose = dir.path().join("purpose.md");
    std::fs::write(&purpose, "# My vault\n\nPersonal knowledge base.\n").unwrap();

    // --force run
    let receipt = bootstrap(&forced(dir.path())).unwrap();

    // purpose.md is overwritten with the template
    let content = std::fs::read_to_string(&purpose).unwrap();
    // must match PURPOSE_MD in vault.rs
    assert_eq!(
        content,
        "# Purpose\n\n<!-- Why does this vault exist? -->\n"
    );

    // receipt shows all 4 files created
    assert_eq!(
        receipt.files_created.len(),
        4,
        "force: all 4 files should be overwritten"
    );
    assert_eq!(
        receipt.files_skipped.len(),
        0,
        "force: no files should be skipped"
    );
}

#[test]
fn bootstrap_reports_db_path() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = bootstrap(&opts(dir.path())).unwrap();
    assert_eq!(receipt.db_path, dir.path().join(".cairn/cairn.db"));
    // db is NOT created by bootstrap — only its path is reported
    assert!(!dir.path().join(".cairn/cairn.db").exists());
}

#[test]
fn bootstrap_receipt_serializes_to_json() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = bootstrap(&opts(dir.path())).unwrap();
    let json = serde_json::to_string(&receipt).expect("receipt must serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.get("vault_path").is_some());
    assert!(parsed.get("config_path").is_some());
    assert!(parsed.get("db_path").is_some());
    assert!(parsed.get("dirs_created").is_some());
    assert!(parsed.get("dirs_existing").is_some());
    assert!(parsed.get("files_created").is_some());
    assert!(parsed.get("files_skipped").is_some());
}

#[test]
fn bootstrap_human_output_first_run() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = cairn_cli::vault::bootstrap(&opts(dir.path())).unwrap();
    let output = cairn_cli::vault::render_human(&receipt);
    // normalize absolute path and vault_id (ULID) so the snapshot is stable
    // across machines and test runs.
    let normalized = output
        .replace(dir.path().to_str().unwrap(), "<vault>")
        .replace(receipt.vault_id.as_str(), "<vault-id>");
    insta::assert_snapshot!(normalized);
}

#[test]
fn bootstrap_human_output_second_run() {
    let dir = tempfile::tempdir().unwrap();
    let first = cairn_cli::vault::bootstrap(&opts(dir.path())).unwrap();
    let receipt = cairn_cli::vault::bootstrap(&opts(dir.path())).unwrap();
    let output = cairn_cli::vault::render_human(&receipt);
    // normalize absolute path and vault_id (ULID) so the snapshot is stable
    // across machines and test runs.
    let normalized = output
        .replace(dir.path().to_str().unwrap(), "<vault>")
        .replace(first.vault_id.as_str(), "<vault-id>");
    insta::assert_snapshot!(normalized);
}

// ── vault.id lifecycle tests (bootstrap amendment 2026-04-27) ─────────────

/// First run must create `.cairn/vault.id` containing a valid ULID.
#[test]
fn bootstrap_mints_vault_id_first_run() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = bootstrap(&opts(dir.path())).unwrap();

    let vault_id_path = dir.path().join(".cairn/vault.id");
    assert!(
        vault_id_path.is_file(),
        ".cairn/vault.id must exist after first bootstrap"
    );

    let raw = std::fs::read_to_string(&vault_id_path).unwrap();
    let parsed = VaultId::parse(raw.trim()).expect("vault.id must contain a valid ULID");
    assert_eq!(
        parsed.as_str(),
        receipt.vault_id.as_str(),
        "receipt.vault_id must match file"
    );
}

/// Second run must leave `.cairn/vault.id` unchanged (idempotent).
#[test]
fn bootstrap_preserves_vault_id_on_second_run() {
    let dir = tempfile::tempdir().unwrap();
    let first = bootstrap(&opts(dir.path())).unwrap();
    let first_id = first.vault_id.as_str().to_owned();

    let second = bootstrap(&opts(dir.path())).unwrap();

    assert_eq!(
        first_id,
        second.vault_id.as_str(),
        "second bootstrap must not change vault.id"
    );
    let file_content = std::fs::read_to_string(dir.path().join(".cairn/vault.id")).unwrap();
    assert_eq!(
        first_id,
        file_content.trim(),
        "vault.id file must not change on second run"
    );
}

/// If `.cairn/vault.id` is deleted but the DB contains a `vault_meta` row,
/// bootstrap must fail closed (the vault has been bound).
///
/// This test creates a DB row by hand to simulate a vault that has completed
/// first-bind without relying on the full identity-provisioning stack.
#[test]
fn bootstrap_fails_closed_when_vault_id_lost_with_db_row() {
    use rusqlite::Connection;

    let dir = tempfile::tempdir().unwrap();

    // Bootstrap once to create the directory tree and vault.id.
    bootstrap(&opts(dir.path())).unwrap();

    // Simulate a completed first-bind by creating a minimal vault_meta table.
    let db_path = dir.path().join(".cairn/cairn.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vault_meta (vault_id TEXT NOT NULL, witness_sha256 BLOB NOT NULL);
             INSERT INTO vault_meta (vault_id, witness_sha256) VALUES ('01TESTULID000000000000000A', zeroblob(32));",
        )
        .unwrap();
    }

    // Delete vault.id — simulates accidental deletion after binding.
    std::fs::remove_file(dir.path().join(".cairn/vault.id")).unwrap();

    let err = bootstrap(&opts(dir.path())).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("vault.id lost"),
        "error must mention vault.id lost; got: {msg}"
    );
}

/// If `.cairn/vault.id` is deleted but a `.cairn/vault.binding` sentinel exists,
/// bootstrap must fail closed.
#[test]
fn bootstrap_fails_closed_when_vault_id_lost_with_sentinel_only() {
    let dir = tempfile::tempdir().unwrap();

    // Bootstrap once to create the directory tree.
    bootstrap(&opts(dir.path())).unwrap();

    // Place a binding sentinel (no DB needed).
    std::fs::write(dir.path().join(".cairn/vault.binding"), b"").unwrap();

    // Delete vault.id.
    std::fs::remove_file(dir.path().join(".cairn/vault.id")).unwrap();

    let err = bootstrap(&opts(dir.path())).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("vault.id lost"),
        "error must mention vault.id lost; got: {msg}"
    );
}

/// `--force` must not rewrite `.cairn/vault.id`.
#[test]
fn bootstrap_force_does_not_rewrite_vault_id() {
    let dir = tempfile::tempdir().unwrap();

    let first = bootstrap(&opts(dir.path())).unwrap();
    let first_id = first.vault_id.as_str().to_owned();

    let forced = bootstrap(&forced(dir.path())).unwrap();

    assert_eq!(
        first_id,
        forced.vault_id.as_str(),
        "--force must not change vault.id"
    );
    let file_content = std::fs::read_to_string(dir.path().join(".cairn/vault.id")).unwrap();
    assert_eq!(
        first_id,
        file_content.trim(),
        "vault.id file must survive --force"
    );
}
