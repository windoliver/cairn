//! Issue #58: record-level forget through the record WAL.

#![allow(missing_docs)]
#![allow(unused_imports)]

use std::sync::Arc;

use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::{MemoryRecord, RecordId, ScopeTuple};
use cairn_core::wal::WalKind;
use cairn_store_sqlite::{StoreError, open, open_in_memory};
use rusqlite::params;

fn sample_record() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

#[tokio::test]
async fn forget_payload_round_trips_body_free() {
    let store = open_in_memory().await.expect("open");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let record = sample_record();
    let payload = cairn_store_sqlite::record_wal::payload::ForgetPayload::new_for_test(
        record.id.clone(),
        record.target_id.clone(),
        record.scope.clone(),
        vec![record.id.clone()],
        "hash:00000000000000000000000000000000".to_owned(),
    );

    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-forget-payload', 1, ?1, 'ISSUED', '{}', 'issuer', ?2, ?3, 0, 'sig', 1, 1)",
            params![
                WalKind::ForgetRecord.as_str(),
                payload.target_hash,
                payload.scope.canonical_wire(),
            ],
        )?;
        cairn_store_sqlite::record_wal::payload::save_forget_payload_for_test(
            c,
            "op-forget-payload",
            &payload,
        )
        .expect("save forget payload");
        let loaded = cairn_store_sqlite::record_wal::payload::load_forget_payload_for_test(
            c,
            "op-forget-payload",
        )
        .expect("load forget payload");
        assert_eq!(loaded.requested_record_id, payload.requested_record_id);
        assert_eq!(loaded.target_id, payload.target_id);
        assert_eq!(loaded.record_ids, payload.record_ids);
        let json = serde_json::to_string(&loaded).expect("payload json");
        assert!(
            !json.contains(&record.body),
            "forget payload must not contain body text"
        );
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("payload round trip");
}
