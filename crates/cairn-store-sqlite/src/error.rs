//! Store-level error type.

use thiserror::Error;

/// Errors raised by the `SQLite` store adapter.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// Underlying `SQLite` failure.
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),

    /// Background `tokio_rusqlite` worker error (channel closed, panic in
    /// the worker thread, etc.).
    #[error("tokio_rusqlite worker error")]
    Worker(#[from] tokio_rusqlite::Error),

    /// Migration runner failure.
    #[error("migration error")]
    Migration(#[from] rusqlite_migration::Error),

    /// Vault path is unusable (cannot create parent directory, etc.).
    #[error("vault path error: {0}")]
    VaultPath(String),

    /// On-disk schema diverged from the binary's expected manifest
    /// (missing trigger, mutated migration row, extra object, etc.).
    #[error("schema drift: {0}")]
    SchemaDrift(String),
}
