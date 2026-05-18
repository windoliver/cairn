//! Snapshot-backed `ConsentJournalReader` adapter for `SQLite`.

use std::collections::HashSet;
use std::path::Path;

use cairn_core::contract::{ConsentJournalReader, MalformedSourceForget, SourceForget};

use crate::StoreError;
use crate::consent;

/// Read-only snapshot of forget-related consent journal state.
#[derive(Debug, Clone, Default)]
pub struct SqliteConsentJournalReader {
    forgotten_source_hashes: HashSet<String>,
    source_forgets: Vec<SourceForget>,
    malformed: Vec<MalformedSourceForget>,
}

impl SqliteConsentJournalReader {
    /// Open a snapshot from the journal at `path`.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the database cannot be opened or the
    /// journal cannot be read.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        // Read-only snapshot: the journal table is created by the main
        // async `open`, so we only need a raw connection here — no
        // migrations, no pragmas, no vec0 registration.
        let conn = rusqlite::Connection::open(path.as_ref())?;
        Self::from_connection(&conn)
    }

    /// Build a snapshot from an existing `rusqlite` connection.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the journal cannot be queried or if a
    /// future `source_forget` row is malformed.
    pub fn from_connection(conn: &rusqlite::Connection) -> Result<Self, StoreError> {
        let source_forgets = consent::forgotten_source_forgets(conn)?;
        Ok(Self {
            forgotten_source_hashes: source_forgets
                .iter()
                .map(|row| row.source_bytes_hash.clone())
                .collect(),
            source_forgets,
            malformed: consent::malformed_source_forget_rows(conn)?,
        })
    }
}

impl ConsentJournalReader for SqliteConsentJournalReader {
    fn forgotten_source_bytes_hashes(&self) -> HashSet<String> {
        self.forgotten_source_hashes.clone()
    }

    fn forgotten_source_forgets(&self) -> Vec<SourceForget> {
        self.source_forgets.clone()
    }

    fn malformed_source_forget_rows(&self) -> Vec<MalformedSourceForget> {
        self.malformed.clone()
    }

    fn malformed_source_forget_rows_for_source(
        &self,
        source_bytes_hash: &str,
    ) -> Vec<MalformedSourceForget> {
        self.malformed
            .iter()
            .filter(|row| row.source_bytes_hash.as_deref() == Some(source_bytes_hash))
            .cloned()
            .collect()
    }
}
