//! Folder sidecars — `_index.md`, `_policy.yaml`, `_summary.md` (brief §3.4).
//!
//! Pure functions only — zero I/O, zero async. Caller (CLI / future hooks)
//! supplies records and policy bytes; module returns projected files,
//! parsed policies, and resolved effective policies.

pub mod index;
pub mod links;
pub mod policy;

pub use index::*;
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
}
