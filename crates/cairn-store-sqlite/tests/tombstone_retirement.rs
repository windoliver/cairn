// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Tombstone retirement: once any version of a target is tombstoned,
//! the target is permanently retired. `stage_version`,
//! `activate_version`, `expire_active`, and `add_edge` must all fail
//! closed.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{
    apply::MemoryStoreApply,
    error::{ConflictKind, StoreError},
    types::{Edge, EdgeKind, TargetId},
};
use cairn_core::domain::{
    ChainRole, EvidenceVector, MemoryClass, MemoryKind, MemoryVisibility, Provenance,
    Rfc3339Timestamp, ScopeTuple,
    actor_chain::ActorChainEntry,
    actor_ref::ActorRef,
    identity::Identity,
    record::{Ed25519Signature, MemoryRecord, RecordId},
};
use cairn_core::wal::test_apply_token;
use cairn_store_sqlite::SqliteMemoryStore;
use tempfile::tempdir;

fn record(id_ulid: &str) -> MemoryRecord {
    let owner = Identity::parse("usr:tomb").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:tomb".to_owned()),
            ..ScopeTuple::default()
        },
        body: "tomb body".to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-29T11:00:00Z").expect("valid"),
            originating_agent_id: owner.clone(),
            source_hash: format!("sha256:{}", "8".repeat(64)),
            consent_ref: "consent:tomb".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-29T11:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: owner,
            at: Rfc3339Timestamp::parse("2026-04-29T11:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "9".repeat(128))).expect("valid"),
        tags: vec!["tomb".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

async fn open_store_with_active_target(
    target_ulid: &str,
) -> (SqliteMemoryStore, TargetId, RecordId) {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");
    let target = TargetId::new("tomb-target");
    let rec = record(target_ulid);
    let actor = ActorRef::from_string("agt:test:integration:m:v1");
    let actor2 = actor.clone();
    let target_a = target.clone();
    let target_b = target.clone();
    let rec_clone = rec.clone();
    store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.stage_version(&target_a, &rec_clone, &actor)?;
            Ok(())
        })
        .await
        .expect("stage");
    store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.activate_version(&target_b, 1, None, &actor2)
        })
        .await
        .expect("activate");
    // Forget tempdir so the store outlives this fn.
    std::mem::forget(dir);
    (store, target, rec.id)
}

async fn tombstone(store: &SqliteMemoryStore, target: &TargetId) {
    let target = target.clone();
    let actor = ActorRef::from_string("agt:test:integration:m:v1");
    store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.tombstone_target(&target, &actor)
        })
        .await
        .expect("tombstone");
}

#[tokio::test]
async fn stage_version_rejected_after_tombstone() {
    let (store, target, _) = open_store_with_active_target("01HQZX9F5N0000000000000T01").await;
    tombstone(&store, &target).await;

    let target_clone = target.clone();
    let rec = record("01HQZX9F5N0000000000000T02");
    let actor = ActorRef::from_string("agt:test:integration:m:v1");
    let result = store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.stage_version(&target_clone, &rec, &actor)
        })
        .await;
    assert!(
        matches!(
            result,
            Err(StoreError::Conflict {
                kind: ConflictKind::UniqueViolation
            })
        ),
        "stage after tombstone must conflict; got: {result:?}"
    );
}

#[tokio::test]
async fn activate_version_rejected_after_tombstone() {
    // Stage v2 BEFORE tombstone so activate sees an existing version,
    // then tombstone, then attempt activate-on-stale.
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");
    let target = TargetId::new("tomb-activate");
    let actor = ActorRef::from_string("agt:test:integration:m:v1");

    let target_a = target.clone();
    let actor_a = actor.clone();
    let r1 = record("01HQZX9F5N0000000000000T11");
    let r2 = record("01HQZX9F5N0000000000000T12");
    store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.stage_version(&target_a, &r1, &actor_a)?;
            tx.stage_version(&target_a, &r2, &actor_a)?;
            Ok(())
        })
        .await
        .expect("stage v1+v2");

    let target_b = target.clone();
    let actor_b = actor.clone();
    store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.activate_version(&target_b, 1, None, &actor_b)
        })
        .await
        .expect("activate v1");

    tombstone(&store, &target).await;

    // Now try to activate v2 — must reject.
    let target_c = target.clone();
    let actor_c = actor.clone();
    let result = store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.activate_version(&target_c, 2, Some(1), &actor_c)
        })
        .await;
    assert!(
        matches!(
            result,
            Err(StoreError::Conflict {
                kind: ConflictKind::UniqueViolation
            })
        ),
        "activate after tombstone must conflict; got: {result:?}"
    );
}

#[tokio::test]
async fn expire_active_rejected_after_tombstone() {
    let (store, target, _) = open_store_with_active_target("01HQZX9F5N0000000000000T21").await;
    tombstone(&store, &target).await;

    let target_clone = target.clone();
    let result = store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.expire_active(
                &target_clone,
                Rfc3339Timestamp::parse("2026-04-29T12:00:00Z").expect("valid"),
            )
        })
        .await;
    assert!(
        matches!(
            result,
            Err(StoreError::Conflict {
                kind: ConflictKind::UniqueViolation
            })
        ),
        "expire after tombstone must conflict; got: {result:?}"
    );
}

#[tokio::test]
async fn add_edge_rejected_when_endpoint_target_is_tombstoned() {
    // Two independent targets: A is healthy, B becomes tombstoned.
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");
    let actor = ActorRef::from_string("agt:test:integration:m:v1");

    let target_a = TargetId::new("edge-A");
    let target_b = TargetId::new("edge-B");

    let rec_a = record("01HQZX9F5N00000000000ED001");
    let rec_b = record("01HQZX9F5N00000000000ED002");
    // stage_version writes the deterministic record_id derived from
    // (target_id, version) — not rec.id — so reconstruct it the same
    // way for the edge endpoints.
    let id_a =
        cairn_core::contract::memory_store::types::RecordId::from_target_version(&target_a, 1);
    let id_b =
        cairn_core::contract::memory_store::types::RecordId::from_target_version(&target_b, 1);

    let target_alpha = target_a.clone();
    let target_bravo = target_b.clone();
    let writer = actor.clone();
    store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.stage_version(&target_alpha, &rec_a, &writer)?;
            tx.stage_version(&target_bravo, &rec_b, &writer)?;
            tx.activate_version(&target_alpha, 1, None, &writer)?;
            tx.activate_version(&target_bravo, 1, None, &writer)?;
            Ok(())
        })
        .await
        .expect("stage+activate both");

    // Tombstone B.
    tombstone(&store, &target_b).await;

    // Adding A→B must be rejected.
    let edge = Edge {
        from: id_a.clone(),
        to: id_b.clone(),
        kind: EdgeKind::Refines,
        weight: 1.0,
        metadata: serde_json::json!({}),
    };
    let result = store
        .with_apply_tx(test_apply_token(), move |tx| tx.add_edge(&edge))
        .await;
    assert!(
        matches!(
            result,
            Err(StoreError::Conflict {
                kind: ConflictKind::UniqueViolation
            })
        ),
        "add_edge to tombstoned endpoint must conflict; got: {result:?}"
    );

    // Adding A→A on the healthy target must still work.
    let healthy_edge = Edge {
        from: id_a.clone(),
        to: id_a,
        kind: EdgeKind::Refines,
        weight: 1.0,
        metadata: serde_json::json!({}),
    };
    store
        .with_apply_tx(test_apply_token(), move |tx| tx.add_edge(&healthy_edge))
        .await
        .expect("healthy edge should succeed");
}
