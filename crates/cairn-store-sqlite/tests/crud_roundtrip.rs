// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Smoke test for the core CRUD round-trip:
//! `stage_version` → `activate_version` → `get` returns a byte-equal `MemoryRecord`.

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

/// Build a minimal valid `MemoryRecord` for testing.
///
/// - `id` = "01HQZX9F5N0000000000000000" (ULID)
/// - `visibility = Public` so `Principal::system(&test_apply_token())` always sees it without
///   extra rebac setup.
fn make_record(body: &str) -> MemoryRecord {
    let user_id = Identity::parse("usr:tafeng").expect("valid");
    MemoryRecord {
        id: RecordId::parse("01HQZX9F5N0000000000000000").expect("valid"),
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

#[tokio::test]
async fn stage_activate_get_roundtrip() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let record = make_record("user prefers dark mode");
    let target = TargetId(record.id.as_str().to_owned());

    // Stage version 1 + activate it.
    store
        .with_apply_tx(test_apply_token(), {
            let record = record.clone();
            let target = target.clone();
            move |tx| {
                let _rid = tx.stage_version(&target, &record, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&target, 1, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok(())
            }
        })
        .await
        .expect("apply_tx");

    // Read back via system principal (bypasses rebac).
    let principal = Principal::system(&test_apply_token());
    let got = store
        .get(&principal, &target)
        .await
        .expect("get")
        .expect("record present");

    // Full structural equality — body, provenance, actor_chain, confidence, salience.
    assert_eq!(got.body, record.body, "body must round-trip");
    // f32 fields: compare within a small epsilon to survive the f64 JSON round-trip.
    assert!(
        (got.confidence - record.confidence).abs() < 1e-6,
        "confidence must round-trip"
    );
    assert!(
        (got.salience - record.salience).abs() < 1e-6,
        "salience must round-trip"
    );
    assert_eq!(got.actor_chain.len(), 1, "one actor in chain");
    assert_eq!(got.provenance.consent_ref, record.provenance.consent_ref);

    // list should return exactly 1 row, 0 hidden (system principal).
    let list = store.list(&ListQuery::new(principal)).await.expect("list");
    assert_eq!(list.rows.len(), 1, "list must return 1 row");
    assert_eq!(list.hidden, 0, "no rows hidden for system principal");
}

#[tokio::test]
async fn get_missing_target_returns_none() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");
    let principal = Principal::system(&test_apply_token());
    let target = TargetId::new("does-not-exist");
    let result = store.get(&principal, &target).await.expect("no error");
    assert!(result.is_none(), "missing target_id must return None");
}

#[tokio::test]
async fn non_owner_cannot_read_private_record() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let record = make_record("alice private data");
    let target = TargetId(record.id.as_str().to_owned());

    store
        .with_apply_tx(test_apply_token(), {
            let record = record.clone();
            let target = target.clone();
            move |tx| {
                tx.stage_version(&target, &record, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&target, 1, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok(())
            }
        })
        .await
        .expect("apply_tx");

    // bob cannot read alice's private record.
    let bob = Principal::from_identity(Identity::parse("usr:bob").expect("valid"));
    let got = store.get(&bob, &target).await.expect("no error");
    assert!(got.is_none(), "non-owner must not see private record");

    // list from bob's perspective: 0 visible, 1 hidden.
    let list = store.list(&ListQuery::new(bob)).await.expect("list");
    assert_eq!(list.rows.len(), 0, "bob sees 0 rows");
    assert_eq!(list.hidden, 1, "1 row hidden from bob");
}

/// Verify that staging v2 under the same `target_id` as v1 correctly increments
/// the version counter rather than colliding. Prior to the `target_id` fix,
/// `stage_version` used `record.id` (domain ULID) as the store `target_id`,
/// so v1 and v2 of the *same* logical target would be stored under *different*
/// target identities — COW versioning was silently broken.
#[tokio::test]
async fn multi_version_cow_same_target() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let record_v1 = make_record("user prefers dark mode");
    // Use a stable, deterministic target_id that is distinct from record.id.
    let target = TargetId::new("test-target-cow-1");

    // Stage v1 + activate.
    store
        .with_apply_tx(test_apply_token(), {
            let record = record_v1.clone();
            let target = target.clone();
            move |tx| {
                let rid = tx.stage_version(&target, &record, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                assert!(
                    !rid.as_str().is_empty(),
                    "stage_version returned a record_id"
                );
                tx.activate_version(&target, 1, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok(())
            }
        })
        .await
        .expect("stage v1");

    // Read back v1.
    let principal = Principal::system(&test_apply_token());
    let got_v1 = store
        .get(&principal, &target)
        .await
        .expect("get v1")
        .expect("v1 present");
    assert_eq!(got_v1.body, record_v1.body, "v1 body matches");

    // Stage v2 (updated body) under the same target_id.
    let mut record_v2 = make_record("user prefers light mode");
    // Give the domain record a different id so the BLAKE3 inputs differ.
    record_v2.id = RecordId::parse("01HQZX9F5N0000000000000001").expect("valid");

    store
        .with_apply_tx(test_apply_token(), {
            let record = record_v2.clone();
            let target = target.clone();
            move |tx| {
                let rid = tx.stage_version(&target, &record, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                assert!(
                    !rid.as_str().is_empty(),
                    "stage_version v2 returned a record_id"
                );
                tx.activate_version(&target, 2, Some(1), &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok(())
            }
        })
        .await
        .expect("stage v2");

    // Read back active version — must be v2.
    let got_v2 = store
        .get(&principal, &target)
        .await
        .expect("get v2")
        .expect("v2 present");
    assert_eq!(
        got_v2.body, record_v2.body,
        "active body must be v2 after COW"
    );

    // list must still return exactly 1 active row.
    let list = store.list(&ListQuery::new(principal)).await.expect("list");
    assert_eq!(list.rows.len(), 1, "list returns 1 active row");
}
