//! Store-level error type.

use thiserror::Error;

/// Errors raised by the `SQLite` store adapter.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// Underlying `SQLite` failure.
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),

    /// Async `SQLite` runner failure (`tokio_rusqlite`).
    #[error("async sqlite error")]
    AsyncSqlite(#[from] tokio_rusqlite::Error),

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
