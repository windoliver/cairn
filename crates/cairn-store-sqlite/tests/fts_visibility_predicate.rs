// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! FTS5 visibility predicate (brief §5.4 expiry fence + tombstone fence).
//!
//! Issue #46 ships the FTS5 virtual table + sync triggers; the search
//! verb itself lands in #47. This test exercises the predicate inline:
//! a candidate row matches only if active=1, tombstoned=0, and not
//! past its `expired_at`.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{apply::MemoryStoreApply, types::TargetId};
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
    let user_id = Identity::parse("usr:ftstest").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:ftstest".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-23T18:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "2".repeat(64)),
            consent_ref: "consent:fts1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-23T18:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.6,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-23T18:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "3".repeat(128))).expect("valid"),
        tags: vec!["fts".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

#[tokio::test]
async fn fts_match_filters_tombstoned_and_expired() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let store = SqliteMemoryStore::open(&db_path).await.expect("open");

    let target_visible = TargetId::new("fts-visible");
    let target_tomb = TargetId::new("fts-tombstoned");
    let target_expired = TargetId::new("fts-expired");
    let actor = ActorRef::from_string("usr:ftstest");

    // All three records share the keyword "needle" so only the predicate
    // distinguishes them.
    for (target, ulid) in [
        (&target_visible, "01HQZX9F5N00000000000000A0"),
        (&target_tomb, "01HQZX9F5N00000000000000A1"),
        (&target_expired, "01HQZX9F5N00000000000000A2"),
    ] {
        let rec = make_record(ulid, "needle in this haystack");
        let t = target.clone();
        store
            .with_apply_tx(test_apply_token(), move |tx| {
                tx.stage_version(&t, &rec, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&t, 1, None)?;
                Ok(())
            })
            .await
            .expect("stage+activate");
    }

    // Tombstone one + expire another (past timestamp).
    let tomb_target = target_tomb.clone();
    let actor_clone = actor.clone();
    store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.tombstone_target(&tomb_target, &actor_clone)?;
            Ok(())
        })
        .await
        .expect("tombstone");

    let exp_target = target_expired.clone();
    store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.expire_active(
                &exp_target,
                Rfc3339Timestamp::parse("2020-01-01T00:00:00Z").expect("valid past"),
            )?;
            Ok(())
        })
        .await
        .expect("expire");

    // FTS query joined with the visibility predicate.
    let conn = Connection::open(&db_path).expect("raw conn");
    let mut stmt = conn
        .prepare(
            "SELECT records.target_id FROM records_fts \
             JOIN records ON records.rowid = records_fts.rowid \
             WHERE records_fts MATCH ?1 \
               AND records.active = 1 \
               AND records.tombstoned = 0 \
               AND (records.expired_at IS NULL OR records.expired_at > ?2)",
        )
        .expect("prepare");
    let now = Rfc3339Timestamp::now();
    let mut rows = stmt
        .query(rusqlite::params!["needle", now.as_str()])
        .expect("query");

    let mut hits: Vec<String> = Vec::new();
    while let Some(row) = rows.next().expect("row") {
        let tid: String = row.get(0).expect("target_id");
        hits.push(tid);
    }
    assert_eq!(
        hits,
        vec![target_visible.as_str().to_owned()],
        "only the visible row must match: {hits:?}"
    );
}
