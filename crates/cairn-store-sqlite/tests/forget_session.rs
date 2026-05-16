//! Issue #108: session-level forget fan-out must be a WAL-backed store operation.

#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::time::Duration;

use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::{MemoryRecord, RecordId, ScopeTuple, TargetId};
use cairn_store_sqlite::StoreError;
use cairn_store_sqlite::open_in_memory;
use rusqlite::params;

fn sample_record() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

fn session_record(record_id: &str, target_id: &str, body: &str, session_id: &str) -> MemoryRecord {
    let mut record = sample_record();
    record.id = RecordId::parse(record_id).expect("valid record id");
    record.target_id = TargetId::parse(target_id).expect("valid target id");
    body.clone_into(&mut record.body);
    record.scope = ScopeTuple {
        session_id: Some(session_id.to_owned()),
        user: record.scope.user.clone(),
        agent: record.scope.agent.clone(),
        ..ScopeTuple::default()
    };
    record
}

fn consolidation_summary_record(
    record_id: &str,
    target_id: &str,
    source_record_ids: &[&str],
) -> MemoryRecord {
    let mut record = sample_record();
    record.id = RecordId::parse(record_id).expect("valid record id");
    record.target_id = TargetId::parse(target_id).expect("valid target id");
    "derived consolidation summary".clone_into(&mut record.body);
    record.scope = ScopeTuple {
        user: Some("hmn:summaryfixture".to_owned()),
        ..ScopeTuple::default()
    };
    record.extra_frontmatter = BTreeMap::from([(
        "consolidation".to_owned(),
        serde_json::json!({
            "source_record_ids": source_record_ids,
            "last_sequence": 7,
        }),
    )]);
    record
}

#[tokio::test]
async fn forget_session_purges_all_session_targets_through_wal() {
    let store = open_in_memory().await.expect("open store");
    let first = session_record(
        "01J00000000000000000001001",
        "01HQZX9F5N0000000000010001",
        "issue108 first session body",
        "sess-108",
    );
    let second = session_record(
        "01J00000000000000000001002",
        "01HQZX9F5N0000000000010002",
        "issue108 second session body",
        "sess-108",
    );
    let outside = session_record(
        "01J00000000000000000001003",
        "01HQZX9F5N0000000000010003",
        "issue108 outside body",
        "sess-outside",
    );

    store.upsert(&first).await.expect("upsert first");
    store.upsert(&second).await.expect("upsert second");
    store.upsert(&outside).await.expect("upsert outside");

    let outcome = store
        .forget_session("sess-108")
        .await
        .expect("forget session");

    assert_eq!(outcome.deleted_count, 2);
    assert_eq!(outcome.tombstones.len(), 2);

    let listed = store.list(&ListArgs::default()).await.expect("list");
    let listed_ids = listed
        .records
        .iter()
        .map(|record| record.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(listed_ids, vec![outside.id.as_str()]);

    let conn = store.raw_conn_for_admin().expect("raw conn").clone();
    let operation_id = outcome.operation_id.as_str().to_owned();
    let first_id = first.id.as_str().to_owned();
    conn.call(move |c| {
        let (kind, state): (String, String) = c.query_row(
            "SELECT kind, state FROM wal_ops WHERE operation_id = ?1",
            params![operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(kind, "forget_session");
        assert_eq!(state, "COMMITTED");

        let leaked_rows: i64 = c.query_row(
            "SELECT COUNT(*) FROM records WHERE json_extract(scope, '$.session_id') = 'sess-108'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(leaked_rows, 0);

        let fence_state: String = c.query_row(
            "SELECT state FROM reader_fence \
              WHERE operation_id = ?1 AND resource = 'session:default:default:sess-108'",
            params![operation_id],
            |row| row.get(0),
        )?;
        assert_eq!(fence_state, "CLEARED");

        let scope_json: String = c.query_row(
            "SELECT scope_json FROM wal_ops WHERE operation_id = ?1",
            params![operation_id],
            |row| row.get(0),
        )?;
        let receipt: serde_json::Value =
            serde_json::from_str(&scope_json).expect("scope_json is valid json");
        let audit_receipt = receipt
            .get("audit_receipt")
            .expect("forget_session WAL scope records an audit receipt");
        assert_eq!(audit_receipt["deleted_count"], 2);
        assert_eq!(audit_receipt["tombstone_count"], 2);
        assert_eq!(
            audit_receipt["target_hashes"]
                .as_array()
                .expect("target_hashes array")
                .len(),
            2
        );
        assert!(
            !scope_json.contains(first_id.as_str()),
            "receipt must not retain raw forgotten record ids"
        );
        assert!(
            !scope_json.contains("issue108 first session body"),
            "receipt must not retain forgotten content"
        );
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("wal assertion");
}

#[tokio::test]
async fn forget_session_expires_entity_edges_sourced_by_session_records() {
    let store = open_in_memory().await.expect("open store");
    let record = session_record(
        "01J00000000000000000001101",
        "01HQZX9F5N0000000000011001",
        "issue108 graph source",
        "sess-108-graph",
    );
    store.upsert(&record).await.expect("upsert source");

    let conn = store.raw_conn_for_admin().expect("raw conn").clone();
    let source_record_id = record.id.as_str().to_owned();
    conn.call(move |c| {
        c.execute_batch(
            "INSERT OR IGNORE INTO entity_nodes (id, name, name_norm, created_at) VALUES
               ('session-source', 'Session Source', 'session source', 1),
               ('session-target', 'Session Target', 'session target', 1);",
        )?;
        c.execute(
            "INSERT INTO entity_edges \
               (id, source_id, target_id, relation, confidence, confidence_score, \
                valid_at, invalid_at, created_at, expired_at, tombstone_reason, \
                source_record_id, body_hash) \
             VALUES ('session-edge', 'session-source', 'session-target', 'mentions', \
                     'EXTRACTED', 0.9, 100, NULL, 100, NULL, NULL, ?1, ?2)",
            params![source_record_id, vec![9_u8; 32]],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("seed entity edge");

    let outcome = store
        .forget_session("sess-108-graph")
        .await
        .expect("forget session");
    assert_eq!(outcome.deleted_count, 1);

    let conn = store.raw_conn_for_admin().expect("raw conn").clone();
    conn.call(move |c| {
        let (expired_at, tombstone_reason, retained_source): (
            Option<i64>,
            Option<String>,
            Option<String>,
        ) = c.query_row(
            "SELECT expired_at, tombstone_reason, source_record_id \
               FROM entity_edges WHERE id = 'session-edge'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert!(expired_at.is_some());
        assert_eq!(tombstone_reason.as_deref(), Some("forget"));
        assert_eq!(retained_source, None);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("entity edge assertion");
}

#[tokio::test]
async fn forget_session_purges_consolidation_summaries_that_reference_session_records() {
    let store = open_in_memory().await.expect("open store");
    let source = session_record(
        "01J00000000000000000001201",
        "01HQZX9F5N0000000000012001",
        "issue108 summarized source",
        "sess-108-summary",
    );
    let summary = consolidation_summary_record(
        "01J00000000000000000001202",
        "01HQZX9F5N0000000000012002",
        &[source.id.as_str()],
    );
    let unrelated_summary = consolidation_summary_record(
        "01J00000000000000000001203",
        "01HQZX9F5N0000000000012003",
        &["01J00000000000000000009999"],
    );

    store.upsert(&source).await.expect("upsert source");
    store.upsert(&summary).await.expect("upsert summary");
    store
        .upsert(&unrelated_summary)
        .await
        .expect("upsert unrelated summary");

    let outcome = store
        .forget_session("sess-108-summary")
        .await
        .expect("forget session");
    assert_eq!(outcome.deleted_count, 2);
    assert!(outcome.tombstones.contains(&source.id));
    assert!(outcome.tombstones.contains(&summary.id));

    let listed = store.list(&ListArgs::default()).await.expect("list");
    let listed_ids = listed
        .records
        .iter()
        .map(|record| record.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(listed_ids, vec![unrelated_summary.id.as_str()]);

    let conn = store.raw_conn_for_admin().expect("raw conn").clone();
    let summary_id = summary.id.as_str().to_owned();
    conn.call(move |c| {
        let leaked_summary: i64 = c.query_row(
            "SELECT COUNT(*) FROM records WHERE record_id = ?1",
            params![summary_id],
            |row| row.get(0),
        )?;
        assert_eq!(leaked_summary, 0);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("summary assertion");
}

#[tokio::test]
async fn forget_session_respects_active_session_writer_lock() {
    let store = open_in_memory().await.expect("open store");
    let record = session_record(
        "01J00000000000000000001301",
        "01HQZX9F5N0000000000013001",
        "issue108 active writer",
        "sess-108-lock",
    );
    store.upsert(&record).await.expect("upsert source");

    let conn = store.raw_conn_for_admin().expect("raw conn").clone();
    let incarnation = store.incarnation().expect("incarnation").clone();
    let shared_holder = cairn_store_sqlite::locks::acquire(
        &conn,
        &cairn_store_sqlite::locks::ResourceKey::session("default", "default", "sess-108-lock"),
        cairn_store_sqlite::locks::LockMode::Shared,
        "test-session-writer",
        Duration::from_secs(30),
        &incarnation,
        "test_session_writer",
    )
    .await
    .expect("acquire shared session lock");

    let err = store
        .forget_session("sess-108-lock")
        .await
        .expect_err("exclusive session forget should be fenced by active writer");
    assert!(
        matches!(err, StoreError::RecordWalLock(_)),
        "expected lock error, got {err:?}"
    );

    let listed = store.list(&ListArgs::default()).await.expect("list");
    assert_eq!(listed.records.len(), 1);
    assert_eq!(listed.records[0].id, record.id);

    shared_holder.release().await.expect("release shared lock");
    let outcome = store
        .forget_session("sess-108-lock")
        .await
        .expect("forget after release");
    assert_eq!(outcome.deleted_count, 1);
}
