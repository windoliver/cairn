// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Closure-returning-`Err` rollback for `with_apply_tx`.
//!
//! Spec §4.3 (tx execution model) requires that ANY closure that returns
//! `Err(StoreError::*)` causes a `ROLLBACK`, surfacing zero rows from the
//! aborted transaction — including consent-journal rows, which the same
//! transaction must persist atomically with the state change.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{
    MemoryStore,
    apply::MemoryStoreApply,
    error::StoreError,
    types::{ConsentJournalEntry, ListQuery, OpId, TargetId},
};
use cairn_core::domain::{
    ChainRole, EvidenceVector, MemoryClass, MemoryKind, MemoryVisibility, Provenance,
    Rfc3339Timestamp, ScopeTuple,
    actor_chain::ActorChainEntry,
    actor_ref::ActorRef,
    identity::Identity,
    principal::Principal,
    record::{Ed25519Signature, MemoryRecord, RecordId},
};
use cairn_core::wal::test_apply_token;
use cairn_store_sqlite::SqliteMemoryStore;
use rusqlite::Connection;
use tempfile::tempdir;

fn make_record(id_ulid: &str, body: &str) -> MemoryRecord {
    let user_id = Identity::parse("usr:rollback").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:rollback".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-23T15:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "9".repeat(64)),
            consent_ref: "consent:rb1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-23T15:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.6,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-23T15:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "f".repeat(128))).expect("valid"),
        tags: vec!["rb".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

/// Closure returning `Err` rolls back BOTH the staged record and the
/// consent-journal row — they must commit atomically or not at all
/// (brief §5.6 line 2029).
#[tokio::test]
async fn err_in_closure_rolls_back_record_and_consent_journal_atomically() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let store = SqliteMemoryStore::open(&db_path).await.expect("open");

    let target = TargetId::new("rollback-target-err");
    let actor = ActorRef::from_string("usr:rollback");
    let op_id = OpId::new("op-rollback-err-1");

    let result: Result<(), StoreError> = store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let a = actor.clone();
            let op = op_id.clone();
            move |tx| {
                let rec = make_record("01HQZX9F5N0000000000000070", "rolled back body");
                tx.stage_version(&t, &rec)?;
                tx.activate_version(&t, 1, None)?;
                tx.append_consent_journal(&ConsentJournalEntry {
                    op_id: op,
                    kind: "activate".to_owned(),
                    target_id: Some(t),
                    actor: a,
                    payload: serde_json::json!({"note": "should not survive"}),
                    at: Rfc3339Timestamp::now(),
                })?;
                Err(StoreError::Invariant("simulated rollback"))
            }
        })
        .await;
    assert!(matches!(result, Err(StoreError::Invariant(_))));

    // No record visible.
    let principal = Principal::system(&test_apply_token());
    let list = store
        .list(&ListQuery::new(principal.clone()))
        .await
        .expect("list after rollback");
    assert_eq!(list.rows.len(), 0, "Err must roll back staged record");

    // No consent_journal row.
    let conn = Connection::open(&db_path).expect("raw conn");
    let cj_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE op_id = ?1",
            rusqlite::params![op_id.as_str()],
            |r| r.get(0),
        )
        .expect("consent_journal count");
    assert_eq!(
        cj_count, 0,
        "Err must roll back consent_journal row alongside the record"
    );
}

/// After an `Err` rollback, the connection must still be usable: a
/// subsequent `with_apply_tx` succeeds and its writes are visible.
#[tokio::test]
async fn connection_survives_err_rollback() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("rollback-survive");

    let _ignored: Result<(), StoreError> = store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| {
                let rec = make_record("01HQZX9F5N0000000000000071", "ignored");
                tx.stage_version(&t, &rec)?;
                Err(StoreError::Invariant("abort"))
            }
        })
        .await;

    // Second tx: real write.
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| {
                let rec = make_record("01HQZX9F5N0000000000000072", "kept body");
                tx.stage_version(&t, &rec)?;
                tx.activate_version(&t, 1, None)?;
                Ok(())
            }
        })
        .await
        .expect("post-rollback tx must succeed");

    let principal = Principal::system(&test_apply_token());
    let got = store
        .get(&principal, &target)
        .await
        .expect("get")
        .expect("record present after successful tx");
    assert_eq!(got.body, "kept body");
}
