//! Connection setup: open, pragma application, and migration runner.

use crate::error::SqliteStoreError;
use crate::schema::{MIGRATIONS, runner::apply_pending};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

/// `SQLite` connection shared between `SqliteMemoryStore` (reads) and the
/// apply-tx surface (writes). `Connection` is `!Sync`, so it lives behind
/// a `tokio::sync::Mutex`.
pub type SharedConn = Arc<Mutex<Connection>>;

/// Pragma batch applied after every `Connection::open`. Must not be run
/// as a migration SQL file — pragmas cannot be executed inside a
/// user-initiated transaction.
const PRAGMAS: &str = "
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
PRAGMA synchronous = NORMAL;
";

/// Open the database at `path`, apply pragmas, and run all pending
/// migrations. Returns a `SharedConn` ready for read and write use.
///
/// This is a **synchronous** function. Callers from async contexts should
/// dispatch it to `tokio::task::spawn_blocking`.
///
/// # Errors
///
/// Returns [`SqliteStoreError`] if the file cannot be opened, pragma
/// application fails, or any migration fails its checksum or DDL
/// execution.
pub fn open_blocking(path: &Path) -> Result<SharedConn, SqliteStoreError> {
    let mut conn = Connection::open(path)?;
    conn.execute_batch(PRAGMAS)?;
    apply_pending(&mut conn, &MIGRATIONS)?;
    // Legacy-row gate: migration 0009 added `record_json` without a
    // deterministic backfill. Rows written before 0009 have
    // `record_json IS NULL`. The read path cannot reconstruct a valid
    // `MemoryRecord` from such a row, so opening a database that still
    // contains legacy rows is a data-loss hazard — every read would
    // silently report the row as missing. Fail closed at open() so
    // operators must run a repair/repopulate workflow before the
    // database is reachable from a running binary.
    let legacy_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE record_json IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if legacy_count > 0 {
        return Err(SqliteStoreError::LegacyRowsPresent {
            count: u64::try_from(legacy_count).unwrap_or(u64::MAX),
        });
    }
    Ok(Arc::new(Mutex::new(conn)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_creates_schema_migrations() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("cairn.db");
        let conn = open_blocking(&path).expect("open");
        let conn = conn.try_lock().expect("mutex");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .expect("query");
        assert_eq!(
            usize::try_from(count).expect("count fits usize"),
            MIGRATIONS.len()
        );
    }

    #[test]
    fn open_idempotent() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("cairn.db");
        let first = open_blocking(&path).expect("first open");
        drop(first);
        let second = open_blocking(&path).expect("second open");
        let conn = second.try_lock().expect("mutex");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .expect("query");
        assert_eq!(
            usize::try_from(count).expect("count fits usize"),
            MIGRATIONS.len(),
            "no duplicate ledger rows on re-open"
        );
    }

    #[test]
    fn pragmas_applied() {
        let dir = tempdir().expect("tempdir");
        let conn = open_blocking(&dir.path().join("cairn.db")).expect("open");
        let conn = conn.try_lock().expect("mutex");
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .expect("journal_mode pragma");
        assert_eq!(journal, "wal");
        let fks: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .expect("foreign_keys pragma");
        assert_eq!(fks, 1);
    }
}
