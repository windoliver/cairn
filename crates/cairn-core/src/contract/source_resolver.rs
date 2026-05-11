//! `SourceResolver` — read-only access to source bytes referenced by
//! [`crate::domain::Provenance::source_refs`] (issue #257; brief §3, §6.5).
//!
//! The resolver is the *only* surface lint uses to touch `sources/`. The
//! trait is intentionally write-free: there is no `write`, `create`, or
//! `delete` method, and the CI grep gate scans the lint module to verify
//! no other write-path is reintroduced through a different API. Per
//! brief §3, sources are immutable from Cairn's side.
//!
//! `SourceRef::id` is opaque to lint. The resolver implementation is
//! responsible for mapping the logical id to actual storage — typically
//! `<vault>/<sources_dir>/<id>` for the filesystem adapter, where
//! `<sources_dir>` comes from `VaultLayout` (configurable; brief §3
//! permits non-default names like `inbox/`).

use std::fmt;

/// Read-only resolver from a logical `SourceRef.id` to source bytes.
///
/// Implementations are owned by adapters (filesystem in `cairn-cli`,
/// in-memory in `cairn-test-fixtures`). `cairn-core` never constructs
/// one — lint takes a `&dyn SourceResolver` from the caller.
pub trait SourceResolver: Send + Sync {
    /// Does this id resolve to a readable source?
    ///
    /// Implementations MUST NOT open the file for write or create
    /// missing parents — `exists` only inspects current state.
    fn exists(&self, id: &str) -> bool;

    /// Read the raw bytes of the source.
    ///
    /// # Errors
    ///
    /// Returns [`SourceResolverError::NotFound`] when the id has no
    /// backing source, or [`SourceResolverError::Io`] when the read
    /// itself fails for an underlying reason (permissions, transient
    /// filesystem error, malformed path). The latter is distinct from
    /// `NotFound` so lint can report it differently (`io` is an
    /// operator-level fault, `not_found` is a vault-level
    /// `source_link_dangling` finding).
    fn read(&self, id: &str) -> Result<Vec<u8>, SourceResolverError>;

    /// Diagnostic locator string for finding messages.
    ///
    /// Implementation-defined: filesystem returns the absolute path,
    /// in-memory returns a synthetic `"memory:<id>"`. Callers MUST NOT
    /// parse this — it is for operator messages only.
    fn locator(&self, id: &str) -> String;
}

/// Errors a [`SourceResolver`] can return.
#[derive(Debug)]
#[non_exhaustive]
pub enum SourceResolverError {
    /// The id has no backing source.
    NotFound,
    /// The read failed for an underlying reason (permissions, transient
    /// I/O, malformed path). The detail field carries an operator-
    /// readable description; lint surfaces it verbatim in the finding
    /// message.
    Io {
        /// Operator-readable description of the I/O failure.
        detail: String,
    },
}

impl fmt::Display for SourceResolverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "source not found"),
            Self::Io { detail } => write!(f, "source read failed: {detail}"),
        }
    }
}

impl std::error::Error for SourceResolverError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trivial in-test impl that proves the trait shape compiles
    /// against `&dyn SourceResolver`. Real impls live in adapter
    /// crates (cairn-cli for fs, cairn-test-fixtures for in-memory).
    struct Stub;

    impl SourceResolver for Stub {
        fn exists(&self, _id: &str) -> bool {
            false
        }
        fn read(&self, _id: &str) -> Result<Vec<u8>, SourceResolverError> {
            Err(SourceResolverError::NotFound)
        }
        fn locator(&self, id: &str) -> String {
            format!("stub:{id}")
        }
    }

    fn takes_resolver(r: &dyn SourceResolver) -> bool {
        r.exists("anything")
    }

    #[test]
    fn dyn_dispatch_compiles() {
        assert!(!takes_resolver(&Stub));
    }

    #[test]
    fn locator_is_diagnostic_only() {
        assert_eq!(Stub.locator("x/y"), "stub:x/y");
    }

    #[test]
    fn error_display_is_operator_readable() {
        assert_eq!(format!("{}", SourceResolverError::NotFound), "source not found");
        assert_eq!(
            format!(
                "{}",
                SourceResolverError::Io {
                    detail: "perm denied".into()
                }
            ),
            "source read failed: perm denied"
        );
    }
}
