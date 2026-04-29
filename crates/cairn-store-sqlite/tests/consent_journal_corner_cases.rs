// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Consent-journal corner cases introduced in rounds 5/9 of the
//! review-fix loop:
//!
//! - canonical-form retry: payload JSON with reordered keys must be
//!   idempotent (round 5)
//! - divergent actor under same `op_id` must Conflict (round 5)
//! - store stamps `at` itself; caller-supplied `at` is ignored, so a
//!   retry's `at` cannot rewrite the persisted timestamp (round 9)

use cairn_core::contract::memory_store::{
    apply::MemoryStoreApply,
    error::StoreError,
    types::{ConsentJournalEntry, OpId, TargetId},
};
use cairn_core::domain::{Rfc3339Timestamp, actor_ref::ActorRef};
use cairn_core::wal::test_apply_token;
use cairn_store_sqlite::SqliteMemoryStore;
use rusqlite::Connection;
use tempfile::tempdir;

#[tokio::test]
async fn reordered_payload_keys_are_idempotent() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let store = SqliteMemoryStore::open(&db_path).await.expect("open");

    // First write: keys in order {a, b, c}.
    let first = ConsentJournalEntry {
        op_id: OpId::new("op-cj-keyorder"),
        kind: "activate".to_owned(),
        target_id: Some(TargetId::new("cj-keyorder-target")),
        actor: ActorRef::from_string("usr:order"),
        payload: serde_json::json!({"a": 1, "b": [2, 3], "c": {"x": "y"}}),
        at: Rfc3339Timestamp::now(),
    };
    let id_first = store
        .with_apply_tx(test_apply_token(), {
            let e = first.clone();
            move |tx| tx.append_consent_journal(&e)
        })
        .await
        .expect("first append");

    // Retry: same logical content but the JSON object would serialize
    // with a different key order in some runtimes (we force this by
    // re-parsing through a string with explicit ordering).
    let reordered_str = r#"{"c":{"x":"y"},"b":[2,3],"a":1}"#;
    let reordered_payload: serde_json::Value =
        serde_json::from_str(reordered_str).expect("valid json");
    let mut reordered = first.clone();
    reordered.payload = reordered_payload;
    // Also bump the caller-supplied `at` to demonstrate the
    // store-stamped behavior survives a retry with a fresh clock.
    reordered.at = Rfc3339Timestamp::parse("2030-01-01T00:00:00Z").expect("valid");

    let id_second = store
        .with_apply_tx(test_apply_token(), {
            let e = reordered;
            move |tx| tx.append_consent_journal(&e)
        })
        .await
        .expect("reordered retry must be idempotent");

    assert_eq!(
        id_first, id_second,
        "canonicalized payload comparison must treat reordered keys as identical"
    );

    // Exactly one row.
    let conn = Connection::open(&db_path).expect("raw conn");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE op_id = ?1",
            rusqlite::params!["op-cj-keyorder"],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 1, "reordered retry must not duplicate rows");
}

#[tokio::test]
async fn divergent_actor_rejects_under_same_op_id() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let entry = ConsentJournalEntry {
        op_id: OpId::new("op-cj-actor-divergent"),
        kind: "activate".to_owned(),
        target_id: Some(TargetId::new("cj-actor-target")),
        actor: ActorRef::from_string("usr:original"),
        payload: serde_json::json!({"k": "v"}),
        at: Rfc3339Timestamp::now(),
    };
    store
        .with_apply_tx(test_apply_token(), {
            let e = entry.clone();
            move |tx| tx.append_consent_journal(&e)
        })
        .await
        .expect("first append");

    let mut tampered = entry.clone();
    tampered.actor = ActorRef::from_string("usr:tampered");
    let err = store
        .with_apply_tx(test_apply_token(), {
            let e = tampered;
            move |tx| tx.append_consent_journal(&e)
        })
        .await
        .expect_err("divergent actor must fail");
    assert!(
        matches!(err, StoreError::Conflict { .. }),
        "divergent actor must Conflict, got: {err:?}"
    );
}

#[tokio::test]
async fn store_owns_at_and_ignores_caller_supplied_value() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let store = SqliteMemoryStore::open(&db_path).await.expect("open");

    // Caller supplies an obviously-wrong far-future `at`. Store must
    // ignore it and stamp the real time.
    let bogus_at = Rfc3339Timestamp::parse("2099-12-31T23:59:59Z").expect("valid");
    let entry = ConsentJournalEntry {
        op_id: OpId::new("op-cj-stamp"),
        kind: "activate".to_owned(),
        target_id: Some(TargetId::new("cj-stamp-target")),
        actor: ActorRef::from_string("usr:stamp"),
        payload: serde_json::json!({"k": "v"}),
        at: bogus_at.clone(),
    };
    store
        .with_apply_tx(test_apply_token(), {
            let e = entry;
            move |tx| tx.append_consent_journal(&e)
        })
        .await
        .expect("append");

    // Read the persisted `at` and confirm it is NOT the caller's bogus
    // 2099 value. Anything in the current decade is fine — what
    // matters is that the caller's value did not survive.
    let conn = Connection::open(&db_path).expect("raw conn");
    let stored_at: String = conn
        .query_row(
            "SELECT at FROM consent_journal WHERE op_id = ?1",
            rusqlite::params!["op-cj-stamp"],
            |r| r.get(0),
        )
        .expect("read at");
    assert_ne!(
        stored_at,
        bogus_at.as_str(),
        "store must ignore caller-supplied `at`; instead persisted: {stored_at}"
    );
    assert!(
        stored_at.starts_with("202") || stored_at.starts_with("203"),
        "stored at must look like a real recent timestamp; got: {stored_at}"
    );
}
