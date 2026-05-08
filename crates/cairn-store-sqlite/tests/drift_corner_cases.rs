//! Corner cases for schema-drift detection beyond the basic dropped-trigger.

use cairn_store_sqlite::open_sync as open;
use tempfile::tempdir;

fn fresh_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _ = open(&db).expect("first open");
    }
    (dir, db)
}

#[test]
fn drift_detected_when_view_redefined_weaker() {
    let (_dir, db) = fresh_db();
    {
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        conn.execute_batch(
            "DROP VIEW records_latest; \
             CREATE VIEW records_latest AS SELECT * FROM records;",
        )
        .expect("recreate weaker view");
    }
    let err = open(&db).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("DDL digest mismatch") || msg.contains("schema fingerprint"),
        "view redefinition must be detected, got: {err}"
    );
}

#[test]
fn drift_detected_when_extra_object_added() {
    let (_dir, db) = fresh_db();
    {
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        conn.execute("CREATE TABLE rogue (x INTEGER)", [])
            .expect("add rogue table");
    }
    let err = open(&db).unwrap_err();
    assert!(
        format!("{err}").contains("schema fingerprint mismatch"),
        "extra object must be reported, got: {err}"
    );
}

#[test]
fn drift_detected_on_index_predicate_change() {
    let (_dir, db) = fresh_db();
    {
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        conn.execute_batch(
            "DROP INDEX records_path_idx; \
             CREATE INDEX records_path_idx ON records(path);",
        )
        .expect("drop partial-index predicate");
    }
    let err = open(&db).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("DDL digest mismatch"),
        "index predicate change must trip the DDL digest, got: {err}"
    );
}

#[test]
fn idempotent_reopen_preserves_stamped_hash() {
    let (_dir, db) = fresh_db();
    let read_hash = || -> String {
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        conn.query_row(
            "SELECT sql_hash FROM schema_migrations WHERE migration_id = 1",
            [],
            |r| r.get(0),
        )
        .expect("read hash")
    };
    let h1 = read_hash();
    assert!(!h1.is_empty(), "hash should be stamped on first open");
    let _ = open(&db).expect("reopen");
    let h2 = read_hash();
    assert_eq!(h1, h2, "stamped hash must remain stable across reopens");
}
