//! Issue #254: `wal_ops.kind` CHECK accepts `'lint_repair'` after migration 0031.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_store_sqlite::open_in_memory;

#[tokio::test]
async fn wal_ops_accepts_lint_repair_kind() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = store
        .raw_conn_for_admin()
        .expect("store has a live connection");

    conn.call(|c| {
        c.execute(
            "INSERT INTO wal_ops(\
               operation_id, issued_seq, kind, state, envelope, \
               issuer, target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-lint-1', 1, 'lint_repair', 'ISSUED', '{}', 'i', 'h', '{}', 0, '', 0, 0)",
            [],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("kind 'lint_repair' must satisfy CHECK constraint");
}

#[tokio::test]
async fn wal_ops_rejects_unknown_kind() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = store
        .raw_conn_for_admin()
        .expect("store has a live connection");

    let r = conn
        .call(|c| {
            c.execute(
                "INSERT INTO wal_ops(\
                   operation_id, issued_seq, kind, state, envelope, \
                   issuer, target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES ('op-bogus', 1, 'totally_made_up', 'ISSUED', '{}', 'i', 'h', '{}', 0, '', 0, 0)",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await;

    assert!(r.is_err(), "unknown kind must violate CHECK");
}

#[tokio::test]
async fn wal_ops_still_accepts_original_kinds() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = store
        .raw_conn_for_admin()
        .expect("store has a live connection");

    let kinds = [
        "upsert",
        "delete",
        "promote",
        "expire",
        "forget_session",
        "forget_record",
        "evolve",
    ];

    for (seq, kind) in kinds.iter().enumerate() {
        let op_id = format!("op-{kind}");
        let kind_owned = (*kind).to_owned();
        let seq = i64::try_from(seq + 1).expect("seq fits i64");
        conn.call(move |c| {
            c.execute(
                "INSERT INTO wal_ops(\
                   operation_id, issued_seq, kind, state, envelope, \
                   issuer, target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES (?, ?, ?, 'ISSUED', '{}', 'i', 'h', '{}', 0, '', 0, 0)",
                rusqlite::params![op_id, seq, kind_owned],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .unwrap_or_else(|e| panic!("original kind '{kind}' must still be accepted: {e}"));
    }
}

/// Regression for the migration 0031 FK-target preservation: child tables
/// (`wal_steps`, `wal_op_deps`, `reader_fence`) must continue to FK-resolve
/// to the rebuilt `wal_ops` parent. Without the `PRAGMA writable_schema`
/// patch path (or `legacy_alter_table = ON` during a rebuild),
/// `ALTER TABLE wal_ops RENAME TO wal_ops_old` rewrites child FK targets
/// to `wal_ops_old`; the rebuilt parent is unreferenced, and
/// `PRAGMA foreign_key_check` reports orphaned children.
#[tokio::test]
async fn wal_ops_child_fks_resolve_after_migration_0031() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = store
        .raw_conn_for_admin()
        .expect("store has a live connection");

    conn.call(|c| {
        // Foreign-key enforcement is the whole point of this test.
        c.pragma_update(None, "foreign_keys", "ON")?;

        // Seed a parent wal_ops row.
        c.execute(
            "INSERT INTO wal_ops(\
               operation_id, issued_seq, kind, state, envelope, \
               issuer, target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-fk-parent', 1, 'upsert', 'ISSUED', '{}', 'i', 'h', '{}', 0, '', 0, 0)",
            [],
        )?;

        // FK child must resolve to the rebuilt parent.
        c.execute(
            "INSERT INTO wal_steps(operation_id, step_ord, step_kind, state) \
             VALUES ('op-fk-parent', 0, 'project', 'PENDING')",
            [],
        )?;

        // FK violation must be rejected.
        let bad: rusqlite::Result<usize> = c.execute(
            "INSERT INTO wal_steps(operation_id, step_ord, step_kind, state) \
             VALUES ('op-does-not-exist', 0, 'project', 'PENDING')",
            [],
        );
        assert!(
            bad.is_err(),
            "wal_steps FK must reject unknown operation_id"
        );

        // PRAGMA foreign_key_check must report a clean tree.
        let mut stmt = c.prepare("PRAGMA foreign_key_check")?;
        let mut rows = stmt.query([])?;
        assert!(
            rows.next()?.is_none(),
            "foreign_key_check must return no orphaned rows after migration 0031",
        );

        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("FK preservation regression");
}
