//! Integration tests for the `SQLite` consent-journal snapshot reader.

use cairn_core::contract::{ConsentJournalReader, MalformedSourceForgetReason};
use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryVisibility, Rfc3339Timestamp,
};
use cairn_store_sqlite::{SqliteConsentJournalReader, consent, open_sync};
use rusqlite::Connection;

fn forget_event(consent_id: &str, target_hash: &str) -> ConsentEvent {
    ConsentEvent {
        consent_id: consent_id.to_owned(),
        kind: ConsentKind::ForgetIntent,
        actor: Identity::parse("hmn:tafeng").expect("identity"),
        subject: target_hash.to_owned(),
        scope: "private".to_owned(),
        op_id: Some(format!("op-{consent_id}")),
        sensor_id: None,
        payload: ConsentPayload::IntentReceipt {
            target_id_hash: target_hash.to_owned(),
            scope_tier: MemoryVisibility::Private,
            reason_code: "user_command".to_owned(),
        },
        decided_at: Rfc3339Timestamp::parse("2026-04-28T12:00:00Z").expect("ts"),
        expires_at: None,
    }
}

#[test]
fn snapshot_reader_ignores_non_source_forget_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let conn = open_sync(&db_path).expect("open");
    consent::append(
        &conn,
        &forget_event("c-1", "hash:00000000000000000000000000000001"),
    )
    .expect("append");

    let reader = SqliteConsentJournalReader::open(&db_path).expect("snapshot");

    assert!(reader.forgotten_source_bytes_hashes().is_empty());
}

#[test]
fn snapshot_reader_parses_source_forget_rows_and_surfaces_malformed_versions() {
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(
        "CREATE TABLE consent_journal (
            rowid INTEGER PRIMARY KEY,
            kind TEXT NOT NULL,
            op_id TEXT,
            subject TEXT NOT NULL,
            payload_json TEXT
        );",
    )
    .expect("schema");
    conn.execute(
        "INSERT INTO consent_journal (kind, op_id, subject, payload_json)
         VALUES (?1, ?2, ?3, ?4)",
        (
            "source_forget",
            "forget-op-valid",
            "sources/chat/session-1.md",
            r#"{"source_id":"sources/chat/session-1.md","source_bytes_hash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","target":null}"#,
        ),
    )
    .expect("insert valid");
    conn.execute(
        "INSERT INTO consent_journal (kind, op_id, subject, payload_json)
         VALUES (?1, ?2, ?3, ?4)",
        (
            "source_forget",
            "forget-op-bad-version",
            "sources/chat/session-2.md",
            r#"{"source_id":"sources/chat/session-2.md","source_bytes_hash":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","target":{"hash":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","version":9}}"#,
        ),
    )
    .expect("insert malformed");

    let reader = SqliteConsentJournalReader::from_connection(&conn).expect("snapshot");

    assert!(
        reader
            .forgotten_source_bytes_hashes()
            .contains("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert!(
        reader
            .forgotten_source_forgets()
            .iter()
            .any(|row| row.op_id == "forget-op-valid")
    );
    assert!(
        reader
            .malformed_source_forget_rows()
            .iter()
            .any(|row| row.op_id == "forget-op-bad-version"
                && matches!(
                    row.reason,
                    MalformedSourceForgetReason::UnsupportedReplayHashVersion { version: 9 }
                ))
    );
}
