//! Migration 0053 stores body-bearing WAL payloads for record operations.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::wal::WalKind;
use cairn_store_sqlite::open_in_memory;
use rusqlite::params;

#[tokio::test]
async fn wal_payloads_table_is_present_and_immutable() {
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
async fn migration_0055_widens_wal_payloads_kind_to_include_forget_record() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));

    // Insert a sentinel wal_ops row with kind=forget_record so the FK +
    // trigger constraints are satisfied, then insert the matching payload
    // under the widened CHECK introduced by migration 0055.
    conn.call(|c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-mig-55', 1, 'forget_record', 'ISSUED', '{}', 'test', \
                     'target', '{}', 0, 'sig', 1, 1)",
            [],
        )?;
        c.execute(
            "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
             VALUES ('op-mig-55', 'forget_record', '{}', 1)",
            [],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("forget_record payload insert succeeds under widened CHECK");
}
