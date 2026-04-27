//! `SQLite` open path: pragmas + migrations.

use std::path::Path;

use rusqlite::Connection;

use crate::error::StoreError;
use crate::migrations::migrations;

/// Open (or create) the Cairn store at `path` and bring it to schema head.
///
/// Applies persistent pragmas (WAL journal, foreign keys, busy timeout),
/// then runs `rusqlite_migration` to the latest migration.
///
/// # Errors
/// Returns [`StoreError`] if the directory cannot be created, the connection
/// cannot be opened, pragmas fail, or migrations fail.
pub fn open(path: impl AsRef<Path>) -> Result<Connection, StoreError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::VaultPath(e.to_string()))?;
    }

    let mut conn = Connection::open(path)?;
    apply_pragmas(&conn)?;
    migrations().to_latest(&mut conn)?;
    Ok(conn)
}

/// Open an in-memory store at schema head. For tests.
///
/// # Errors
/// Returns [`StoreError`] if pragmas or migrations fail.
pub fn open_in_memory() -> Result<Connection, StoreError> {
    let mut conn = Connection::open_in_memory()?;
    apply_pragmas(&conn)?;
    migrations().to_latest(&mut conn)?;
    Ok(conn)
}

fn apply_pragmas(conn: &Connection) -> Result<(), StoreError> {
    // `journal_mode = WAL` returns the resulting mode as a row, so use
    // execute_batch which ignores result rows. WAL is silently downgraded
    // to MEMORY for `:memory:` connections; that's fine.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA synchronous=NORMAL;\
         PRAGMA busy_timeout=5000;",
    )?;
    Ok(())
}
