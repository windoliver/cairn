//! `source_forget` journal entry — brief §5.6 phase A.
//!
//! Cairn's `forget --target source` codepath records a `source_forget` row in
//! the consent journal so subsequent re-ingestion can dedup by content-hash
//! and lint can flag active records that still reference a forgotten source
//! (issue #257 rules `source_not_forgotten` + `source_redact_on_forget_honored`).
//!
//! This module defines the typed projection the lint engine consumes. The
//! dispatch layer (CLI) is responsible for fetching rows out of the
//! `consent_journal` table and assembling a `source_id → SourceForgetEntry`
//! map before invoking `lint::run_checks`. Mirrors the `author_states`
//! pre-fetch pattern.
//!
//! The fields here are deliberately metadata-only — no source bytes, no
//! payload — so the entry is safe to log at any tracing level.

use serde::{Deserialize, Serialize};

use crate::domain::timestamp::Rfc3339Timestamp;

/// One row from the `source_forget` slice of the consent journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SourceForgetEntry {
    /// Vault-relative source path that was forgotten (e.g. `sources/hook/a.txt`).
    pub source_id: String,
    /// `wal_ops.operation_id` of the `forget` op that produced this entry.
    pub forget_op_id: String,
    /// RFC3339 timestamp of the journal row.
    pub decided_at: Rfc3339Timestamp,
    /// `Some(ts)` when the forget op also scrubbed source bytes (per
    /// `vault.redact_on_forget`); `None` when only the journal row was
    /// written and the source file is expected to remain on disk.
    pub redacted_at: Option<Rfc3339Timestamp>,
}

impl SourceForgetEntry {
    /// Build a new entry. Use in test fixtures and the SQLite adapter.
    #[must_use]
    pub fn new(
        source_id: impl Into<String>,
        forget_op_id: impl Into<String>,
        decided_at: Rfc3339Timestamp,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            forget_op_id: forget_op_id.into(),
            decided_at,
            redacted_at: None,
        }
    }

    /// Builder-style setter for `redacted_at`. Returns `self` for chaining.
    #[must_use]
    pub fn with_redacted_at(mut self, redacted_at: Rfc3339Timestamp) -> Self {
        self.redacted_at = Some(redacted_at);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse("2026-05-12T00:00:00Z").expect("invariant: valid ts")
    }

    #[test]
    fn new_defaults_redacted_at_to_none() {
        let entry = SourceForgetEntry::new("sources/x.txt", "op-1", ts());
        assert_eq!(entry.source_id, "sources/x.txt");
        assert_eq!(entry.forget_op_id, "op-1");
        assert!(entry.redacted_at.is_none());
    }

    #[test]
    fn with_redacted_at_sets_field() {
        let entry = SourceForgetEntry::new("sources/x.txt", "op-1", ts()).with_redacted_at(ts());
        assert!(entry.redacted_at.is_some());
    }
}
