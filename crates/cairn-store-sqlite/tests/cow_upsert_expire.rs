//! Issue #57: COW upsert and expire through body-bearing WAL.

#![allow(missing_docs, unused_imports)]

use std::sync::Arc;

use cairn_core::contract::memory_store::{
    KeywordSearchArgs, ListArgs, MemoryStore, TombstoneReason,
};
use cairn_core::domain::{MemoryRecord, TargetId};
use cairn_store_sqlite::open_in_memory;
use rusqlite::params;

fn sample() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

#[tokio::test]
async fn payload_round_trip_smoke() {
    let store = open_in_memory().await.expect("open");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let record = sample();
    let payload = cairn_store_sqlite::record_wal::payload::UpsertPayload::new_for_test(record);

    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-round-trip', 1, 'upsert', 'ISSUED', '{}', 'issuer', 'target', '{}', 0, 'sig', 1, 1)",
            [],
        )?;
        cairn_store_sqlite::record_wal::payload::save_upsert_payload_for_test(
            c,
            "op-round-trip",
            &payload,
        )
        .expect("save upsert payload");
        let loaded = cairn_store_sqlite::record_wal::payload::load_upsert_payload_for_test(
            c,
            "op-round-trip",
        )
        .expect("load upsert payload");
        assert_eq!(loaded.record.target_id, payload.record.target_id);
        assert_eq!(loaded.record.body, payload.record.body);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("payload round trip");
}

#[tokio::test]
async fn upsert_commits_wal_operation_and_all_steps() {
    let store = open_in_memory().await.expect("open");
    let record = sample();

    let out = store.upsert(&record).await.expect("upsert through wal");
    assert_eq!(out.version, 1);
    assert!(out.content_changed);

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(|c| {
        let row: (String, i64) = c.query_row(
            "SELECT state, COUNT(*) FROM wal_ops WHERE kind = 'upsert' GROUP BY state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(row, ("COMMITTED".to_owned(), 1));

        let done_count: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_steps ws \
              JOIN wal_ops wo ON wo.operation_id = ws.operation_id \
             WHERE wo.kind = 'upsert' AND ws.state = 'DONE'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(done_count, 6);

        let payload_count: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_payloads wp \
              JOIN wal_ops wo ON wo.operation_id = wp.operation_id \
             WHERE wo.kind = 'upsert'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(payload_count, 1);

        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("wal rows");
}
