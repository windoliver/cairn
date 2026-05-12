//! `ConsentJournalReader` — read-only access to forget-related
//! `consent_journal` state for lint source-link hygiene checks.
//!
//! Issue #257, brief §5.6 and §14. This stays intentionally narrow:
//! lint needs to know whether a source-bytes hash has been forgotten so
//! active records can be flagged as resurrection paths.

use std::collections::HashSet;

/// Well-formed `source_forget` journal row relevant to lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceForget {
    /// Forget operation id that produced the row.
    pub op_id: String,
    /// Logical source id used for operator diagnostics.
    pub source_id: String,
    /// Raw source-bytes hash in `SourceRef.hash` space.
    pub source_bytes_hash: String,
}

/// Malformed `source_forget` journal row surfaced fail-closed through lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedSourceForget {
    /// Forget operation id carrying the malformed payload.
    pub op_id: String,
    /// Logical source id if present.
    pub source_id: String,
    /// Parsed source-bytes hash, or `None` when this field itself is malformed.
    pub source_bytes_hash: Option<String>,
    /// Why the row is considered malformed for enforcement.
    pub reason: MalformedSourceForgetReason,
}

/// Reason a `source_forget` row cannot participate in enforcement safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MalformedSourceForgetReason {
    /// Target replay-hash version is unknown to this binary.
    UnsupportedReplayHashVersion {
        /// Unsupported replay-hash version found on the row.
        version: u32,
    },
    /// Target replay-hash string is malformed.
    MalformedReplayHashFormat,
    /// Source-bytes hash is malformed.
    MalformedSourceBytesHashFormat,
}

/// Read-only view of the forget-related portion of `consent_journal`.
///
/// Object-safe so verb-layer code can pass a `&dyn ConsentJournalReader`
/// through `LintInputs` without leaking adapter types into core.
pub trait ConsentJournalReader: Send + Sync {
    /// Every forgotten source-bytes hash currently recorded.
    fn forgotten_source_bytes_hashes(&self) -> HashSet<String>;

    /// Every well-formed source-scope forget row currently recorded.
    fn forgotten_source_forgets(&self) -> Vec<SourceForget>;

    /// Every malformed `source_forget` row, regardless of source scope.
    fn malformed_source_forget_rows(&self) -> Vec<MalformedSourceForget>;

    /// Malformed `source_forget` rows intersecting a single source hash.
    fn malformed_source_forget_rows_for_source(
        &self,
        source_bytes_hash: &str,
    ) -> Vec<MalformedSourceForget>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticJournal {
        hashes: HashSet<String>,
    }

    impl ConsentJournalReader for StaticJournal {
        fn forgotten_source_bytes_hashes(&self) -> HashSet<String> {
            self.hashes.clone()
        }

        fn forgotten_source_forgets(&self) -> Vec<SourceForget> {
            Vec::new()
        }

        fn malformed_source_forget_rows(&self) -> Vec<MalformedSourceForget> {
            Vec::new()
        }

        fn malformed_source_forget_rows_for_source(
            &self,
            _source_bytes_hash: &str,
        ) -> Vec<MalformedSourceForget> {
            Vec::new()
        }
    }

    #[test]
    fn trait_is_object_safe() {
        fn accept(_: &dyn ConsentJournalReader) {}
        let journal = StaticJournal {
            hashes: HashSet::new(),
        };
        accept(&journal);
    }

    #[test]
    fn returns_hashes() {
        let journal = StaticJournal {
            hashes: HashSet::from(["sha256:abc".to_owned()]),
        };
        assert!(
            journal
                .forgotten_source_bytes_hashes()
                .contains("sha256:abc")
        );
    }
}
