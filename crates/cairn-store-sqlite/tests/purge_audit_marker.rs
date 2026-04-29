// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! `purge_target`: physically removes all record rows, cleans up graph edges
//! and FTS, and writes an audit marker into `record_purges`. Re-invocation
//! with the same `(target_id, op_id)` returns `PurgeOutcome::AlreadyPurged`.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::types::Edge;
use cairn_core::contract::memory_store::{
    MemoryStore,
    apply::MemoryStoreApply,
    types::{EdgeKind, HistoryEntry, OpId, PurgeOutcome, TargetId},
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
    let user_id = Identity::parse("usr:purgetest").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:purgetest".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-23T12:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "4".repeat(64)),
            consent_ref: "consent:purge1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-23T12:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-23T12:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "5".repeat(128))).expect("valid"),
        tags: vec!["purge".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

/// Stage 3 versions (activating each), add edges, then purge. Assert:
/// - zero `records` rows remain for the captured `record_id`s.
/// - zero `edges` rows remain.
/// - zero `edge_versions` rows remain.
/// - one row in `record_purges`.
/// - `version_history(system)` returns one `HistoryEntry::Purge`.
/// - Re-purge with same `(target_id, op_id)` → `AlreadyPurged`, no extra rows.
#[tokio::test]
#[allow(clippy::too_many_lines)] // integration test: linear setup + many asserts
async fn purge_removes_records_edges_and_writes_audit_marker() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let store = SqliteMemoryStore::open(&db_path).await.expect("open");

    let target = TargetId::new("purge-target-1");
    let other_target = TargetId::new("purge-other-target-1");

    // Stage v1, v2, v3 for the target to be purged.
    let (rid_v1, rid_v2, rid_v3) = store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| {
                let r1 = make_record("01HQZX9F5N0000000000000050", "purge v1");
                let r2 = make_record("01HQZX9F5N0000000000000051", "purge v2");
                let r3 = make_record("01HQZX9F5N0000000000000052", "purge v3");
                let id1 = tx.stage_version(&t, &r1, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                let id2 = tx.stage_version(&t, &r2, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                let id3 = tx.stage_version(&t, &r3, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                // Activate sequentially.
                tx.activate_version(&t, 1, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&t, 2, Some(1), &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&t, 3, Some(2), &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok((id1, id2, id3))
            }
        })
        .await
        .expect("stage v1+v2+v3");

    // Stage + activate another record (the edge "other" side).
    let rid_other = store
        .with_apply_tx(test_apply_token(), {
            let t = other_target.clone();
            move |tx| {
                let rec = make_record("01HQZX9F5N0000000000000053", "other record");
                let rid = tx.stage_version(&t, &rec, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&t, 1, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok(rid)
            }
        })
        .await
        .expect("stage other record");

    // Add an edge from v3 → other.
    store
        .with_apply_tx(test_apply_token(), {
            let from = rid_v3.clone();
            let to = rid_other.clone();
            move |tx| {
                tx.add_edge(&Edge {
                    from,
                    to,
                    kind: EdgeKind::SeeAlso,
                    weight: 0.5,
                    metadata: serde_json::json!({}),
                })?;
                Ok(())
            }
        })
        .await
        .expect("add edge");

    let actor = ActorRef::from_string("usr:purgetest");
    let op_id = OpId::new("op-purge-001");

    // Purge the target.
    let outcome = store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let op = op_id.clone();
            let a = actor.clone();
            move |tx| tx.purge_target(&t, &op, &a)
        })
        .await
        .expect("purge_target");
    assert_eq!(outcome, PurgeOutcome::Purged, "expected Purged outcome");

    // Verify via raw SQL that records/edges/edge_versions are all gone.
    let conn = Connection::open(&db_path).expect("raw conn");

    for rid in &[&rid_v1, &rid_v2, &rid_v3] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM records WHERE record_id = ?1",
                rusqlite::params![rid.as_str()],
                |r| r.get(0),
            )
            .expect("records count");
        assert_eq!(count, 0, "records row for {rid:?} must be gone after purge");

        let edge_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE from_id = ?1 OR to_id = ?1",
                rusqlite::params![rid.as_str()],
                |r| r.get(0),
            )
            .expect("edges count");
        assert_eq!(
            edge_count, 0,
            "edges row referencing {rid:?} must be gone after purge"
        );

        let ev_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edge_versions WHERE from_id = ?1 OR to_id = ?1",
                rusqlite::params![rid.as_str()],
                |r| r.get(0),
            )
            .expect("edge_versions count");
        assert_eq!(
            ev_count, 0,
            "edge_versions row referencing {rid:?} must be gone after purge"
        );
    }

    // One audit marker in record_purges.
    let purge_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM record_purges WHERE target_id = ?1",
            rusqlite::params![target.as_str()],
            |r| r.get(0),
        )
        .expect("record_purges count");
    assert_eq!(purge_count, 1, "exactly one purge audit marker must exist");

    // version_history(system) must return one Purge entry.
    let principal = Principal::system(&test_apply_token());
    let history = store
        .version_history(&principal, &target)
        .await
        .expect("version_history");
    assert_eq!(
        history.len(),
        1,
        "version_history must return one Purge entry after purge"
    );
    assert!(
        matches!(history[0], HistoryEntry::Purge(_)),
        "history entry must be a Purge variant; got: {:?}",
        history[0]
    );

    // Re-purge with same (target, op_id) → AlreadyPurged.
    let outcome2 = store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let op = op_id.clone();
            let a = actor.clone();
            move |tx| tx.purge_target(&t, &op, &a)
        })
        .await
        .expect("re-purge");
    assert_eq!(
        outcome2,
        PurgeOutcome::AlreadyPurged,
        "re-purge with same op_id must be AlreadyPurged"
    );

    // No extra purge rows.
    let purge_count2: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM record_purges WHERE target_id = ?1",
            rusqlite::params![target.as_str()],
            |r| r.get(0),
        )
        .expect("record_purges count after re-purge");
    assert_eq!(
        purge_count2, 1,
        "re-purge must not add extra audit rows; still expect exactly 1"
    );
}

/// Purge markers are rebac-filtered using the scope/taxonomy
/// snapshot captured at purge time (migration 0010): the owner who
/// could read the row sees the audit marker; an unrelated principal
/// does not. System principals always see every marker.
#[tokio::test]
async fn purge_marker_visibility_uses_purge_time_snapshot() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("purge-marker-vis");
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| {
                let rec = make_record("01HQZX9F5N0000000000000054", "to be purged");
                tx.stage_version(&t, &rec, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&t, 1, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok(())
            }
        })
        .await
        .expect("stage+activate");

    let actor = ActorRef::from_string("usr:purgetest");
    let op_id = OpId::new("op-purge-vis");
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let op = op_id.clone();
            let a = actor.clone();
            move |tx| tx.purge_target(&t, &op, &a)
        })
        .await
        .expect("purge");

    // System sees the purge marker.
    let system = Principal::system(&test_apply_token());
    let sys_history = store
        .version_history(&system, &target)
        .await
        .expect("version_history (system)");
    assert_eq!(sys_history.len(), 1, "system must observe the purge marker");
    assert!(matches!(sys_history[0], HistoryEntry::Purge(_)));

    // The owner who could read the original row sees the purge marker
    // via the captured scope/taxonomy snapshot.
    let owner = Principal::from_identity(Identity::parse("usr:purgetest").expect("valid"));
    let owner_history = store
        .version_history(&owner, &target)
        .await
        .expect("version_history (owner)");
    assert_eq!(
        owner_history.len(),
        1,
        "owner must observe the rebac-gated purge marker; got: {owner_history:?}"
    );
    assert!(matches!(owner_history[0], HistoryEntry::Purge(_)));

    // An unrelated principal must NOT see the marker.
    let outsider = Principal::from_identity(Identity::parse("usr:outsider").expect("valid"));
    let outsider_history = store
        .version_history(&outsider, &target)
        .await
        .expect("version_history (outsider)");
    assert!(
        outsider_history.is_empty(),
        "non-owner must not observe the purge marker; got: {outsider_history:?}"
    );
}

/// Regression: once a `target_id` has been purged, it must not be
/// re-stageable. Re-staging would start at version 1 again and produce
/// the same deterministic `record_id` as the purged record, splicing
/// a new logical row into the old purge audit history.
#[tokio::test]
async fn purged_target_id_cannot_be_restaged() {
    use cairn_core::contract::memory_store::error::{ConflictKind, StoreError};

    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("retired-after-purge");
    let actor = ActorRef::from_string("usr:purgetest");

    // Stage + activate + purge.
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| {
                let r = make_record("01HQZX9F5N0000000000000077", "first life");
                tx.stage_version(&t, &r, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&t, 1, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                Ok(())
            }
        })
        .await
        .expect("stage+activate");

    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let a = actor.clone();
            move |tx| tx.purge_target(&t, &OpId::new("op-retire"), &a)
        })
        .await
        .expect("purge");

    // Re-staging the same target_id must now fail.
    let result = store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| {
                let r = make_record("01HQZX9F5N0000000000000088", "second life");
                tx.stage_version(&t, &r, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))
            }
        })
        .await;
    assert!(
        matches!(
            result,
            Err(StoreError::Conflict {
                kind: ConflictKind::UniqueViolation
            })
        ),
        "re-stage of purged target must return Conflict; got: {result:?}"
    );
}

/// Purging a target that has no records and no prior marker must
/// return `NotFound` rather than fabricate audit history.
#[tokio::test]
async fn purge_nonexistent_target_returns_not_found() {
    use cairn_core::contract::memory_store::error::StoreError;

    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("never-existed");
    let actor = ActorRef::from_string("usr:purgetest");
    let op_id = OpId::new("op-purge-noop");

    let result = store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let op = op_id.clone();
            let a = actor.clone();
            move |tx| tx.purge_target(&t, &op, &a)
        })
        .await;
    assert!(
        matches!(result, Err(StoreError::NotFound(_))),
        "purge of nonexistent target must return NotFound; got: {result:?}"
    );
}
