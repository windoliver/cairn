// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Per-row rebac filtering at the store layer (brief lines 2557/3287/4136).
//!
//! - `private` records are visible only to their owner.
//! - `team` / `org` collapse to owner-match until membership context
//!   lands; `public` is visible to any identified principal.
//! - `Principal::system(&test_apply_token())` bypasses all checks.
//! - `ListResult::hidden` reports the count of rows the rebac filter
//!   dropped.

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

fn principal_for(identity_str: &str) -> Principal {
    Principal::from_identity(Identity::parse(identity_str).expect("valid identity"))
}

fn make_record(
    id_ulid: &str,
    body: &str,
    owner: &str,
    visibility: MemoryVisibility,
) -> MemoryRecord {
    let user_id = Identity::parse(owner).expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility,
        scope: ScopeTuple {
            user: Some(owner.to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-23T17:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "e".repeat(64)),
            consent_ref: "consent:rebac1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-23T17:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.6,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-23T17:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "1".repeat(128))).expect("valid"),
        tags: vec!["rebac".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

#[tokio::test]
async fn list_drops_private_rows_owned_by_other_principals_and_reports_hidden() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target_alice = TargetId::new("rebac-alice");
    let target_bob = TargetId::new("rebac-bob");
    let target_team = TargetId::new("rebac-team");

    // Stage + activate three records under different (owner, visibility) tuples.
    for (target, ulid, owner, vis, body) in [
        (
            &target_alice,
            "01HQZX9F5N0000000000000090",
            "usr:alice",
            MemoryVisibility::Private,
            "alice's private body",
        ),
        (
            &target_bob,
            "01HQZX9F5N0000000000000091",
            "usr:bob",
            MemoryVisibility::Private,
            "bob's private body",
        ),
        (
            &target_team,
            "01HQZX9F5N0000000000000092",
            "usr:carol",
            MemoryVisibility::Public,
            "carol's public body",
        ),
    ] {
        let rec = make_record(ulid, body, owner, vis);
        let t = target.clone();
        store
            .with_apply_tx(test_apply_token(), move |tx| {
                tx.stage_version(&t, &rec)?;
                tx.activate_version(&t, 1, None)?;
                Ok(())
            })
            .await
            .expect("stage+activate");
    }

    // Alice sees: own private + carol's public. Bob's private is hidden.
    let alice = principal_for("usr:alice");
    let alice_list = store
        .list(&ListQuery::new(alice.clone()))
        .await
        .expect("alice list");
    assert_eq!(alice_list.rows.len(), 2, "alice sees own private + public");
    assert_eq!(alice_list.hidden, 1, "bob's private is hidden");

    // Alice cannot get bob's record by target id.
    let bob_target_for_alice = store
        .get(&alice, &target_bob)
        .await
        .expect("get bob via alice");
    assert!(
        bob_target_for_alice.is_none(),
        "alice must not see bob's private record"
    );

    // System sees all three.
    let system = Principal::system(&test_apply_token());
    let sys_list = store.list(&ListQuery::new(system)).await.expect("sys list");
    assert_eq!(sys_list.rows.len(), 3);
    assert_eq!(sys_list.hidden, 0);
}
