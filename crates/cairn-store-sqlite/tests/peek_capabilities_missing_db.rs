//! Regression test: `peek_capabilities` must report `SchemaNotInitialized`
//! (not a `Sqlite` error) when the DB file does not exist.
//!
//! Surfaced by the issue-190 e2e run: a freshly bootstrapped vault has
//! no `cairn.db` until a store-opening verb runs, and the previous
//! implementation surfaced that as a generic sqlite "unable to open"
//! error which the `cairn status` graph-tools probe then reported as
//! `state: probe_failed, reason: store_open_error, error: "sqlite
//! error"` — alarming for what is the documented post-bootstrap state.

#![allow(missing_docs)]

use cairn_store_sqlite::{StoreError, peek_capabilities};

#[test]
fn peek_capabilities_returns_schema_not_initialized_for_missing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("does-not-exist.db");
    assert!(!missing.exists(), "precondition: file must not exist");

    match peek_capabilities(&missing) {
        Err(StoreError::SchemaNotInitialized) => {}
        other => panic!(
            "expected SchemaNotInitialized for missing DB, got {other:?} \
             — peek_capabilities must distinguish 'no DB yet' from 'DB broken'"
        ),
    }
}
