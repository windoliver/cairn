// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! `activate_version` monotonicity: activating an older version when a newer
//! one is already active must return `StoreError::Conflict { kind: ActivationRaced }`.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{
    apply::MemoryStoreApply,
    error::{ConflictKind, StoreError},
    types::TargetId,
};
use cairn_core::domain::{
    ChainRole, EvidenceVector, MemoryClass, MemoryKind, MemoryVisibility, Provenance,
    Rfc3339Timestamp, ScopeTuple,
    actor_chain::ActorChainEntry,
    identity::Identity,
    record::{Ed25519Signature, MemoryRecord, RecordId},
};
use cairn_core::wal::test_apply_token;
use cairn_store_sqlite::SqliteMemoryStore;
use tempfile::tempdir;

fn make_record(id_suffix: u8, body: &str) -> MemoryRecord {
    let user_id = Identity::parse("usr:monocheck").expect("valid");
    let id = format!("01HQZX9F5N000000000000004{id_suffix:X}");
    MemoryRecord {
        id: RecordId::parse(id).expect("valid"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:monocheck".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-23T11:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "2".repeat(64)),
            consent_ref: "consent:mono1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-23T11:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-23T11:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "3".repeat(128))).expect("valid"),
        tags: vec!["mono".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

/// Stage v1, v2, v3. Activate v1, then v2, then v3 (with `expected_prior=Some(2)`).
/// Then attempt to activate v2 again with `expected_prior=Some(3)` → must return
/// `Conflict { kind: ActivationRaced }`. v3 must remain active.
#[tokio::test]
async fn downgrade_activation_returns_activation_raced() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::principal::Principal;

    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("mono-target-1");
    let v1 = make_record(0, "v1 body");
    let v2 = make_record(1, "v2 body");
    let v3 = make_record(2, "v3 body");

    // Stage all three versions in one transaction.
    store
        .with_apply_tx(test_apply_token(), {
            let (r1, r2, r3) = (v1.clone(), v2.clone(), v3.clone());
            let t = target.clone();
            move |tx| {
                tx.stage_version(&t, &r1, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.stage_version(&t, &r2, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.stage_version(&t, &r3, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok(())
            }
        })
        .await
        .expect("stage v1+v2+v3");

    // Activate v1.
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| tx.activate_version(&t, 1, None)
        })
        .await
        .expect("activate v1");

    // Activate v2 (expected_prior = 1).
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| tx.activate_version(&t, 2, Some(1))
        })
        .await
        .expect("activate v2");

    // Activate v3 (expected_prior = 2).
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| tx.activate_version(&t, 3, Some(2))
        })
        .await
        .expect("activate v3");

    // Now try to activate v2 again with expected_prior=Some(3): must conflict.
    let result = store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| tx.activate_version(&t, 2, Some(3))
        })
        .await;

    assert!(
        matches!(
            result,
            Err(StoreError::Conflict {
                kind: ConflictKind::ActivationRaced,
            })
        ),
        "expected ActivationRaced when downgrading activation; got: {result:?}"
    );

    // v3 must still be the active version after the conflict.
    let principal = Principal::system(&test_apply_token());
    let active = store
        .get(&principal, &target)
        .await
        .expect("get")
        .expect("v3 still active");
    assert_eq!(active.body, v3.body, "v3 must remain the active version");
}
