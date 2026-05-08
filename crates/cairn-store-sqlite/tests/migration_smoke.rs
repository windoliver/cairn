//! Smoke test: verify that `0002_identity.sql` applies cleanly to an
//! in-memory `SQLite` database without returning an error.

/// Ensure the identity migration DDL parses and executes without error.
#[test]
fn migration_0002_applies_cleanly() {
    let r = cairn_store_sqlite::SqliteIdentityRegistry::open_in_memory();
    assert!(r.is_ok(), "{:?}", r.err());
}

/// Identity shares the store DB, so its migration guard must validate the
/// identity-owned objects directly instead of trusting SQLite user_version.
#[test]
fn migration_0002_rejects_identity_schema_drift() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("cairn.db");
    let registry = cairn_store_sqlite::SqliteIdentityRegistry::open(&db).expect("open registry");
    drop(registry);

    let conn = rusqlite::Connection::open(&db).expect("open db");
    conn.execute_batch("DROP TRIGGER vault_meta_no_update")
        .expect("drop trigger");
    drop(conn);

    let err = match cairn_store_sqlite::SqliteIdentityRegistry::open(&db) {
        Ok(_) => panic!("schema drift must reject"),
        Err(err) => err,
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("identity schema mismatch"),
        "unexpected error: {msg}"
    );
}

/// Unexpected identity-owned objects are drift too, not harmless sidecars.
#[test]
fn migration_0002_rejects_extra_identity_schema_object() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("cairn.db");
    let registry = cairn_store_sqlite::SqliteIdentityRegistry::open(&db).expect("open registry");
    drop(registry);

    let conn = rusqlite::Connection::open(&db).expect("open db");
    conn.execute_batch(
        "CREATE TRIGGER identities_extra_guard \
           BEFORE INSERT ON identities \
         BEGIN \
           SELECT 1; \
         END;",
    )
    .expect("create extra trigger");
    drop(conn);

    let err = match cairn_store_sqlite::SqliteIdentityRegistry::open(&db) {
        Ok(_) => panic!("extra schema object must reject"),
        Err(err) => err,
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("identity schema mismatch"),
        "unexpected error: {msg}"
    );
}

/// Verify that migration 0022 adds the expected generated columns and indices
/// for trace records (issue #77, spec §6.1).
#[test]
fn migration_0022_trace_links_applies() {
    let conn = cairn_store_sqlite::open_in_memory_sync()
        .expect("open_in_memory_sync: migrations should apply cleanly");

    // Verify generated columns exist on `records`. Use `table_xinfo` (extended)
    // rather than `table_info` because SQLite omits generated/virtual columns
    // from the non-extended PRAGMA output.
    let mut stmt = conn
        .prepare("PRAGMA table_xinfo(records)")
        .expect("prepare PRAGMA table_xinfo");
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query_map table_xinfo")
        .filter_map(Result::ok)
        .collect();
    for expected in [
        "trace_event",
        "trace_session_id",
        "trace_turn_id",
        "trace_sequence",
        "trace_capture_event_id",
        "trace_parent_event_id",
        "trace_payload_hash",
    ] {
        assert!(
            cols.contains(&expected.to_string()),
            "missing column {expected}"
        );
    }

    // Verify expected indices exist.
    let mut idx = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='records'")
        .expect("prepare sqlite_master index query");
    let names: Vec<String> = idx
        .query_map([], |r| r.get(0))
        .expect("query_map index names")
        .filter_map(Result::ok)
        .collect();
    for expected in [
        "records_trace_event_id",
        "records_trace_summary",
        "records_trace_seq",
        "records_trace_parent",
        "records_trace_payload_hash",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "missing index {expected}"
        );
    }
}
