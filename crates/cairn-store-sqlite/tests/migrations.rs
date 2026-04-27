//! Integration tests for the migration set.

use cairn_store_sqlite::{migrations::migrations, open, open_in_memory};
use rusqlite::params;
use tempfile::tempdir;

#[test]
fn fresh_in_memory_opens_to_head() {
    let conn = open_in_memory().expect("open in-memory store");
    let head: i64 = conn
        .query_row("SELECT MAX(migration_id) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .expect("query head");
    assert_eq!(head, 6);
}

#[test]
fn fresh_vault_opens_and_reopens_idempotent() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _conn = open(&db).expect("first open");
    }
    let conn = open(&db).expect("reopen");
    let head: i64 = conn
        .query_row("SELECT MAX(migration_id) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .expect("query head");
    assert_eq!(head, 6);
}

#[test]
fn pragmas_applied() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    let conn = open(&db).expect("open");

    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("journal_mode");
    assert_eq!(journal.to_lowercase(), "wal");

    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .expect("foreign_keys");
    assert_eq!(fk, 1);
}

#[test]
fn migrations_validate() {
    migrations()
        .validate()
        .expect("migrations validate against schema");
}

#[test]
fn fts_round_trip() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO records \
         (record_id, target_id, version, path, kind, class, visibility, scope, \
          actor_chain, body, body_hash, created_at, updated_at, active, tombstoned, is_static) \
         VALUES ('r1','t1',1,'p','note','n','public','s','[]','hello world','h',0,0,1,0,0)",
        [],
    )
    .expect("insert record");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records_fts WHERE records_fts MATCH ?",
            params!["hello"],
            |r| r.get(0),
        )
        .expect("fts query");
    assert_eq!(count, 1);
}

#[test]
fn updates_edge_kind_flip_blocked() {
    let conn = open_in_memory().expect("open");
    conn.execute_batch(
        "INSERT INTO records \
          (record_id, target_id, version, path, kind, class, visibility, scope, \
           actor_chain, body, body_hash, created_at, updated_at, active, tombstoned, is_static) \
          VALUES ('r1','t1',1,'p','note','n','public','s','[]','b1','h',0,0,1,0,0); \
         INSERT INTO records \
          (record_id, target_id, version, path, kind, class, visibility, scope, \
           actor_chain, body, body_hash, created_at, updated_at, active, tombstoned, is_static) \
          VALUES ('r2','t2',1,'p','note','n','public','s','[]','b2','h',0,0,1,0,0); \
         INSERT INTO edges (src, dst, kind) VALUES ('r1','r2','related');",
    )
    .expect("seed records + benign edge");
    let err = conn
        .execute(
            "UPDATE edges SET kind = 'updates' WHERE src = 'r1' AND dst = 'r2'",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("updates edge identity"),
        "kind-flip should be blocked, got: {err}"
    );
}

#[test]
fn schema_drift_detected_on_weakened_trigger() {
    use tempfile::tempdir;
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _ = open(&db).expect("first open");
    }
    {
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        // Same name, weaker body — name-only fingerprint would miss this.
        conn.execute_batch(
            "DROP TRIGGER schema_migrations_no_delete; \
             CREATE TRIGGER schema_migrations_no_delete \
               BEFORE DELETE ON schema_migrations \
               FOR EACH ROW WHEN 0 \
             BEGIN SELECT 1; END;",
        )
        .expect("recreate weaker trigger");
    }
    let err = open(&db).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("DDL digest mismatch") || msg.contains("schema fingerprint"),
        "DDL drift should be detected, got: {err}"
    );
}

#[test]
fn schema_drift_detected_on_dropped_trigger() {
    use tempfile::tempdir;
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _ = open(&db).expect("first open");
    }
    {
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        conn.execute("DROP TRIGGER records_fts_ai", [])
            .expect("drop trigger");
    }
    let err = open(&db).unwrap_err();
    assert!(
        format!("{err}").contains("schema fingerprint mismatch"),
        "drift should be detected, got: {err}"
    );
}

#[test]
fn migration_hash_drift_detected() {
    use tempfile::tempdir;
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _ = open(&db).expect("first open");
    }
    {
        // Bypass the immutable trigger by dropping it, then tampering.
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        conn.execute("DROP TRIGGER schema_migrations_immutable", [])
            .expect("drop immutability trigger");
        conn.execute(
            "UPDATE schema_migrations SET sql_hash = 'deadbeef' WHERE migration_id = 1",
            [],
        )
        .expect("tamper hash");
    }
    let err = open(&db).unwrap_err();
    assert!(
        format!("{err}").contains("hash mismatch") || format!("{err}").contains("schema drift"),
        "hash drift should be detected, got: {err}"
    );
}

#[test]
fn schema_migrations_is_append_only() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute("DELETE FROM schema_migrations WHERE migration_id = 1", [])
        .unwrap_err();
    assert!(format!("{err}").contains("schema_migrations is append-only"));
}

#[test]
fn wal_ops_terminal_immutable() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO wal_ops (operation_id, issued_seq, kind, state, envelope, issuer, \
          target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
         VALUES ('op1', 1, 'upsert', 'ISSUED', '{}', 'i', 'h', '{}', 0, 'sig', 0, 0)",
        [],
    )
    .expect("insert wal_ops");
    conn.execute(
        "UPDATE wal_ops SET state = 'PREPARED' WHERE operation_id = 'op1'",
        [],
    )
    .expect("ISSUED -> PREPARED");
    conn.execute(
        "UPDATE wal_ops SET state = 'COMMITTED' WHERE operation_id = 'op1'",
        [],
    )
    .expect("PREPARED -> COMMITTED");
    let err = conn
        .execute(
            "UPDATE wal_ops SET reason = 'x' WHERE operation_id = 'op1'",
            [],
        )
        .unwrap_err();
    assert!(format!("{err}").contains("terminal-state"));
}
