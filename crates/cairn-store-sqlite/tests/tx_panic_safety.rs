// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Panic-safety tests for `with_apply_tx`.
//!
//! Verifies that a panic inside the user closure triggers a `ROLLBACK` so the
//! connection remains usable for subsequent transactions and no partial writes
//! from the panicking closure are visible.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{
    MemoryStore,
    apply::MemoryStoreApply,
    types::{ListQuery, TargetId},
};
use cairn_core::domain::{
    ChainRole, EvidenceVector, MemoryClass, MemoryKind, MemoryVisibility, Provenance,
    Rfc3339Timestamp, ScopeTuple,
    actor_chain::ActorChainEntry,
    identity::Identity,
    principal::Principal,
    record::{Ed25519Signature, MemoryRecord, RecordId},
};
use cairn_core::wal::test_apply_token;
use cairn_store_sqlite::SqliteMemoryStore;
use tempfile::tempdir;

fn make_record(id_ulid: &str, body: &str) -> MemoryRecord {
    let user_id = Identity::parse("usr:tafeng").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:tafeng".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "a".repeat(64)),
            consent_ref: "consent:01HQZ".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-22T14:05:11Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "a".repeat(128))).expect("valid"),
        tags: vec!["pref".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

/// A panic inside `with_apply_tx` must:
/// 1. Surface as a `JoinError` (wrapped in `StoreError::Backend`).
/// 2. Roll back any partial writes made before the panic.
/// 3. Leave the connection in a usable state for the next `with_apply_tx`.
#[tokio::test]
async fn panic_in_closure_rolls_back_and_connection_survives() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("test-panic-target-1");
    let record = make_record("01HQZX9F5N0000000000000002", "will be rolled back");

    // Attempt 1: stage a record then panic — must roll back.
    let result: Result<(), _> = store
        .with_apply_tx(test_apply_token(), {
            let target = target.clone();
            let record = record.clone();
            move |tx| {
                // Stage the record (partial write).
                tx.stage_version(&target, &record)?;
                // Panic before committing.
                panic!("simulated panic inside with_apply_tx");
            }
        })
        .await;

    // The JoinError from spawn_blocking surfaces as StoreError::Backend.
    assert!(
        result.is_err(),
        "panicking closure must return Err (JoinError wrapped in StoreError::Backend)"
    );

    // Partial write must NOT be visible.
    let principal = Principal::system(&test_apply_token());
    let list = store
        .list(&ListQuery::new(principal.clone()))
        .await
        .expect("list after panic must not error");
    assert_eq!(
        list.rows.len(),
        0,
        "no partial writes from panicking closure should be visible"
    );

    // Attempt 2: the connection must still be usable for a normal transaction.
    store
        .with_apply_tx(test_apply_token(), {
            let target = target.clone();
            let record = record.clone();
            move |tx| {
                tx.stage_version(&target, &record)?;
                tx.activate_version(&target, 1, None)?;
                Ok(())
            }
        })
        .await
        .expect("second with_apply_tx must succeed after panic rollback");

    let got = store
        .get(&principal, &target)
        .await
        .expect("get after recovery")
        .expect("record must be present after successful second tx");
    assert_eq!(
        got.body, record.body,
        "record written in second tx is correct"
    );
}
