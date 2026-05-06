//! `SQLite` store error surface.

use thiserror::Error;

/// Errors returned by the `SQLite` store adapter.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Underlying `SQLite` error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A required schema object is missing.
    #[error("required schema object missing: {object}")]
    SchemaMissing {
        /// Missing schema object name.
        object: &'static str,
    },

    /// An entity edge stored an unsupported confidence value.
    #[error("invalid confidence for edge {edge_id}: {value}")]
    InvalidConfidence {
        /// Edge id containing the unsupported confidence.
        edge_id: String,
        /// Unsupported confidence value.
        value: String,
    },
}
