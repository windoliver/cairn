//! `SQLite` open path: pragmas + migrations, returning an async store handle.

use std::path::Path;
use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStoreCapabilities;
use tokio_rusqlite::Connection as AsyncConn;

use crate::error::StoreError;
use crate::migrations::migrations;
use crate::store::SqliteMemoryStore;
use crate::verify::{verify_migration_history, verify_schema_fingerprint};

/// Default capability flags. `fts` is enabled by the FTS5 search path
/// in `src/store/search.rs`; `vector` ships in a later issue (#48).
pub(crate) static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
    fts: true,
    vector: false,
    graph_edges: true,
    transactions: true,
};

const PRAGMAS: &str = "PRAGMA journal_mode=WAL;\
     PRAGMA foreign_keys=ON;\
     PRAGMA synchronous=NORMAL;\
     PRAGMA busy_timeout=5000;\
     PRAGMA temp_store=MEMORY;\
     PRAGMA mmap_size=268435456;";

/// Open (or create) the Cairn store at `path` and bring it to schema head.
///
/// # Errors
/// Returns [`StoreError`] if the directory cannot be created, the
/// connection cannot be opened, pragmas fail, or migrations fail.
pub async fn open(path: impl AsRef<Path>) -> Result<SqliteMemoryStore, StoreError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::VaultPath(e.to_string()))?;
    }
    let conn = AsyncConn::open(path).await?;
    bootstrap(&conn).await?;
    Ok(SqliteMemoryStore {
        conn: Some(Arc::new(conn)),
    })
}

/// In-memory store at schema head. For tests.
///
/// # Errors
/// Returns [`StoreError`] if pragmas or migrations fail.
pub async fn open_in_memory() -> Result<SqliteMemoryStore, StoreError> {
    let conn = AsyncConn::open_in_memory().await?;
    bootstrap(&conn).await?;
    Ok(SqliteMemoryStore {
        conn: Some(Arc::new(conn)),
    })
}

async fn bootstrap(conn: &AsyncConn) -> Result<(), StoreError> {
    conn.call(|c| {
        c.execute_batch(PRAGMAS)?;
        migrations()
            .to_latest(c)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        verify_migration_history(c).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        verify_schema_fingerprint(c).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok(())
    })
    .await?;
    Ok(())
}

/// Sync open at `path`, returning a raw `rusqlite::Connection`. For tests
/// that drive SQL directly (drift detection, migration validation). Not
/// part of the production API — gated behind `test-helpers` feature.
///
/// # Errors
/// Returns [`StoreError`] if the directory cannot be created, the
/// connection cannot be opened, pragmas fail, or migrations fail.
#[cfg(any(test, feature = "test-helpers"))]
pub fn open_sync(path: impl AsRef<Path>) -> Result<rusqlite::Connection, StoreError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::VaultPath(e.to_string()))?;
    }
    let mut conn = rusqlite::Connection::open(path)?;
    conn.execute_batch(PRAGMAS)?;
    migrations().to_latest(&mut conn)?;
    verify_migration_history(&conn)?;
    verify_schema_fingerprint(&conn)?;
    Ok(conn)
}

/// Sync in-memory open returning a raw `rusqlite::Connection` for tests
/// that drive SQL directly. Not part of the production API — gated behind
/// `test-helpers` feature.
///
/// # Errors
/// Returns [`StoreError`] if pragmas or migrations fail.
#[cfg(any(test, feature = "test-helpers"))]
pub fn open_in_memory_sync() -> Result<rusqlite::Connection, StoreError> {
    let mut conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch(PRAGMAS)?;
    migrations().to_latest(&mut conn)?;
    verify_migration_history(&conn)?;
    verify_schema_fingerprint(&conn)?;
    Ok(conn)
}
