// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Tombstoning a target must not rewrite earlier `Update` events, must
//! append a `Tombstone` event to every version row, and `get` returns `None`.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::types::ChangeKind;
use cairn_core::contract::memory_store::{
    MemoryStore,
    apply::MemoryStoreApply,
    types::{HistoryEntry, TargetId},
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

fn make_record(id_suffix: &str, body: &str) -> MemoryRecord {
    let user_id = Identity::parse("usr:tombtest").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_suffix).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:tombtest".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-23T10:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "d".repeat(64)),
            consent_ref: "consent:tomb1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-23T10:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.4,
        confidence: 0.6,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-23T10:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "e".repeat(128))).expect("valid"),
        tags: vec!["tomb".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

/// Stage v1 → activate → stage v2 → activate → tombstone. Assert:
/// - `version_history` returns 2 `HistoryEntry::Version` entries.
/// - Both versions have an `Update` event as the first event.
/// - Both versions have a `Tombstone` event appended.
/// - The `Update` event `at` timestamp is preserved (not rewritten).
/// - `get` returns `None` after tombstoning.
#[tokio::test]
#[allow(clippy::too_many_lines)] // integration test: linear setup + many asserts
async fn tombstone_appends_to_both_versions_without_rewriting_update() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("tombstone-target-1");
    let v1 = make_record("01HQZX9F5N0000000000000020", "v1 body");
    let v2 = make_record("01HQZX9F5N0000000000000021", "v2 body");

    // Stage + activate v1.
    store
        .with_apply_tx(test_apply_token(), {
            let rec = v1.clone();
            let t = target.clone();
            move |tx| {
                tx.stage_version(
                    &t,
                    &rec,
                    &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"),
                )?;
                tx.activate_version(
                    &t,
                    1,
                    None,
                    &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"),
                )?;
                Ok(())
            }
        })
        .await
        .expect("stage+activate v1");

    // Stage + activate v2.
    store
        .with_apply_tx(test_apply_token(), {
            let rec = v2.clone();
            let t = target.clone();
            move |tx| {
                tx.stage_version(
                    &t,
                    &rec,
                    &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"),
                )?;
                tx.activate_version(
                    &t,
                    2,
                    Some(1),
                    &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"),
                )?;
                Ok(())
            }
        })
        .await
        .expect("stage+activate v2");

    let actor = ActorRef::from_string("usr:tombtest");

    // Tombstone the target.
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

    // get must return None for a tombstoned target.
    let principal = Principal::system(&test_apply_token());
    let got = store
        .get(&principal, &target)
        .await
        .expect("get must not error");
    assert!(got.is_none(), "get after tombstone must return None");

    // version_history must return 2 Version entries.
    let history = store
        .version_history(&principal, &target)
        .await
        .expect("version_history");
    assert_eq!(
        history.len(),
        2,
        "version_history must return 2 entries after tombstone; got: {history:?}"
    );

    let versions: Vec<_> = history
        .iter()
        .filter_map(|e| {
            if let HistoryEntry::Version(v) = e {
                Some(v)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(versions.len(), 2, "both entries must be Version variants");

    for v in &versions {
        // First event is always Update.
        assert!(
            !v.events.is_empty(),
            "version {} must have at least one event",
            v.version
        );
        let first_kind = v.events.first().map(|e| e.kind);
        assert_eq!(
            first_kind,
            Some(ChangeKind::Update),
            "first event of version {} must be Update; got: {first_kind:?}",
            v.version
        );
        // Last event is Tombstone.
        let last_kind = v.events.last().map(|e| e.kind);
        assert_eq!(
            last_kind,
            Some(ChangeKind::Tombstone),
            "last event of version {} must be Tombstone; got: {last_kind:?}",
            v.version
        );
        // The Tombstone event has an `at` timestamp.
        let tombstone_evt = v
            .events
            .iter()
            .find(|e| e.kind == ChangeKind::Tombstone)
            .expect("Tombstone event must exist");
        assert!(
            tombstone_evt.at.is_some(),
            "tombstone event on version {} must have an `at` timestamp",
            v.version
        );
    }
}

/// Tombstoning is idempotent: tombstoning again is a no-op, `get` still
/// returns `None`, and `version_history` still returns exactly 2 entries.
#[tokio::test]
async fn tombstone_is_idempotent() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("tombstone-idempotent-1");
    let v1 = make_record("01HQZX9F5N0000000000000022", "body for idem test");
    let actor = ActorRef::from_string("usr:tombtest");

    store
        .with_apply_tx(test_apply_token(), {
            let rec = v1.clone();
            let t = target.clone();
            move |tx| {
                tx.stage_version(
                    &t,
                    &rec,
                    &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"),
                )?;
                tx.activate_version(
                    &t,
                    1,
                    None,
                    &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"),
                )?;
                Ok(())
            }
        })
        .await
        .expect("stage+activate");

    // First tombstone.
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
        .expect("first tombstone");

    // Second tombstone — must be a no-op (no error).
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
        .expect("second tombstone (idempotent)");

    let principal = Principal::system(&test_apply_token());
    assert!(
        store.get(&principal, &target).await.expect("get").is_none(),
        "get after double-tombstone must still return None"
    );
}
