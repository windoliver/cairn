//! Issue #58: record-level forget through the record WAL.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::{MemoryRecord, RecordId, ScopeTuple};
use cairn_core::wal::{OperationId, WalKind};
use cairn_store_sqlite::record_wal::RecordWalRegistry;
use cairn_store_sqlite::record_wal::payload::{
    PurgedPayload, RecordWalPayload, UpsertPayload, save_purged_payload_for_test,
};
use cairn_store_sqlite::wal::{RecoveryError, StepBodyRegistry};
use cairn_store_sqlite::{StoreError, open_in_memory};
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
        let raw_json: String = c.query_row(
            "SELECT payload_json FROM wal_payloads WHERE operation_id = ?1",
            params!["op-forget-payload"],
            |row| row.get(0),
        )?;
        assert!(
            !raw_json.contains(&record.body),
            "persisted forget payload must not contain body text"
        );
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

#[tokio::test]
async fn purged_payload_cannot_be_saved_directly() {
    let store = open_in_memory().await.expect("open");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));

    conn.call(|c| {
        let payload = PurgedPayload {
            target_hash: "hash:00000000000000000000000000000000".to_owned(),
            purged_by: "forget-record-test".to_owned(),
            purged_at: 1,
        };
        let err = save_purged_payload_for_test(c, "op-purged-payload", &payload)
            .expect_err("direct purged payload save must fail");
        match err {
            StoreError::Invariant { what } => assert!(
                what.contains("purged wal payloads are written by scrub updates"),
                "unexpected invariant: {what}"
            ),
            other => panic!("expected invariant, got {other:?}"),
        }
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("purged payload assertion");
}

#[tokio::test]
async fn recovery_rejects_purged_payload_for_body_bearing_kinds() {
    let store = open_in_memory().await.expect("open");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let record = sample_record();
    let upsert_payload = UpsertPayload::new_for_test(record.clone());
    let expire_payload_json = serde_json::json!({
        "type": "expire",
        "target_id": record.target_id.as_str(),
        "reason": "expire",
        "scope": record.scope,
    })
    .to_string();
    let purged_payload_json =
        serde_json::to_string(&RecordWalPayload::Purged(Box::new(PurgedPayload {
            target_hash: "hash:00000000000000000000000000000000".to_owned(),
            purged_by: "forget-record-test".to_owned(),
            purged_at: 1,
        })))
        .expect("purged payload json");

    conn.call({
        let purged_payload_json = purged_payload_json.clone();
        move |c| {
            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES ('op-upsert-purged-recovery', 1, 'upsert', 'ISSUED', '{}', 'issuer', ?1, '{}', 0, 'sig', 1, 1)",
                params![record.target_id.as_str()],
            )?;
            cairn_store_sqlite::record_wal::payload::save_upsert_payload_for_test(
                c,
                "op-upsert-purged-recovery",
                &upsert_payload,
            )
            .expect("save upsert payload");
            c.execute(
                "UPDATE wal_payloads SET kind = 'purged', payload_json = ?1 \
                 WHERE operation_id = 'op-upsert-purged-recovery'",
                params![purged_payload_json],
            )?;

            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES ('op-expire-purged-recovery', 2, 'expire', 'ISSUED', '{}', 'issuer', ?1, '{}', 0, 'sig', 1, 1)",
                params![record.target_id.as_str()],
            )?;
            c.execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES ('op-expire-purged-recovery', 'expire', ?1, 1)",
                params![expire_payload_json],
            )?;
            c.execute(
                "UPDATE wal_payloads SET kind = 'purged', payload_json = ?1 \
                 WHERE operation_id = 'op-expire-purged-recovery'",
                params![purged_payload_json],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        }
    })
    .await
    .expect("seed purged payloads");

    let registry = RecordWalRegistry::new(Arc::from("forget-record-test"));
    for (op, kind, kind_name) in [
        (
            "op-upsert-purged-recovery",
            WalKind::Upsert,
            WalKind::Upsert.as_str(),
        ),
        (
            "op-expire-purged-recovery",
            WalKind::Expire,
            WalKind::Expire.as_str(),
        ),
    ] {
        let op_id = OperationId::parse(op.to_owned()).expect("op id");
        let Err(err) = registry.body_for(&conn, kind, &op_id).await else {
            panic!("expected purged payload mismatch for {kind_name}");
        };
        match err {
            RecoveryError::Invariant(message) => {
                assert!(
                    message.contains("payload variant purged does not match wal kind"),
                    "unexpected invariant: {message}"
                );
                assert!(
                    message.contains(kind_name),
                    "invariant should name wal kind: {message}"
                );
            }
            other => panic!("expected invariant, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn forget_resolution_loads_full_target_lineage() {
    let store = open_in_memory().await.expect("open");
    let first = sample_record();
    let mut second = first.clone();
    second.id = RecordId::parse("01J00000000000000000000002").expect("record id");
    second.body = "replacement body for same target".to_owned();
    let first_out = store.upsert(&first).await.expect("first upsert");
    let second_out = store.upsert(&second).await.expect("second upsert");

    let target =
        cairn_store_sqlite::record_wal::forget::resolve_forget_target_for_test(&store, &first.id)
            .await
            .expect("resolve");

    assert_eq!(target.requested_record_id, first.id);
    assert_eq!(target.target_id, first.target_id);
    assert_eq!(target.record_ids.len(), 2);
    assert!(target.record_ids.contains(&first_out.record_id));
    assert!(target.record_ids.contains(&second_out.record_id));
    assert_eq!(target.scope, first.scope);
    assert!(target.target_hash.starts_with("hash:"));
}

#[tokio::test]
async fn forget_resolution_reports_not_found_for_missing_record() {
    let store = open_in_memory().await.expect("open");
    let missing = RecordId::parse("01J00000000000000000000999").expect("record id");
    let err =
        cairn_store_sqlite::record_wal::forget::resolve_forget_target_for_test(&store, &missing)
            .await
            .expect_err("missing record rejects");
    assert!(matches!(err, StoreError::NotFound { id } if id == missing.as_str()));
}

#[tokio::test]
async fn forget_resolution_reports_codec_for_malformed_scope() {
    let store = open_in_memory().await.expect("open");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let record = sample_record();
    store.upsert(&record).await.expect("upsert");

    conn.call({
        let record_id = record.id.clone();
        move |c| {
            c.execute(
                "UPDATE records SET scope = ?1 WHERE record_id = ?2",
                params!["not-json", record_id.as_str()],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        }
    })
    .await
    .expect("corrupt scope");

    let err =
        cairn_store_sqlite::record_wal::forget::resolve_forget_target_for_test(&store, &record.id)
            .await
            .expect_err("malformed scope rejects");
    assert!(matches!(err, StoreError::Codec(_)));
}

#[tokio::test]
async fn forget_record_purges_primary_indexes_and_vectors() {
    let store = open_in_memory().await.expect("open");
    let record = sample_record();
    let body = record.body.clone();
    store.upsert(&record).await.expect("upsert");

    let outcome = store
        .forget_record(&record.id)
        .await
        .expect("forget record");
    assert_eq!(outcome.deleted_count, 1);
    assert_eq!(outcome.tombstones, vec![record.id.clone()]);

    let listed = store.list(&ListArgs::default()).await.expect("list");
    assert!(listed.records.is_empty(), "forgotten record must be invisible");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let record_id = record.id.as_str().to_owned();
    let target_id = record.target_id.as_str().to_owned();
    conn.call(move |c| {
        let records: i64 = c.query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1",
            params![target_id],
            |row| row.get(0),
        )?;
        let vectors: i64 = c.query_row(
            "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?1",
            params![record_id],
            |row| row.get(0),
        )?;
        let pending: i64 = c.query_row(
            "SELECT COUNT(*) FROM pending_embeddings WHERE record_id = ?1",
            params![record_id],
            |row| row.get(0),
        )?;
        let fts_rows: i64 = c.query_row(
            "SELECT COUNT(*) FROM records_fts WHERE body MATCH ?1",
            params![body],
            |row| row.get(0),
        )?;
        assert_eq!(records, 0);
        assert_eq!(vectors, 0);
        assert_eq!(pending, 0);
        assert_eq!(fts_rows, 0);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("physical purge assertions");
}

#[tokio::test]
async fn forget_record_keeps_session_siblings_visible() {
    let store = open_in_memory().await.expect("open");
    let first = sample_record();
    let mut sibling = sample_record();
    sibling.id = RecordId::parse("01J00000000000000000000003").expect("record id");
    sibling.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000003").expect("target");
    sibling.body = "sibling body remains".to_owned();
    sibling.scope = ScopeTuple {
        session_id: first.scope.session_id.clone(),
        user: first.scope.user.clone(),
        agent: first.scope.agent.clone(),
        ..ScopeTuple::default()
    };

    store.upsert(&first).await.expect("first upsert");
    store.upsert(&sibling).await.expect("sibling upsert");
    store
        .forget_record(&first.id)
        .await
        .expect("forget first");

    let listed = store.list(&ListArgs::default()).await.expect("list");
    assert_eq!(listed.records.len(), 1);
    assert_eq!(listed.records[0].id, sibling.id);
}
