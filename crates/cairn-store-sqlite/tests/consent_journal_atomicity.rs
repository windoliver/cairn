// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Consent journal atomicity (brief §5.6 line 2029).
//!
//! The state change (stage/activate) and the consent journal row MUST
//! commit atomically: an `Ok(())` closure persists both rows; a closure
//! returning `Err` (or panicking) commits neither. The Err and panic
//! directions are covered in `tx_rollback.rs` and `tx_panic_safety.rs`;
//! this file pins the **Ok-commits-both** direction with multiple
//! journal entries to exercise the join key (`op_id`).

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{
    MemoryStore,
    apply::MemoryStoreApply,
    types::{ConsentJournalEntry, OpId, TargetId},
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
    let user_id = Identity::parse("usr:cjtest").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:cjtest".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-23T16:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "c".repeat(64)),
            consent_ref: "consent:cj1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-23T16:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-23T16:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "d".repeat(128))).expect("valid"),
        tags: vec!["cj".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

#[tokio::test]
async fn ok_commits_record_and_all_consent_entries() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let store = SqliteMemoryStore::open(&db_path).await.expect("open");

    let target = TargetId::new("cj-target-ok");
    let actor = ActorRef::from_string("usr:cjtest");
    let op_id = OpId::new("op-cj-ok-1");

    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let a = actor.clone();
            let op = op_id.clone();
            move |tx| {
                let rec = make_record("01HQZX9F5N0000000000000080", "committed body");
                tx.stage_version(&t, &rec)?;
                tx.activate_version(&t, 1, None)?;
                // Two consent rows under the same op_id (stage + activate).
                tx.append_consent_journal(&ConsentJournalEntry {
                    op_id: op.clone(),
                    kind: "stage".to_owned(),
                    target_id: Some(t.clone()),
                    actor: a.clone(),
                    payload: serde_json::json!({"phase": "stage"}),
                    at: Rfc3339Timestamp::now(),
                })?;
                tx.append_consent_journal(&ConsentJournalEntry {
                    op_id: op,
                    kind: "activate".to_owned(),
                    target_id: Some(t),
                    actor: a,
                    payload: serde_json::json!({"phase": "activate"}),
                    at: Rfc3339Timestamp::now(),
                })?;
                Ok(())
            }
        })
        .await
        .expect("commit-both tx must succeed");

    // Record is visible.
    let principal = Principal::system(&test_apply_token());
    let got = store
        .get(&principal, &target)
        .await
        .expect("get")
        .expect("record present after Ok commit");
    assert_eq!(got.body, "committed body");

    // Both consent rows persisted under the shared op_id.
    let conn = Connection::open(&db_path).expect("raw conn");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE op_id = ?1",
            rusqlite::params![op_id.as_str()],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(
        count, 2,
        "both consent_journal rows must commit alongside the record"
    );
}

/// Retrying the same `(op_id, kind, target_id)` MUST be a no-op rather
/// than accumulating duplicate audit rows. Returns the same row id.
#[tokio::test]
async fn append_consent_journal_idempotent_under_retry() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let store = SqliteMemoryStore::open(&db_path).await.expect("open");

    let target = TargetId::new("cj-idem-target");
    let actor = ActorRef::from_string("usr:cjtest");
    let op_id = OpId::new("op-cj-retry");

    let entry = ConsentJournalEntry {
        op_id: op_id.clone(),
        kind: "activate".to_owned(),
        target_id: Some(target.clone()),
        actor: actor.clone(),
        payload: serde_json::json!({"phase": "first"}),
        at: Rfc3339Timestamp::now(),
    };

    let id_first = store
        .with_apply_tx(test_apply_token(), {
            let e = entry.clone();
            move |tx| tx.append_consent_journal(&e)
        })
        .await
        .expect("first append");

    // Retry with the same idempotency tuple — payload changed to prove
    // the second insert was a no-op.
    let mut retry = entry.clone();
    retry.payload = serde_json::json!({"phase": "retry-should-be-ignored"});
    let id_second = store
        .with_apply_tx(test_apply_token(), {
            let e = retry.clone();
            move |tx| tx.append_consent_journal(&e)
        })
        .await
        .expect("retry append");

    assert_eq!(
        id_first, id_second,
        "retry must surface the same row id as the first call"
    );

    // Exactly one row in consent_journal under the shared op_id.
    let conn = Connection::open(&db_path).expect("raw conn");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE op_id = ?1",
            rusqlite::params![op_id.as_str()],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 1, "retry must not duplicate consent_journal rows");
}
