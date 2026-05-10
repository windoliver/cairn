//! Migration 0053 stores body-bearing WAL payloads for record operations.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::wal::WalKind;
use cairn_store_sqlite::open_in_memory;
use rusqlite::params;

#[tokio::test]
async fn wal_payloads_table_is_present_and_rejects_non_scrub_updates() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));

    conn.call(|c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-payload', 1, ?1, 'ISSUED', '{}', 'issuer', 'target', '{}', 0, 'sig', 1, 1)",
            params![WalKind::Upsert.as_str()],
        )?;
        c.execute(
            "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
             VALUES ('op-payload', 'upsert', '{\"kind\":\"upsert\"}', 1)",
            [],
        )?;

        let payload: String = c.query_row(
            "SELECT payload_json FROM wal_payloads WHERE operation_id = 'op-payload'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(payload, "{\"kind\":\"upsert\"}");

        let update = c.execute(
            "UPDATE wal_payloads SET payload_json = '{}' WHERE operation_id = 'op-payload'",
            [],
        );
        assert!(update.is_err(), "payload rows must not be updated");

        let delete = c.execute("DELETE FROM wal_payloads WHERE operation_id = 'op-payload'", []);
        assert!(delete.is_err(), "payload rows must not be deleted");

        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("schema assertions");
}

#[tokio::test]
async fn wal_payloads_requires_existing_operation() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));

    conn.call(|c| {
        let err = c
            .execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES ('missing-op', 'upsert', '{}', 1)",
                [],
            )
            .expect_err("foreign key rejects missing wal_ops row");
        assert!(err.to_string().contains("FOREIGN KEY"));
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("foreign-key assertion");
}

#[tokio::test]
async fn wal_payloads_kind_must_match_parent_operation() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));

    conn.call(|c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-expire-payload', 1, ?1, 'ISSUED', '{}', 'issuer', 'target', '{}', 0, 'sig', 1, 1)",
            params![WalKind::Expire.as_str()],
        )?;

        let err = c
            .execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES ('op-expire-payload', 'upsert', '{}', 1)",
                [],
            )
            .expect_err("trigger rejects payload kind mismatch");
        assert!(
            err.to_string()
                .contains("wal_payloads.kind must match wal_ops.kind"),
            "unexpected mismatch error: {err}"
        );

        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("kind-match assertion");
}

#[tokio::test]
async fn wal_payloads_accepts_forget_record_and_only_scrub_updates() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));

    conn.call(|c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('forget-record-payload', 1, 'forget_record', 'ISSUED', '{}', \
                     'issuer', 'hash:00000000000000000000000000000000', 'user=hmn:tafeng', \
                     0, 'sig', 1, 1)",
            [],
        )?;
        c.execute(
            "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
             VALUES ('forget-record-payload', 'forget_record', \
                     '{\"type\":\"forget_record\",\"target_hash\":\"hash:00000000000000000000000000000000\"}', 1)",
            [],
        )?;

        let normal_update = c.execute(
            "UPDATE wal_payloads SET payload_json = '{}' \
             WHERE operation_id = 'forget-record-payload'",
            [],
        );
        assert!(
            normal_update.is_err(),
            "non-scrub payload updates must still be blocked"
        );

        let missing_type_scrub = c.execute(
            "UPDATE wal_payloads \
                SET kind = 'purged', payload_json = '{}' \
              WHERE operation_id = 'forget-record-payload'",
            [],
        );
        assert!(
            missing_type_scrub.is_err(),
            "scrub updates must require a purged payload type"
        );

        c.execute(
            "UPDATE wal_payloads \
                SET kind = 'purged', \
                    payload_json = '{\"type\":\"purged\",\"target_hash\":\"hash:00000000000000000000000000000000\",\"purged_by\":\"forget-record-payload\",\"purged_at\":1}' \
              WHERE operation_id = 'forget-record-payload'",
            [],
        )?;
        let kind: String = c.query_row(
            "SELECT kind FROM wal_payloads WHERE operation_id = 'forget-record-payload'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(kind, "purged");

        let second_scrub = c.execute(
            "UPDATE wal_payloads \
                SET kind = 'purged', \
                    payload_json = '{\"type\":\"purged\",\"target_hash\":\"hash:00000000000000000000000000000000\",\"purged_by\":\"forget-record-payload\",\"purged_at\":2}' \
              WHERE operation_id = 'forget-record-payload'",
            [],
        );
        assert!(
            second_scrub.is_err(),
            "purged payload rows must not be scrubbed again"
        );

        let delete = c.execute(
            "DELETE FROM wal_payloads WHERE operation_id = 'forget-record-payload'",
            [],
        );
        assert!(delete.is_err(), "payload rows remain append-only");

        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("schema assertions");
}
