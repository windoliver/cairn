//! End-to-end provenance source-link hygiene checks for `cairn lint`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{fs, path::Path, process::Command};

use cairn_core::config::CairnConfig;
use cairn_core::generated::verbs::lint::Kind;
use cairn_store_sqlite::SqliteIdentityRegistry;

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

fn json_stdout(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout was not valid JSON: {e}\nstdout: {stdout:?}");
    })
}

fn ingest_body(vault: &Path, body: &str) -> String {
    let out = cli()
        .current_dir(vault)
        .args([
            "ingest",
            "--kind",
            "reasoning",
            "--session",
            "01HQZX9F5N0000000000000000",
            "--body",
            body,
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

    let response = json_stdout(&out);
    let record_id = response["data"]["record_id"]
        .as_str()
        .expect("record_id string");
    let conn = rusqlite::Connection::open(vault.join(".cairn/cairn.db")).expect("open db");
    let record_json: String = conn
        .query_row(
            "SELECT record_json FROM records WHERE record_id = ?1 AND active = 1",
            [record_id],
            |row| row.get(0),
        )
        .expect("record json");
    let record: serde_json::Value = serde_json::from_str(&record_json).expect("parse record json");
    record["provenance"]["source_ids"][0]
        .as_str()
        .expect("source ref string")
        .to_owned()
}

#[tokio::test]
async fn lint_flags_missing_source_artifact_after_ingest() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let source_ref = ingest_body(vault.path(), "Source artifact should exist before lint.");
    fs::remove_file(vault.path().join(&source_ref)).expect("remove source artifact");

    let store = cairn_store_sqlite::open(&vault.path().join(".cairn/cairn.db"))
        .await
        .expect("open store");
    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();
    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint");

    let findings: Vec<_> = result
        .data
        .findings
        .iter()
        .filter(|finding| {
            finding.kind == Kind::MissingProvenance
                && finding.message.contains("does not resolve")
                && finding.message.contains(&source_ref)
        })
        .collect();
    assert_eq!(findings.len(), 1, "expected one dangling-source finding");
}

#[tokio::test]
async fn lint_flags_source_hash_mismatch_after_ingest() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let source_ref = ingest_body(vault.path(), "Immutable source bytes matter.");
    fs::write(
        vault.path().join(&source_ref),
        "tampered source bytes after ingest",
    )
    .expect("rewrite source artifact");

    let store = cairn_store_sqlite::open(&vault.path().join(".cairn/cairn.db"))
        .await
        .expect("open store");
    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();
    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint");

    let findings: Vec<_> = result
        .data
        .findings
        .iter()
        .filter(|finding| {
            finding.kind == Kind::MissingProvenance
                && finding.message.contains("hash mismatch")
                && finding.message.contains(&source_ref)
        })
        .collect();
    assert_eq!(findings.len(), 1, "expected one hash-mismatch finding");
}
