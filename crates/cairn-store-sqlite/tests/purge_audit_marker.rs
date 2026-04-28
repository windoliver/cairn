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
                let id1 = tx.stage_version(&t, &r1)?;
                let id2 = tx.stage_version(&t, &r2)?;
                let id3 = tx.stage_version(&t, &r3)?;
                // Activate sequentially.
                tx.activate_version(&t, 1, None)?;
                tx.activate_version(&t, 2, Some(1))?;
                tx.activate_version(&t, 3, Some(2))?;
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
                let rid = tx.stage_version(&t, &rec)?;
                tx.activate_version(&t, 1, None)?;
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
    let principal = Principal::system();
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
