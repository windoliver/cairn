//! End-to-end tests for `cairn forget --record`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{fs, path::Path, process::Command};

use sha2::{Digest, Sha256};

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

fn ingest_body(vault: &Path, body: &str) -> (String, String, String) {
    let out = cli()
        .current_dir(vault)
        .args(["ingest", "--kind", "reasoning", "--body", body, "--json"])
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
    let record_id = response["data"]["record_id"]
        .as_str()
        .expect("record_id")
        .to_owned();

    let conn = rusqlite::Connection::open(vault.join(".cairn/cairn.db")).expect("open db");
    let record_json: String = conn
        .query_row(
            "SELECT record_json FROM records WHERE record_id = ?1",
            [record_id.as_str()],
            |row| row.get(0),
        )
        .expect("record json");
    let record: serde_json::Value = serde_json::from_str(&record_json).expect("parse record");
    let source_id = record["provenance"]["source_ids"][0]
        .as_str()
        .expect("source id")
        .to_owned();
    let source_hash = record["provenance"]["source_hash"]
        .as_str()
        .expect("source hash")
        .to_owned();
    (record_id, source_id, source_hash)
}

fn target_hash(target_id: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(target_id.as_bytes()))
}

#[test]
fn forget_record_purges_rows_and_records_source_forget_receipts() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let (record_id, source_id, source_hash) = ingest_body(vault.path(), "forget this body");

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

    let conn = rusqlite::Connection::open(vault.path().join(".cairn/cairn.db")).expect("open db");
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1",
            [record_id.as_str()],
            |row| row.get(0),
        )
        .expect("remaining rows");
    assert_eq!(remaining, 0, "all target versions should be purged");

    let record_events =
        cairn_store_sqlite::consent::query_by_subject(&conn, &target_hash(&record_id))
            .expect("record events");
    assert!(record_events.iter().any(|event| {
        event.kind == cairn_core::domain::ConsentKind::ForgetIntent
            && matches!(
                &event.payload,
                cairn_core::domain::ConsentPayload::IntentReceipt { reason_code, .. }
                    if reason_code == "record_forget"
            )
    }));

    let source_events =
        cairn_store_sqlite::consent::query_by_subject(&conn, &source_hash).expect("source events");
    assert!(source_events.iter().any(|event| {
        event.kind == cairn_core::domain::ConsentKind::ForgetIntent
            && matches!(
                &event.payload,
                cairn_core::domain::ConsentPayload::IntentReceipt { reason_code, .. }
                    if reason_code == "source_forget"
            )
    }));

    assert!(
        vault.path().join(source_id).exists(),
        "default policy should preserve the source artifact path"
    );
}

#[test]
fn forget_record_redacts_source_and_blocks_reingest() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    fs::write(
        vault.path().join(".cairn/config.yaml"),
        "vault:\n  source:\n    redact_on_forget: true\n",
    )
    .expect("write config");

    let body = "do not resurrect this source";
    let (record_id, source_id, source_hash) = ingest_body(vault.path(), body);

    let forget = cli()
        .current_dir(vault.path())
        .args(["forget", "--record", &record_id, "--json"])
        .output()
        .expect("cairn forget");
    assert_eq!(
        forget.status.code(),
        Some(0),
        "forget should commit; stderr: {}",
        String::from_utf8_lossy(&forget.stderr)
    );

    let marker = fs::read_to_string(vault.path().join(&source_id)).expect("redaction marker");
    assert!(marker.starts_with("cairn:redacted-source:v1\n"));
    assert!(marker.contains(&format!("source_hash={source_hash}")));

    let reingest = cli()
        .current_dir(vault.path())
        .args(["ingest", "--kind", "reasoning", "--body", body, "--json"])
        .output()
        .expect("cairn ingest");
    assert_eq!(
        reingest.status.code(),
        Some(64),
        "re-ingest should be rejected; stderr: {}",
        String::from_utf8_lossy(&reingest.stderr)
    );
    let stdout = String::from_utf8(reingest.stdout).expect("utf-8 stdout");
    let response: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json envelope");
    assert_eq!(response["status"], "rejected");
    assert_eq!(response["error"]["code"], "InvalidArgs");
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("prior source-forget receipt"))
    );
}
