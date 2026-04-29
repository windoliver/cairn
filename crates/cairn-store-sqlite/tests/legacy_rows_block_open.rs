// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Regression: opening a database with pre-0009 legacy rows
//! (`record_json IS NULL`) must fail closed with
//! `SqliteStoreError::LegacyRowsPresent`. The read path cannot
//! reconstruct a valid `MemoryRecord` from such rows, so opening would
//! silently drop them on every `get`/`list` — a data-loss hazard.

use cairn_store_sqlite::{SqliteMemoryStore, error::SqliteStoreError};
use rusqlite::params;
use tempfile::tempdir;

#[tokio::test]
async fn open_fails_closed_when_legacy_rows_have_null_record_json() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("cairn.db");

    // First open — runs all migrations, no rows yet.
    {
        let _store = SqliteMemoryStore::open(&path)
            .await
            .expect("first open succeeds with empty db");
    }

    // Inject a row that simulates a pre-0009 record:
    // every required column is populated EXCEPT `record_json`,
    // which stays NULL as it would for a row written before the
    // migration added the column.
    {
        let conn = rusqlite::Connection::open(&path).expect("open raw");
        conn.execute(
            "INSERT INTO records ( \
                 record_id, target_id, version, active, tombstoned, \
                 created_at, created_by, body, provenance, actor_chain, \
                 evidence, scope, taxonomy, confidence, salience \
             ) VALUES (?1, ?2, 1, 0, 0, '2026-04-22T14:02:11Z', \
                 'usr:legacy', 'old body', '{}', '[]', '{}', '{}', \
                 '{\"visibility\":\"public\"}', 0.5, 0.5)",
            params!["rec/legacy#1", "rec/legacy"],
        )
        .expect("insert legacy row");
    }

    // Re-open: the legacy-row gate must fire.
    match SqliteMemoryStore::open(&path).await {
        Ok(_) => panic!("re-open must reject legacy rows but succeeded"),
        Err(SqliteStoreError::LegacyRowsPresent { count: 1 }) => {}
        Err(other) => {
            panic!("expected LegacyRowsPresent {{ count: 1 }}, got {other:?}")
        }
    }
}
