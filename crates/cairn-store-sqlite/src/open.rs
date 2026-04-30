//! `SQLite` open path: pragmas + migrations, returning an async store handle.

use std::path::Path;
use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStoreCapabilities;
use cairn_embeddings_local::EmbeddingModel;
use tokio_rusqlite::Connection as AsyncConn;
use tokio_util::sync::CancellationToken;

use crate::error::StoreError;
use crate::migrations::migrations;
use crate::store::SqliteMemoryStore;
use crate::vec_ext::register_vec0;
use crate::verify::{verify_migration_history, verify_schema_fingerprint};

const PRAGMAS: &str = "PRAGMA journal_mode=WAL;\
     PRAGMA foreign_keys=ON;\
     PRAGMA synchronous=NORMAL;\
     PRAGMA busy_timeout=5000;\
     PRAGMA temp_store=MEMORY;\
     PRAGMA mmap_size=268435456;";

/// Build the base capability set based on whether an embedder is present.
fn base_caps(vector: bool) -> MemoryStoreCapabilities {
    MemoryStoreCapabilities {
        fts: true,
        vector,
        graph_edges: true,
        transactions: true,
    }
}

/// Finish constructing a [`SqliteMemoryStore`] from an open connection and
/// optional embedder. Spawns the background drain loop when an embedder is
/// provided.
fn build_store(conn: AsyncConn, embedder: Option<Arc<dyn EmbeddingModel>>) -> SqliteMemoryStore {
    let conn = Arc::new(conn);
    let vector = embedder.is_some();
    let caps = base_caps(vector);
    let cancel = embedder.as_ref().map(|_| CancellationToken::new());

    if let (Some(emb), Some(tok)) = (embedder.as_ref(), cancel.as_ref()) {
        let conn2 = Arc::clone(&conn);
        let emb2 = Arc::clone(emb);
        let tok2 = tok.clone();
        tokio::spawn(crate::store::reindex::drain_loop(conn2, emb2, tok2));
    }

    SqliteMemoryStore {
        conn: Some(conn),
        embedder,
        caps,
        _cancel: cancel,
    }
}

/// Open (or create) the Cairn store at `path` and bring it to schema head.
///
/// # Errors
/// Returns [`StoreError`] if the directory cannot be created, the
/// connection cannot be opened, pragmas fail, or migrations fail.
pub async fn open(path: impl AsRef<Path>) -> Result<SqliteMemoryStore, StoreError> {
    open_with_embedder(path, None).await
}

/// Open (or create) the Cairn store at `path` with an optional local
/// embedding model. When `embedder` is `Some`, the `vector` capability is
/// enabled and a background drain loop is spawned to embed queued records.
///
/// # Errors
/// Returns [`StoreError`] if the directory cannot be created, the
/// connection cannot be opened, pragmas fail, or migrations fail.
pub async fn open_with_embedder(
    path: impl AsRef<Path>,
    embedder: Option<Arc<dyn EmbeddingModel>>,
) -> Result<SqliteMemoryStore, StoreError> {
    // Register the sqlite-vec vec0 module globally before opening any
    // connection so migration 0020 (CREATE VIRTUAL TABLE USING vec0) succeeds.
    register_vec0();
    let path = path.as_ref();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::VaultPath(e.to_string()))?;
    }
    let conn = AsyncConn::open(path).await?;
    bootstrap(&conn).await?;
    Ok(build_store(conn, embedder))
}

/// In-memory store at schema head. For tests.
///
/// # Errors
/// Returns [`StoreError`] if pragmas or migrations fail.
pub async fn open_in_memory() -> Result<SqliteMemoryStore, StoreError> {
    open_in_memory_with_embedder(None).await
}

/// In-memory store at schema head with an optional local embedding model.
/// For tests that exercise the vector search path.
///
/// # Errors
/// Returns [`StoreError`] if pragmas or migrations fail.
pub async fn open_in_memory_with_embedder(
    embedder: Option<Arc<dyn EmbeddingModel>>,
) -> Result<SqliteMemoryStore, StoreError> {
    register_vec0();
    let conn = AsyncConn::open_in_memory().await?;
    bootstrap(&conn).await?;
    Ok(build_store(conn, embedder))
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
    register_vec0();
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
    register_vec0();
    let mut conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch(PRAGMAS)?;
    migrations().to_latest(&mut conn)?;
    verify_migration_history(&conn)?;
    verify_schema_fingerprint(&conn)?;
    Ok(conn)
}
