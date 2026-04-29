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
    assert_eq!(head, 7);
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
    assert_eq!(head, 7);
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
fn consent_journal_kind_domain_enforced() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, decided_at_iso) \
             VALUES ('c1', 's1', 'private', 'GRANT', 'usr:t', 0, \
                     'not_a_kind', '2026-04-28T12:00:00Z')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("kind not in §14 domain"),
        "kind CHECK should fire, got: {err}"
    );
}

#[test]
fn consent_journal_accepts_known_kinds() {
    let conn = open_in_memory().expect("open");
    for kind in [
        "sensor_enable",
        "sensor_disable",
        "policy_change",
        "remember_intent",
        "forget_intent",
        "grant",
        "revoke",
        "promote_receipt",
    ] {
        conn.execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, decided_at_iso) \
             VALUES (?, 's', 'private', 'GRANT', 'usr:t', 0, ?, '2026-04-28T12:00:00Z')",
            params![format!("c-{kind}"), kind],
        )
        .unwrap_or_else(|e| panic!("kind {kind} should be accepted: {e}"));
    }
}

#[test]
fn consent_journal_event_requires_iso_timestamp() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, kind) \
             VALUES ('c-no-iso', 's', 'private', 'GRANT', 'usr:t', 0, 'grant')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("decided_at_iso"),
        "iso requirement should fire, got: {err}"
    );
}

#[test]
fn consent_journal_kind_null_back_compat() {
    // Rows written before 0007 have kind = NULL. Inserting one should
    // still succeed — the trigger only fires when kind IS NOT NULL.
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('legacy', 's', 'private', 'GRANT', 'usr:t', 0)",
        [],
    )
    .expect("legacy insert with NULL kind");
}

#[test]
fn forget_intent_payload_must_be_body_free() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, decided_at_iso, payload_json) \
             VALUES ('c2', 'h', 'private', 'GRANT', 'usr:t', 0, 'forget_intent', \
                     '2026-04-28T12:00:00Z', \
                     '{\"target_id_hash\":\"h\",\"body\":\"leak\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("body-free"),
        "forget receipt body guard should fire, got: {err}"
    );
}

#[test]
fn forget_intent_payload_accepts_hash_only() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, decided_at_iso, payload_json) \
         VALUES ('c3', 'h', 'private', 'GRANT', 'usr:t', 0, 'forget_intent', \
                 '2026-04-28T12:00:00Z', \
                 '{\"target_id_hash\":\"hash:abc\",\"reason_code\":\"user_command\"}')",
        [],
    )
    .expect("hash-only payload should be accepted");
}

#[test]
fn consent_journal_queryable_by_op_actor_sensor_scope() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, decided_at_iso, op_id, actor, sensor_id, payload_json) \
         VALUES ('c4', 'snr:local:screen:host:v1', 'global', 'GRANT', 'usr:t', 0, \
                 'sensor_enable', '2026-04-28T12:00:00Z', \
                 'op-1', 'usr:tafeng', 'snr:local:screen:host:v1', \
                 '{\"shape\":\"sensor_toggle\",\"reason_code\":\"first_run_prompt\"}')",
        [],
    )
    .expect("seed sensor_enable row");

    // queryable by operation
    let by_op: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE op_id = 'op-1'",
            [],
            |r| r.get(0),
        )
        .expect("by op");
    assert_eq!(by_op, 1);

    // queryable by identity (actor)
    let by_actor: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE actor = 'usr:tafeng'",
            [],
            |r| r.get(0),
        )
        .expect("by actor");
    assert_eq!(by_actor, 1);

    // queryable by sensor
    let by_sensor: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE sensor_id = 'snr:local:screen:host:v1'",
            [],
            |r| r.get(0),
        )
        .expect("by sensor");
    assert_eq!(by_sensor, 1);

    // queryable by scope (already covered by the 0005 index, asserted here
    // for completeness against the issue AC).
    let by_scope: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE scope = 'global'",
            [],
            |r| r.get(0),
        )
        .expect("by scope");
    assert_eq!(by_scope, 1);
}

#[test]
fn consent_journal_remains_append_only_under_0007() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, decided_at_iso) \
         VALUES ('c5', 's', 'private', 'GRANT', 'usr:t', 0, 'grant', '2026-04-28T12:00:00Z')",
        [],
    )
    .expect("insert");

    let upd = conn
        .execute(
            "UPDATE consent_journal SET payload_json = '{}' WHERE consent_id = 'c5'",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{upd}").contains("immutable"),
        "UPDATE should still be blocked: {upd}"
    );

    let del = conn
        .execute("DELETE FROM consent_journal WHERE consent_id = 'c5'", [])
        .unwrap_err();
    assert!(
        format!("{del}").contains("append-only"),
        "DELETE should still be blocked: {del}"
    );
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
