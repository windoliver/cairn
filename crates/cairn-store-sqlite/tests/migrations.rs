//! Integration tests for the migration runner.
//!
//! Covers: empty-DB apply, re-apply no-op, checksum-mismatch abort, P0 tables.

use cairn_store_sqlite::conn::open_blocking;
use cairn_store_sqlite::error::SqliteStoreError;
use cairn_store_sqlite::schema::MIGRATIONS;
use rusqlite::params;
use tempfile::tempdir;

#[test]
fn apply_on_empty_db() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("cairn.db");
    let conn = open_blocking(&path).expect("open_blocking");
    let conn = conn.try_lock().expect("mutex");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .expect("count query");
    assert_eq!(
        usize::try_from(n).expect("count fits usize"),
        MIGRATIONS.len(),
        "all migrations must be recorded in schema_migrations"
    );
}

#[test]
fn re_apply_is_no_op() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("cairn.db");
    {
        let _first = open_blocking(&path).expect("first open");
    }
    let conn = open_blocking(&path).expect("second open");
    let conn = conn.try_lock().expect("mutex");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .expect("count query");
    assert_eq!(
        usize::try_from(n).expect("count fits usize"),
        MIGRATIONS.len(),
        "no duplicate ledger rows on re-open"
    );
}

#[test]
fn checksum_mismatch_aborts() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("cairn.db");
    {
        // First open applies all migrations.
        let conn = open_blocking(&path).expect("first open");
        let conn = conn.try_lock().expect("mutex");
        // Tamper with migration 2's checksum in the ledger.
        conn.execute(
            "UPDATE schema_migrations SET checksum = ?1 WHERE id = 2",
            params!["deadbeef"],
        )
        .expect("UPDATE checksum");
    }
    // Second open should detect the mismatch and abort.
    let err = open_blocking(&path).expect_err("should fail on checksum mismatch");
    assert!(
        matches!(err, SqliteStoreError::ChecksumMismatch { .. }),
        "expected ChecksumMismatch, got: {err:?}"
    );
}

#[test]
fn p0_tables_present() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("cairn.db");
    let conn = open_blocking(&path).expect("open_blocking");
    let conn = conn.try_lock().expect("mutex");

    let expected = [
        "records",
        "record_purges",
        "records_fts",
        "edges",
        "edge_versions",
        "wal_ops",
        "wal_steps",
        "replay_ledger",
        "issuer_seq",
        "challenges",
        "consent_journal",
        "locks",
        "reader_fence",
        "jobs",
        "schema_migrations",
    ];

    for table in expected {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type IN ('table', 'view') AND name = ?1",
                params![table],
                |r| r.get(0),
            )
            .expect("sqlite_master query");
        assert_eq!(n, 1, "missing table or view: {table}");
    }
}
