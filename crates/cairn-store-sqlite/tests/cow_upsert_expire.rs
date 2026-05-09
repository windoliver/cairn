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
