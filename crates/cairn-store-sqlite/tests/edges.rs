// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Edge add/upsert/remove semantics + history (`edge_versions`) audit log.
//! Tombstoning an endpoint must NOT cascade-delete the edge — purge does
//! that, not tombstone (brief §10 forget pipeline).

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{
    apply::MemoryStoreApply,
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
use rusqlite::Connection;
use tempfile::tempdir;

fn make_record(id_ulid: &str, body: &str) -> MemoryRecord {
    let user_id = Identity::parse("usr:edgetest").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:edgetest".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-23T08:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "7".repeat(64)),
            consent_ref: "consent:edge1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-23T08:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.4,
        confidence: 0.6,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-23T08:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "8".repeat(128))).expect("valid"),
        tags: vec!["edge".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

fn count_edges(conn: &Connection, from: &str, to: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM edges WHERE from_id = ?1 AND to_id = ?2",
        rusqlite::params![from, to],
        |r| r.get(0),
    )
    .expect("count edges")
}

fn count_edge_versions_by_kind(conn: &Connection, from: &str, to: &str, change_kind: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM edge_versions \
         WHERE from_id = ?1 AND to_id = ?2 AND change_kind = ?3",
        rusqlite::params![from, to, change_kind],
        |r| r.get(0),
    )
    .expect("count edge_versions")
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // integration test: linear setup + multi-phase asserts
async fn add_upsert_remove_writes_audit_markers() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let store = SqliteMemoryStore::open(&db_path).await.expect("open");

    let target_a = TargetId::new("edge-target-a");
    let target_b = TargetId::new("edge-target-b");

    // Stage + activate two records.
    let (rid_a, rid_b) = store
        .with_apply_tx(test_apply_token(), {
            let ta = target_a.clone();
            let tb = target_b.clone();
            move |tx| {
                let ra = make_record("01HQZX9F5N0000000000000060", "record A body");
                let rb = make_record("01HQZX9F5N0000000000000061", "record B body");
                let id_a = tx.stage_version(&ta, &ra)?;
                let id_b = tx.stage_version(&tb, &rb)?;
                tx.activate_version(&ta, 1, None)?;
                tx.activate_version(&tb, 1, None)?;
                Ok((id_a, id_b))
            }
        })
        .await
        .expect("stage two records");

    // Insert edge.
    store
        .with_apply_tx(test_apply_token(), {
            let from = rid_a.clone();
            let to = rid_b.clone();
            move |tx| {
                tx.add_edge(&Edge {
                    from,
                    to,
                    kind: EdgeKind::SeeAlso,
                    weight: 0.4,
                    metadata: serde_json::json!({"src": "test"}),
                })?;
                Ok(())
            }
        })
        .await
        .expect("add edge");

    let conn = Connection::open(&db_path).expect("raw conn");
    assert_eq!(count_edges(&conn, rid_a.as_str(), rid_b.as_str()), 1);
    assert_eq!(
        count_edge_versions_by_kind(&conn, rid_a.as_str(), rid_b.as_str(), "insert"),
        1,
        "insert marker missing"
    );
    drop(conn);

    // Re-add same (from, to, kind) with new weight → upsert; insert+update markers.
    store
        .with_apply_tx(test_apply_token(), {
            let from = rid_a.clone();
            let to = rid_b.clone();
            move |tx| {
                tx.add_edge(&Edge {
                    from,
                    to,
                    kind: EdgeKind::SeeAlso,
                    weight: 0.9,
                    metadata: serde_json::json!({"src": "test", "v": 2}),
                })?;
                Ok(())
            }
        })
        .await
        .expect("re-add edge");

    let conn = Connection::open(&db_path).expect("raw conn");
    assert_eq!(
        count_edges(&conn, rid_a.as_str(), rid_b.as_str()),
        1,
        "edge row remains a single upsert"
    );
    assert_eq!(
        count_edge_versions_by_kind(&conn, rid_a.as_str(), rid_b.as_str(), "update"),
        1,
        "update marker missing"
    );
    let weight: f64 = conn
        .query_row(
            "SELECT weight FROM edges WHERE from_id = ?1 AND to_id = ?2",
            rusqlite::params![rid_a.as_str(), rid_b.as_str()],
            |r| r.get(0),
        )
        .expect("weight");
    assert!((weight - 0.9).abs() < 1e-6, "weight must reflect upsert");
    drop(conn);

    // Remove edge.
    store
        .with_apply_tx(test_apply_token(), {
            let from = rid_a.clone();
            let to = rid_b.clone();
            move |tx| {
                tx.remove_edge(&from, &to, EdgeKind::SeeAlso)?;
                Ok(())
            }
        })
        .await
        .expect("remove edge");

    let conn = Connection::open(&db_path).expect("raw conn");
    assert_eq!(count_edges(&conn, rid_a.as_str(), rid_b.as_str()), 0);
    assert_eq!(
        count_edge_versions_by_kind(&conn, rid_a.as_str(), rid_b.as_str(), "remove"),
        1,
        "remove marker missing"
    );
}

#[tokio::test]
async fn tombstone_endpoint_does_not_remove_edge() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let store = SqliteMemoryStore::open(&db_path).await.expect("open");

    let target_a = TargetId::new("edge-tomb-a");
    let target_b = TargetId::new("edge-tomb-b");

    let (rid_a, rid_b) = store
        .with_apply_tx(test_apply_token(), {
            let ta = target_a.clone();
            let tb = target_b.clone();
            move |tx| {
                let ra = make_record("01HQZX9F5N0000000000000062", "tomb endpoint A");
                let rb = make_record("01HQZX9F5N0000000000000063", "tomb endpoint B");
                let id_a = tx.stage_version(&ta, &ra)?;
                let id_b = tx.stage_version(&tb, &rb)?;
                tx.activate_version(&ta, 1, None)?;
                tx.activate_version(&tb, 1, None)?;
                Ok((id_a, id_b))
            }
        })
        .await
        .expect("stage");

    store
        .with_apply_tx(test_apply_token(), {
            let from = rid_a.clone();
            let to = rid_b.clone();
            move |tx| {
                tx.add_edge(&Edge {
                    from,
                    to,
                    kind: EdgeKind::Refines,
                    weight: 0.5,
                    metadata: serde_json::json!({}),
                })?;
                Ok(())
            }
        })
        .await
        .expect("add edge");

    // Tombstone source.
    let actor = ActorRef::from_string("usr:edgetest");
    store
        .with_apply_tx(test_apply_token(), {
            let t = target_a.clone();
            let a = actor.clone();
            move |tx| {
                tx.tombstone_target(&t, &a)?;
                Ok(())
            }
        })
        .await
        .expect("tombstone source");

    let conn = Connection::open(&db_path).expect("raw conn");
    assert_eq!(
        count_edges(&conn, rid_a.as_str(), rid_b.as_str()),
        1,
        "edge must survive tombstone of endpoint (only purge cascades)"
    );
}
