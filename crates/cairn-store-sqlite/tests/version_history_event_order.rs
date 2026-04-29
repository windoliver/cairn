// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! `version_history` event-list reconstruction:
//! - `Expire` and `Tombstone` events persist on a version even after
//!   it is superseded by a newer activation.
//! - Returned events are sorted ascending by timestamp.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{
    MemoryStore,
    apply::MemoryStoreApply,
    types::{ChangeKind, HistoryEntry, TargetId},
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
use tempfile::tempdir;

fn make_record(id_ulid: &str, body: &str) -> MemoryRecord {
    let user_id = Identity::parse("usr:vhtest").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:vhtest".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-24T08:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "6".repeat(64)),
            consent_ref: "consent:vh1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-24T08:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.6,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-24T08:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "7".repeat(128))).expect("valid"),
        tags: vec!["vh".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

#[tokio::test]
async fn expire_event_persists_after_supersession() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("vh-expire-supersede");

    // Stage + activate v1.
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| {
                let r = make_record("01HQZX9F5N00000000000000C0", "v1 body");
                tx.stage_version(&t, &r, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&t, 1, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok(())
            }
        })
        .await
        .expect("stage v1");

    // Expire active (v1).
    let past = Rfc3339Timestamp::parse("2025-01-01T00:00:00Z").expect("valid");
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let exp = past.clone();
            move |tx| {
                tx.expire_active(&t, exp)?;
                Ok(())
            }
        })
        .await
        .expect("expire");

    // Stage + activate v2 → v1 becomes superseded.
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| {
                let r = make_record("01HQZX9F5N00000000000000C1", "v2 body");
                tx.stage_version(&t, &r, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&t, 2, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok(())
            }
        })
        .await
        .expect("stage v2");

    let history = store
        .version_history(&Principal::system(&test_apply_token()), &target)
        .await
        .expect("version_history");

    let v1 = history
        .iter()
        .find_map(|e| match e {
            HistoryEntry::Version(v) if v.version == 1 => Some(v),
            _ => None,
        })
        .expect("v1 present");

    // The Expire event must still be present even though v1 is superseded.
    assert!(
        v1.events.iter().any(|e| e.kind == ChangeKind::Expire),
        "v1 history must retain Expire event after supersession; got: {:?}",
        v1.events
    );
}

#[tokio::test]
async fn events_are_sorted_by_timestamp() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("vh-sort");
    let actor = ActorRef::from_string("usr:vhtest");

    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| {
                let r = make_record("01HQZX9F5N00000000000000C2", "body");
                tx.stage_version(&t, &r, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&t, 1, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok(())
            }
        })
        .await
        .expect("stage");

    // Tombstone (timestamp `now`).
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let a = actor.clone();
            move |tx| {
                tx.tombstone_target(&t, &a)?;
                Ok(())
            }
        })
        .await
        .expect("tombstone");

    let history = store
        .version_history(&Principal::system(&test_apply_token()), &target)
        .await
        .expect("version_history");
    let HistoryEntry::Version(v) = &history[0] else {
        panic!("expected Version entry");
    };

    // All event timestamps must be ascending.
    let times: Vec<_> = v.events.iter().filter_map(|e| e.at.as_ref()).collect();
    for pair in times.windows(2) {
        assert!(
            pair[0].as_str() <= pair[1].as_str(),
            "events out of order: {:?}",
            v.events
        );
    }
}
