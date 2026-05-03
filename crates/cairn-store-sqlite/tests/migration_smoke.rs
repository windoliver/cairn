//! Smoke test: verify that `0002_identity.sql` applies cleanly to an
//! in-memory `SQLite` database without returning an error.

/// Ensure the identity migration DDL parses and executes without error.
#[test]
fn migration_0002_applies_cleanly() {
    let r = cairn_store_sqlite::SqliteIdentityRegistry::open_in_memory();
    assert!(r.is_ok(), "{:?}", r.err());
}
