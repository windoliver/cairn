//! Issue #58: record-level forget through the record WAL.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::{MemoryRecord, RecordId, ScopeTuple};
use cairn_core::wal::{OperationId, WalKind};
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};
use cairn_store_sqlite::record_wal::RecordWalRegistry;
use cairn_store_sqlite::record_wal::payload::{
    PurgedPayload, RecordWalPayload, UpsertPayload, save_purged_payload_for_test,
};
use cairn_store_sqlite::wal::{RecoveryError, StepBodyRegistry};
use cairn_store_sqlite::{StoreError, open, open_in_memory, open_in_memory_with_embedder};
use rusqlite::params;

fn sample_record() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

async fn seed_pending_and_assert_vector_surfaces(
    conn: &Arc<tokio_rusqlite::Connection>,
    record_id: &RecordId,
) {
    let record_id = record_id.as_str().to_owned();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO pending_embeddings \
               (record_id, reason, attempt_count, last_error, enqueued_at) \
             VALUES (?1, 'opt_in_backfill', 0, NULL, 1)",
            params![record_id],
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
        assert!(vectors > 0, "upsert should write a vector before forget");
        assert!(pending > 0, "test should seed pending row before forget");
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("pre-forget vector assertions");
}

async fn assert_forget_purged_surfaces(
    conn: &Arc<tokio_rusqlite::Connection>,
    record: &MemoryRecord,
    operation_id: &OperationId,
) {
    let record_id = record.id.as_str().to_owned();
    let target_id = record.target_id.as_str().to_owned();
    let body = record.body.clone();
    let operation_id = operation_id.as_str().to_owned();
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
        let op_state: String = c.query_row(
            "SELECT state FROM wal_ops WHERE operation_id = ?1",
            params![operation_id],
            |row| row.get(0),
        )?;
        let done_steps: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_steps \
              WHERE operation_id = ?1 AND state = 'DONE'",
            params![operation_id],
            |row| row.get(0),
        )?;
        let total_steps: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_steps WHERE operation_id = ?1",
            params![operation_id],
            |row| row.get(0),
        )?;
        let consent_payload: String = c.query_row(
            "SELECT payload_json FROM consent_journal \
              WHERE op_id = ?1 AND kind = 'forget_intent'",
            params![operation_id],
            |row| row.get(0),
        )?;
        assert_eq!(records, 0);
        assert_eq!(vectors, 0);
        assert_eq!(pending, 0);
        assert_eq!(fts_rows, 0);
        assert_eq!(op_state, "COMMITTED");
        assert_eq!(done_steps, 7);
        assert_eq!(total_steps, 7);
        assert!(
            !consent_payload.contains(&record_id),
            "forget consent event must not retain record id"
        );
        assert!(
            !consent_payload.contains(&target_id),
            "forget consent event must not retain target id"
        );
        assert!(
            !consent_payload.contains(&body),
            "forget consent event must not retain body text"
        );
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("physical purge assertions");
}

async fn seed_upsert_pre_image_leak(
    conn: &Arc<tokio_rusqlite::Connection>,
    record: &MemoryRecord,
) -> (String, i64) {
    let record_id = record.id.as_str().to_owned();
    let target_id = record.target_id.as_str().to_owned();
    let body = record.body.clone();
    conn.call(move |c| {
        let upsert_op = c.query_row(
            "SELECT p.operation_id \
               FROM wal_payloads p \
               JOIN wal_ops o ON o.operation_id = p.operation_id \
              WHERE p.kind = 'upsert' AND o.target_hash = ?1",
            params![target_id],
            |row| row.get::<_, String>(0),
        )?;
        let leaked_pre_image = serde_json::json!({
            "type": "test_leak",
            "record_id": record_id,
            "body": body,
        })
        .to_string()
        .into_bytes();
        let step_ord = c.query_row(
            "SELECT MIN(step_ord) FROM wal_steps WHERE operation_id = ?1",
            params![upsert_op],
            |row| row.get::<_, i64>(0),
        )?;
        let changed = c.execute(
            "UPDATE wal_steps \
                SET pre_image = ?1 \
              WHERE operation_id = ?2 \
                AND step_ord = ?3",
            params![leaked_pre_image, upsert_op, step_ord],
        )?;
        assert_eq!(changed, 1, "test should seed one upsert pre_image leak");
        Ok::<_, tokio_rusqlite::Error>((upsert_op, step_ord))
    })
    .await
    .expect("seed leaking pre_image")
}

async fn assert_forget_scrubbed_prior_wal_and_kept_body_free_payload(
    conn: &Arc<tokio_rusqlite::Connection>,
    record: &MemoryRecord,
    upsert_op: String,
    seeded_step_ord: i64,
    forget_op: &OperationId,
) {
    let record_id = record.id.as_str().to_owned();
    let body = record.body.clone();
    let forget_op = forget_op.as_str().to_owned();
    conn.call(move |c| {
        let retained_fragments = {
            let mut stmt = c.prepare(
                "SELECT 'payload', payload_json \
                   FROM wal_payloads \
                  WHERE operation_id = ?1 \
                 UNION ALL \
                 SELECT 'pre_image', CAST(pre_image AS TEXT) \
                   FROM wal_steps \
                  WHERE operation_id = ?1 AND pre_image IS NOT NULL",
            )?;
            stmt.query_map(params![upsert_op], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        assert!(
            !retained_fragments.is_empty(),
            "prior body-bearing WAL retention should remain as scrub stubs"
        );
        for (surface, retained) in &retained_fragments {
            assert!(
                !retained.contains(&body),
                "scrubbed prior {surface} must not retain body text: {retained}"
            );
            assert!(
                !retained.contains(&record_id),
                "scrubbed prior {surface} must not retain raw record id: {retained}"
            );
        }

        let (payload_type, payload_purged_by): (String, String) = c.query_row(
            "SELECT json_extract(payload_json, '$.type'), \
                    json_extract(payload_json, '$.purged_by') \
               FROM wal_payloads \
              WHERE operation_id = ?1 \
                AND kind = 'purged' \
                AND json_extract(payload_json, '$.type') = 'purged'",
            params![upsert_op],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(payload_type, "purged");
        assert_eq!(payload_purged_by, forget_op);

        let (pre_image_type, pre_image_purged_by): (String, String) = c.query_row(
            "SELECT json_extract(CAST(pre_image AS TEXT), '$.type'), \
                    json_extract(CAST(pre_image AS TEXT), '$.purged_by') \
               FROM wal_steps \
              WHERE operation_id = ?1 \
                AND step_ord = ?2 \
                AND pre_image IS NOT NULL",
            params![upsert_op, seeded_step_ord],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(pre_image_type, "purged");
        assert_eq!(pre_image_purged_by, forget_op);

        let (forget_kind, forget_type, forget_json): (String, String, String) = c.query_row(
            "SELECT kind, json_extract(payload_json, '$.type'), payload_json \
               FROM wal_payloads \
              WHERE operation_id = ?1",
            params![forget_op],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(forget_kind, "forget_record");
        assert_eq!(forget_type, "forget_record");
        assert!(
            !forget_json.contains(&body),
            "current forget payload must remain body-free"
        );
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("scrub assertions");
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
    let embedder: Arc<dyn EmbeddingModel> =
        Arc::new(MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open");
    let record = sample_record();
    store.upsert(&record).await.expect("upsert");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    seed_pending_and_assert_vector_surfaces(&conn, &record.id).await;

    let outcome = store
        .forget_record(&record.id)
        .await
        .expect("forget record");
    assert_eq!(outcome.deleted_count, 1);
    assert_eq!(outcome.tombstones, vec![record.id.clone()]);

    let listed = store.list(&ListArgs::default()).await.expect("list");
    assert!(
        listed.records.is_empty(),
        "forgotten record must be invisible"
    );

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    assert_forget_purged_surfaces(&conn, &record, &outcome.operation_id).await;
}

#[tokio::test]
async fn forget_record_scrubs_body_bearing_wal_payloads_and_pre_images() {
    let store = open_in_memory().await.expect("open");
    let record = sample_record();
    store.upsert(&record).await.expect("upsert");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let (upsert_op, seeded_step_ord) = seed_upsert_pre_image_leak(&conn, &record).await;

    let outcome = store
        .forget_record(&record.id)
        .await
        .expect("forget record");
    assert_eq!(outcome.deleted_count, 1);

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    assert_forget_scrubbed_prior_wal_and_kept_body_free_payload(
        &conn,
        &record,
        upsert_op,
        seeded_step_ord,
        &outcome.operation_id,
    )
    .await;
}

#[tokio::test]
async fn forget_record_replay_is_idempotent_after_primary_purge() {
    let store = open_in_memory().await.expect("open");
    let record = sample_record();
    store.upsert(&record).await.expect("upsert");

    let outcome = store
        .forget_record(&record.id)
        .await
        .expect("first forget");
    assert_eq!(outcome.deleted_count, 1);

    let err = store
        .forget_record(&record.id)
        .await
        .expect_err("second forget should see purged primary row");
    assert!(matches!(err, StoreError::NotFound { id } if id == record.id.as_str()));
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
    store.forget_record(&first.id).await.expect("forget first");

    let listed = store.list(&ListArgs::default()).await.expect("list");
    assert_eq!(listed.records.len(), 1);
    assert_eq!(listed.records[0].id, sibling.id);
}

#[tokio::test]
async fn forget_record_does_not_scrub_unrelated_payload_mentions() {
    let store = open_in_memory().await.expect("open");
    let forgotten = sample_record();
    let mut unrelated = sample_record();
    unrelated.id = RecordId::parse("01J00000000000000000000004").expect("record id");
    unrelated.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000004").expect("target");
    unrelated.body = format!(
        "unrelated note mentions {} and {} only as text",
        forgotten.id.as_str(),
        forgotten.target_id.as_str()
    );

    store.upsert(&forgotten).await.expect("forgotten upsert");
    store.upsert(&unrelated).await.expect("unrelated upsert");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let unrelated_target = unrelated.target_id.as_str().to_owned();
    let forgotten_target = forgotten.target_id.as_str().to_owned();
    let (unrelated_op, forgotten_op, forgotten_pre_images) = conn
        .call(move |c| {
            let unrelated_op = c.query_row(
                "SELECT p.operation_id \
                   FROM wal_payloads p \
                   JOIN wal_ops o ON o.operation_id = p.operation_id \
                  WHERE p.kind = 'upsert' AND o.target_hash = ?1",
                params![unrelated_target],
                |row| row.get::<_, String>(0),
            )?;
            let forgotten_op = c.query_row(
                "SELECT p.operation_id \
                   FROM wal_payloads p \
                   JOIN wal_ops o ON o.operation_id = p.operation_id \
                  WHERE p.kind = 'upsert' AND o.target_hash = ?1",
                params![forgotten_target],
                |row| row.get::<_, String>(0),
            )?;
            let forgotten_pre_images = c.query_row(
                "SELECT COUNT(*) FROM wal_steps \
                  WHERE operation_id = ?1 AND pre_image IS NOT NULL",
                params![forgotten_op],
                |row| row.get::<_, i64>(0),
            )?;
            Ok::<_, tokio_rusqlite::Error>((unrelated_op, forgotten_op, forgotten_pre_images))
        })
        .await
        .expect("upsert ops");

    let outcome = store
        .forget_record(&forgotten.id)
        .await
        .expect("forget original");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(move |c| {
        let kind: String = c.query_row(
            "SELECT kind FROM wal_payloads WHERE operation_id = ?1",
            params![unrelated_op],
            |row| row.get(0),
        )?;
        assert_eq!(kind, "upsert");
        let (forgotten_kind, forgotten_type, purged_by): (String, String, String) = c.query_row(
            "SELECT kind, \
                    json_extract(payload_json, '$.type'), \
                    json_extract(payload_json, '$.purged_by') \
               FROM wal_payloads WHERE operation_id = ?1",
            params![forgotten_op],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(forgotten_kind, "purged");
        assert_eq!(forgotten_type, "purged");
        assert_eq!(purged_by, outcome.operation_id.as_str());

        let current_kind: String = c.query_row(
            "SELECT kind FROM wal_payloads WHERE operation_id = ?1",
            params![outcome.operation_id.as_str()],
            |row| row.get(0),
        )?;
        assert_eq!(current_kind, "forget_record");

        if forgotten_pre_images > 0 {
            let purged_pre_images: i64 = c.query_row(
                "SELECT COUNT(*) FROM wal_steps \
                  WHERE operation_id = ?1 \
                    AND pre_image IS NOT NULL \
                    AND json_extract(CAST(pre_image AS TEXT), '$.type') = 'purged' \
                    AND json_extract(CAST(pre_image AS TEXT), '$.purged_by') = ?2",
                params![forgotten_op, outcome.operation_id.as_str()],
                |row| row.get(0),
            )?;
            assert_eq!(purged_pre_images, forgotten_pre_images);
        }
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("unrelated payload remains");
}

#[tokio::test]
async fn prepared_forget_recovers_from_persisted_payload() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cairn.sqlite");
    let op_id = "forget_record-01J00000000000000000001000";
    let record = sample_record();
    let target_hash = "hash:00000000000000000000000000000000".to_owned();

    {
        let store = open(&path).await.expect("open #1");
        store.upsert(&record).await.expect("seed record");
        let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
        let payload_json = serde_json::json!({
            "type": "forget_record",
            "requested_record_id": record.id.as_str(),
            "target_id": record.target_id.as_str(),
            "scope": record.scope.clone(),
            "record_ids": [record.id.as_str()],
            "target_hash": target_hash.clone(),
            "reason_code": "user_command"
        })
        .to_string();
        conn.call(move |c| {
            c.execute("DELETE FROM lock_holders", [])?;
            c.execute("DELETE FROM locks", [])?;
            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES (?1, 100, 'forget_record', 'ISSUED', '{}', 'issuer', ?2, ?3, 0, 'sig', 1, 1)",
                params![op_id, target_hash, record.scope.canonical_wire()],
            )?;
            c.execute(
                "UPDATE wal_ops SET state = 'PREPARED', updated_at = 2 \
                 WHERE operation_id = ?1",
                params![op_id],
            )?;
            c.execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES (?1, 'forget_record', ?2, 1)",
                params![op_id, payload_json],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed prepared forget");
    }

    let store = open(&path).await.expect("open #2 triggers recovery");
    assert!(
        store
            .list(&ListArgs::default())
            .await
            .expect("list")
            .records
            .is_empty()
    );
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(move |c| {
        let state: String = c.query_row(
            "SELECT state FROM wal_ops WHERE operation_id = ?1",
            params![op_id],
            |row| row.get(0),
        )?;
        let done: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_steps WHERE operation_id = ?1 AND state = 'DONE'",
            params![op_id],
            |row| row.get(0),
        )?;
        assert_eq!(state, "COMMITTED");
        assert_eq!(done, 7);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("recovery assertions");
}

#[tokio::test]
async fn prepared_forget_recovers_after_tombstone_linearization() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cairn.sqlite");
    let op_id = "forget_record-01J00000000000000000001002";
    let record = sample_record();
    let target_hash = "hash:22222222222222222222222222222222".to_owned();

    {
        let store = open(&path).await.expect("open #1");
        store.upsert(&record).await.expect("seed record");
        let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
        let payload_json = serde_json::json!({
            "type": "forget_record",
            "requested_record_id": record.id.as_str(),
            "target_id": record.target_id.as_str(),
            "scope": record.scope.clone(),
            "record_ids": [record.id.as_str()],
            "target_hash": target_hash.clone(),
            "reason_code": "user_command"
        })
        .to_string();
        conn.call(move |c| {
            c.execute("DELETE FROM lock_holders", [])?;
            c.execute("DELETE FROM locks", [])?;
            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES (?1, 101, 'forget_record', 'PREPARED', '{}', 'issuer', ?2, ?3, 0, 'sig', 1, 2)",
                params![op_id, target_hash, record.scope.canonical_wire()],
            )?;
            c.execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES (?1, 'forget_record', ?2, 1)",
                params![op_id, payload_json],
            )?;
            c.execute(
                "UPDATE records \
                    SET active = 0, tombstoned = 1, tombstone_reason = 'forget' \
                  WHERE target_id = ?1",
                params![record.target_id.as_str()],
            )?;
            c.execute(
                "INSERT INTO wal_steps \
                   (operation_id, step_ord, step_kind, state, attempts, started_at, finished_at) \
                 VALUES (?1, 0, 'primary.mark_tombstone', 'DONE', 1, 1, 2)",
                params![op_id],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed linearized forget");
    }

    let store = open(&path).await.expect("open #2 triggers recovery");
    assert!(
        store
            .list(&ListArgs::default())
            .await
            .expect("list")
            .records
            .is_empty(),
        "linearized forget must stay invisible during recovery"
    );
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(move |c| {
        let state: String = c.query_row(
            "SELECT state FROM wal_ops WHERE operation_id = ?1",
            params![op_id],
            |row| row.get(0),
        )?;
        let remaining_records: i64 =
            c.query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))?;
        assert_eq!(state, "COMMITTED");
        assert_eq!(remaining_records, 0);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("linearized recovery assertions");
}

#[tokio::test]
async fn purge_pending_lint_reports_exhausted_forget_phase_b_without_raw_ids() {
    let store = open_in_memory().await.expect("open");
    let record = sample_record();
    store.upsert(&record).await.expect("upsert");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let raw_record_id = record.id.as_str().to_owned();
    let raw_target_id = record.target_id.as_str().to_owned();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('forget_record-01J00000000000000000001001', 200, 'forget_record', \
                     'PREPARED', '{}', 'issuer', \
                     'hash:11111111111111111111111111111111', 'user=hmn:tafeng', 0, 'sig', 1, 1)",
            [],
        )?;
        c.execute(
            "INSERT INTO wal_steps \
               (operation_id, step_ord, step_kind, state, attempts, last_error, started_at, finished_at) \
             VALUES ('forget_record-01J00000000000000000001001', 5, 'wal.purge_pre_images', \
                     'FAILED', 3, 'boom', 1, 2)",
            [],
        )?;
        let findings =
            cairn_store_sqlite::lint_purge_pending(c).expect("lint purge pending");
        assert_eq!(findings.len(), 1);
        let rendered = serde_json::to_string(&findings).expect("finding json");
        assert!(rendered.contains("purge_pending"));
        assert!(rendered.contains("hash:11111111111111111111111111111111"));
        assert!(!rendered.contains(&raw_record_id));
        assert!(!rendered.contains(&raw_target_id));
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("lint assertions");
}
