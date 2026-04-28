//! Folder sidecars — `_index.md`, `_policy.yaml`, `_summary.md` (brief §3.4).
//!
//! Pure functions only — zero I/O, zero async. Caller (CLI / future hooks)
//! supplies records and policy bytes; module returns projected files,
//! parsed policies, and resolved effective policies.

pub mod index;
pub mod links;
pub mod policy;

#[allow(unused_imports)] // links module is empty until Task 5
pub use links::*;
pub use policy::*;

/// Errors raised by pure folder helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FolderError {
    /// `_policy.yaml` could not be parsed as a `FolderPolicy`.
    #[error("policy parse failed: {source}")]
    PolicyParse {
        /// Underlying `serde_yaml` error.
        #[source]
        source: serde_yaml::Error,
    },
}

use std::path::PathBuf;

use crate::domain::Rfc3339Timestamp;

/// Schema for `_summary.md`. P0 ships types only — body generation is P1.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderSummary {
    /// Vault-relative folder path.
    pub folder: PathBuf,
    /// When this summary was generated.
    pub generated_at: Rfc3339Timestamp,
    /// Agent that generated the summary (e.g. `agt:cairn-librarian:v2`).
    pub generated_by: String,
    /// Number of records the summary covers.
    pub covers_records: u32,
    /// Approximate token count of `body`.
    pub summary_tokens: u32,
    /// Generated prose body.
    pub body: String,
}

/// Errors raised by [`FolderSummaryWriter`] implementations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FolderSummaryError {
    /// No summary writer is registered (P0 default).
    #[error("folder summary writer not registered")]
    Unimplemented,
    /// Internal error from the writer implementation.
    #[error("folder summary writer internal: {0}")]
    Internal(String),
}

/// Workflow-owned write surface for `_summary.md`. P0 ships zero
/// implementations; P1 `cairn-workflows::FolderSummaryWorkflow` is the
/// first implementor.
#[async_trait::async_trait]
pub trait FolderSummaryWriter: Send + Sync {
    /// Persist a generated [`FolderSummary`] as `_summary.md` under its
    /// folder, atomically. Implementors are responsible for I/O safety
    /// (atomic rename, symlink rejection).
    ///
    /// # Errors
    ///
    /// Returns [`FolderSummaryError::Unimplemented`] when no writer is
    /// registered, or [`FolderSummaryError::Internal`] for I/O / encoding
    /// failures.
    async fn write_summary(
        &self,
        summary: FolderSummary,
    ) -> Result<(), FolderSummaryError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_error_displays_with_source() {
        let yaml = "purpose: [unclosed";
        let err = serde_yaml::from_str::<serde_yaml::Value>(yaml).unwrap_err();
        let folder_err = FolderError::PolicyParse { source: err };
        assert!(folder_err.to_string().starts_with("policy parse failed:"));
    }

    #[test]
    fn folder_summary_writer_trait_object_compiles() {
        struct Stub;
        #[async_trait::async_trait]
        impl FolderSummaryWriter for Stub {
            async fn write_summary(
                &self,
                _summary: FolderSummary,
            ) -> Result<(), FolderSummaryError> {
                Err(FolderSummaryError::Unimplemented)
            }
        }
        let _: Box<dyn FolderSummaryWriter> = Box::new(Stub);
    }
}
