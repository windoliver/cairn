//! Issue #57: COW upsert and expire through body-bearing WAL.

#![allow(missing_docs, unused_imports)]

use std::sync::Arc;

use cairn_core::contract::memory_store::{
    KeywordSearchArgs, ListArgs, MemoryStore, TombstoneReason,
};
use cairn_core::domain::{MemoryRecord, ScopeTuple, TargetId};
use cairn_core::wal::WalKind;
use cairn_store_sqlite::{open, open_in_memory};
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

#[tokio::test]
async fn prepared_expire_recovers_with_persisted_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cairn.sqlite");
    let op_id = "op-prepared-expire-scope-recovery";
    let mut record = sample();
    record.scope = ScopeTuple {
        tenant: Some("tenant-review".to_owned()),
        workspace: Some("workspace-review".to_owned()),
        session_id: Some("session-review".to_owned()),
        user: Some("hmn:tafeng".to_owned()),
        ..ScopeTuple::default()
    };
    let target = record.target_id.clone();

    {
        let store = open(&path).await.expect("open #1");
        store.upsert(&record).await.expect("seed record");
        let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
        let payload_json = serde_json::json!({
            "type": "expire",
            "target_id": target.as_str(),
            "reason": "expire",
            "scope": record.scope,
        })
        .to_string();
        let target_for_seed = target.clone();
        let entity_resource = format!(
            "entity:tenant-review:workspace-review:{}",
            target_for_seed.as_str()
        );
        let session_resource = "session:tenant-review:workspace-review:session-review".to_owned();

        conn.call(move |c| {
            c.execute(
                "DELETE FROM lock_holders WHERE resource IN (?1, ?2)",
                params![entity_resource, session_resource],
            )?;
            c.execute(
                "DELETE FROM locks WHERE resource IN (?1, ?2)",
                params![entity_resource, session_resource],
            )?;
            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES (?1, 100, 'expire', 'ISSUED', '{}', 'issuer', ?2, '{}', 0, 'sig', 1, 1)",
                params![op_id, target_for_seed.as_str()],
            )?;
            c.execute(
                "UPDATE wal_ops SET state = 'PREPARED', updated_at = 2 \
                 WHERE operation_id = ?1",
                params![op_id],
            )?;
            c.execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES (?1, 'expire', ?2, 1)",
                params![op_id, payload_json],
            )?;
            c.execute(
                "UPDATE records SET scope = '{}' WHERE target_id = ?1",
                params![target_for_seed.as_str()],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed prepared expire");
    }

    let store = open(&path).await.expect("open #2");
    assert!(
        store
            .list(&ListArgs::default())
            .await
            .expect("list")
            .records
            .is_empty()
    );

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let target_id = target.as_str().to_owned();
    conn.call(move |c| {
        let payload_json: String = c.query_row(
            "SELECT payload_json FROM wal_payloads WHERE operation_id = ?1",
            params![op_id],
            |row| row.get(0),
        )?;
        let payload: serde_json::Value = serde_json::from_str(&payload_json)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        assert_eq!(
            payload
                .get("scope")
                .and_then(|scope| scope.get("tenant"))
                .and_then(serde_json::Value::as_str),
            Some("tenant-review")
        );

        let entity_resource = format!("entity:tenant-review:workspace-review:{target_id}");
        let session_resource = "session:tenant-review:workspace-review:session-review";
        let non_default_lock_rows: i64 = c.query_row(
            "SELECT COUNT(*) FROM locks WHERE resource IN (?1, ?2)",
            params![entity_resource, session_resource],
            |row| row.get(0),
        )?;
        assert_eq!(
            non_default_lock_rows, 2,
            "recovery must reacquire locks from durable expire scope"
        );

        let state: String = c.query_row(
            "SELECT state FROM wal_ops WHERE operation_id = ?1",
            params![op_id],
            |row| row.get(0),
        )?;
        assert_eq!(state, "COMMITTED");
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("expire recovery assertions");
}

#[tokio::test]
async fn prepared_upsert_recovers_from_persisted_payload() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cairn.sqlite");
    let op_id = "op-prepared-upsert-recovery";
    let record = sample();
    let record_id = record.id.clone();

    {
        let store = open(&path).await.expect("open #1");
        let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
        let payload =
            cairn_store_sqlite::record_wal::payload::UpsertPayload::new_for_test(record.clone());
        let kind = WalKind::Upsert.as_str().to_owned();

        conn.call(move |c| {
            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES (?1, 1, ?2, 'ISSUED', '{}', 'issuer', ?3, '{}', 0, 'sig', 1, 1)",
                params![op_id, kind, record.target_id.as_str()],
            )?;
            c.execute(
                "UPDATE wal_ops SET state = 'PREPARED', updated_at = 2 \
                 WHERE operation_id = ?1",
                params![op_id],
            )?;
            cairn_store_sqlite::record_wal::payload::save_upsert_payload_for_test(
                c, op_id, &payload,
            )
            .expect("save upsert payload");
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed prepared upsert");
    }

    let store = open(&path).await.expect("open #2");
    let recovered = store
        .get(&record_id)
        .await
        .expect("get recovered")
        .expect("record recovered");
    assert_eq!(recovered.body, record.body);

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let op = op_id.to_owned();
    conn.call(move |c| {
        let state: String = c.query_row(
            "SELECT state FROM wal_ops WHERE operation_id = ?1",
            params![op],
            |row| row.get(0),
        )?;
        assert_eq!(state, "COMMITTED");

        let done_steps: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_steps \
             WHERE operation_id = ?1 AND state = 'DONE'",
            params![op],
            |row| row.get(0),
        )?;
        assert_eq!(done_steps, 6);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("recovery wal state");
}

#[tokio::test]
async fn body_changing_upsert_uses_same_record_id_in_outcome_row_and_payload() {
    let store = open_in_memory().await.expect("open");
    let mut record = sample();
    store.upsert(&record).await.expect("initial upsert");

    record.body = "changed body for deterministic wal plan".to_owned();
    let out = store.upsert(&record).await.expect("body-changing upsert");
    assert_eq!(out.version, 2);
    assert!(out.content_changed);

    let target = record.target_id.as_str().to_owned();
    let returned_id = out.record_id.as_str().to_owned();
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(move |c| {
        let active_record_id: String = c.query_row(
            "SELECT record_id FROM records WHERE target_id = ?1 AND active = 1",
            params![target],
            |row| row.get(0),
        )?;

        let payload_json: String = c.query_row(
            "SELECT wp.payload_json \
               FROM wal_payloads wp \
               JOIN wal_ops wo ON wo.operation_id = wp.operation_id \
              WHERE wo.kind = 'upsert' \
              ORDER BY wo.issued_seq DESC \
              LIMIT 1",
            [],
            |row| row.get(0),
        )?;
        let payload: serde_json::Value = serde_json::from_str(&payload_json)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        let planned_record_id = payload
            .get("planned")
            .and_then(|planned| planned.get("outcome_record_id"))
            .and_then(serde_json::Value::as_str)
            .expect("planned outcome_record_id")
            .to_owned();

        assert_eq!(active_record_id, returned_id);
        assert_eq!(planned_record_id, returned_id);

        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("id assertions");
}

#[tokio::test]
async fn upsert_stages_inactive_row_before_activation() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("initial upsert");

    let mut r2 = r.clone();
    r2.body = "changed body for cow staging".to_owned();

    let out = store.upsert(&r2).await.expect("second upsert");
    assert_eq!(out.version, 2);

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let rows: Vec<(String, i64, i64)> = conn
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT record_id, active, tombstoned FROM records \
                 WHERE target_id = ?1 ORDER BY version",
            )?;
            let rows = stmt
                .query_map(params![r.target_id.as_str()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, tokio_rusqlite::Error>(rows)
        })
        .await
        .expect("records");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, 0, "prior version inactive after activation");
    assert_eq!(rows[1].1, 1, "new version active after activation");
    assert_eq!(rows[0].2, 0, "superseded row is not tombstoned");
    assert_eq!(rows[1].2, 0, "new row is visible");
}

#[tokio::test]
async fn upsert_snapshot_stage_records_pre_image_blob() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("upsert");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(|c| {
        let count: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_steps ws \
              JOIN wal_ops wo ON wo.operation_id = ws.operation_id \
             WHERE wo.kind = 'upsert' \
               AND ws.step_kind = 'snapshot.stage' \
               AND ws.pre_image IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("snapshot stage");
}

#[tokio::test]
async fn upsert_rejects_fresh_target_that_reuses_existing_record_id() {
    let store = open_in_memory().await.expect("open");
    let existing = sample();
    store.upsert(&existing).await.expect("initial upsert");

    let mut colliding = existing.clone();
    colliding.target_id = TargetId::parse("01HQZX9F5N0000000000000001").expect("valid target id");
    colliding.body = "different target but same record id".to_owned();

    store
        .upsert(&colliding)
        .await
        .expect_err("record_id collision across targets must fail");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let new_target = colliding.target_id.as_str().to_owned();
    let old_target = existing.target_id.as_str().to_owned();
    conn.call(move |c| {
        let new_active: i64 = c.query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND active = 1",
            params![new_target],
            |row| row.get(0),
        )?;
        assert_eq!(
            new_active, 0,
            "failed collision must not create an active row for the new target"
        );

        let old_active: i64 = c.query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND active = 1",
            params![old_target],
            |row| row.get(0),
        )?;
        assert_eq!(old_active, 1, "original target remains active");
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("records");
}

#[tokio::test]
async fn repeated_upsert_steps_do_not_duplicate_derived_rows() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("upsert");
    store.upsert(&r).await.expect("idempotent upsert");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(move |c| {
        let fts_rows: i64 =
            c.query_row("SELECT COUNT(*) FROM records_fts", [], |row| row.get(0))?;
        let active_records: i64 = c.query_row(
            "SELECT COUNT(*) FROM records WHERE active = 1 AND tombstoned = 0",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(fts_rows, active_records);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("derived counts");
}

#[tokio::test]
async fn expire_retires_target_without_hard_delete() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    let out = store.upsert(&r).await.expect("upsert");

    store.expire(&r.target_id).await.expect("expire");

    assert!(store.get(&out.record_id).await.expect("get").is_none());
    assert!(
        store
            .list(&ListArgs::default())
            .await
            .expect("list")
            .records
            .is_empty()
    );

    let versions = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(versions.len(), 1);
    assert!(!versions[0].active);
    assert!(versions[0].tombstoned);
    assert_eq!(versions[0].tombstone_reason, Some(TombstoneReason::Expire));

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let row_count: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
                .map_err(Into::into)
        })
        .await
        .expect("record count");
    assert_eq!(row_count, 1, "expire must not hard-delete records");
}

#[tokio::test]
async fn expire_preserves_existing_tombstone_reason() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    let out = store.upsert(&r).await.expect("upsert");

    store
        .tombstone(&out.record_id, TombstoneReason::Forget)
        .await
        .expect("forget tombstone");
    store.expire(&r.target_id).await.expect("expire");

    let versions = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(versions.len(), 1);
    assert!(versions[0].tombstoned);
    assert_eq!(versions[0].tombstone_reason, Some(TombstoneReason::Forget));
}

#[tokio::test]
async fn upsert_after_expire_creates_next_visible_version() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("upsert");
    store.expire(&r.target_id).await.expect("expire");

    let mut replacement = r.clone();
    replacement.body = "replacement after expire".to_owned();
    let out = store
        .upsert(&replacement)
        .await
        .expect("replacement upsert");

    assert_eq!(out.version, 2);
    assert!(store.get(&out.record_id).await.expect("get").is_some());

    let versions = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(versions.len(), 2);
    assert!(versions[0].tombstoned);
    assert!(!versions[1].tombstoned);
    assert!(versions[1].active);
}
