//! `rusqlite`-aware error wrapping. Produces typed `StoreError::Conflict`
//! variants where `SQLite` returns a recognizable constraint-violation
//! code; otherwise wraps via `Backend`.

use cairn_core::contract::memory_store::error::{ConflictKind, StoreError};
use rusqlite::ErrorCode;
use thiserror::Error;

/// Adapter-level errors for the `SQLite` store. Wraps `rusqlite` errors and
/// adds migration-specific variants; converts to `StoreError` via `From`.
#[derive(Debug, Error)]
pub enum SqliteStoreError {
    /// A rusqlite operation failed.
    #[error("rusqlite: {0}")]
    Rusqlite(#[from] rusqlite::Error),

    /// A migration SQL batch failed to execute.
    #[error("migration {migration}: {source}")]
    Migration {
        /// Migration filename.
        migration: String,
        /// Underlying rusqlite error.
        #[source]
        source: rusqlite::Error,
    },

    /// A committed migration's bytes have changed (checksum mismatch).
    #[error("migration checksum mismatch for {migration}")]
    ChecksumMismatch {
        /// Migration filename that failed the checksum check.
        migration: String,
    },

    /// I/O error (e.g. path resolution).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization error.
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

impl From<SqliteStoreError> for StoreError {
    fn from(e: SqliteStoreError) -> Self {
        if let SqliteStoreError::Rusqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error { code, .. },
            _msg,
        )) = &e
            && matches!(code, ErrorCode::ConstraintViolation)
        {
            return StoreError::Conflict {
                kind: ConflictKind::UniqueViolation,
            };
        }
        StoreError::Backend(Box::new(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::contract::memory_store::error::StoreError;

    #[test]
    fn rusqlite_unique_to_conflict() {
        let inner = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation,
                extended_code: 0,
            },
            None,
        );
        let store_err: StoreError = SqliteStoreError::Rusqlite(inner).into();
        assert!(
            matches!(
                store_err,
                StoreError::Conflict {
                    kind: ConflictKind::UniqueViolation
                }
            ),
            "expected UniqueViolation, got {store_err:?}"
        );
    }

    #[test]
    fn checksum_mismatch_maps_to_backend() {
        let err = SqliteStoreError::ChecksumMismatch {
            migration: "0002_records.sql".to_string(),
        };
        let store_err: StoreError = err.into();
        assert!(matches!(store_err, StoreError::Backend(_)));
    }
}
