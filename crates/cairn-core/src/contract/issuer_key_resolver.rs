//! `IssuerKeyResolver` — minimal trust-boundary lookup for
//! `(issuer, key_version) → public key + lifecycle`.
//!
//! Verification only consumes one fact from the identity stack: "is
//! this key still trusted at the moment the envelope was issued?" The
//! full [`crate::contract::IdentityRegistry`] surface (provisioning,
//! rotation, revocation receipts) is far broader than that — verifier
//! callers route through this narrower trait so a fake implementation
//! is trivial in tests and the `SQLite` adapter exposes only what the
//! verifier actually needs.

use async_trait::async_trait;
use thiserror::Error;

use crate::domain::identity::{Identity, keys::KeyVersion};
use crate::domain::timestamp::Rfc3339Timestamp;

/// Single-method contract: look up the pubkey + lifecycle for an
/// `(issuer, key_version)` pair.
#[async_trait]
pub trait IssuerKeyResolver: Send + Sync {
    /// Resolve `(issuer, key_version)` to the verifying-key bytes plus
    /// the issuer's current lifecycle state.
    ///
    /// Returns `Ok(None)` when no row exists for the `(issuer,
    /// key_version)` pair — verifier maps that to
    /// `VerifyError::UnknownKey`.
    ///
    /// # Errors
    /// Returns [`ResolverError::Backend`] when the underlying store
    /// (`SQLite`, in-memory fake) fails.
    async fn lookup(
        &self,
        issuer: &Identity,
        key_version: KeyVersion,
    ) -> Result<Option<ResolvedKey>, ResolverError>;
}

/// Output of a successful resolver lookup.
///
/// `public_key` is the raw 32-byte Ed25519 verifying key — same shape
/// as [`crate::domain::identity::records::IdentityKeyEntry::public_key`].
/// `lifecycle` is normalized down to four states the verifier cares
/// about; the registry's full state machine (Pending,
/// `RevokePending`, `PurgePending`, etc.) collapses to those four.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedKey {
    /// 32-byte Ed25519 verifying key.
    pub public_key: [u8; 32],
    /// Trust state of this `(issuer, key_version)` pair.
    pub lifecycle: KeyLifecycle,
}

/// Verifier-relevant collapse of the registry's lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyLifecycle {
    /// Active — the issuer is operational and signing is permitted.
    Active,
    /// Revoked at `effective_at`. Earlier ops remain valid;
    /// the verifier compares `effective_at` against `intent.issued_at`.
    Revoked {
        /// When the revocation took effect.
        effective_at: Rfc3339Timestamp,
    },
    /// Identity row exists but is in a non-operational lifecycle
    /// (`Pending`, `RevokePending`, `PurgePending`). Verifier maps to
    /// `VerifyError::UnknownKey` same as a missing row.
    NonOperational,
    /// Identity row was purged. Verifier maps to `UnknownKey`.
    Purged,
}

/// Error path from the adapter implementation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ResolverError {
    /// The backing store failed (`SQLite` I/O error, in-memory map
    /// poisoned, etc.).
    #[error("backend error: {0}")]
    Backend(
        /// The underlying backend error.
        #[source]
        Box<dyn std::error::Error + Send + Sync>,
    ),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_lifecycle_active_construction() {
        let lc = KeyLifecycle::Active;
        assert!(matches!(lc, KeyLifecycle::Active));
    }

    #[test]
    fn key_lifecycle_revoked_carries_effective_at() {
        let lc = KeyLifecycle::Revoked {
            effective_at: crate::domain::timestamp::Rfc3339Timestamp::parse(
                "2026-01-01T00:00:00Z".to_owned(),
            )
            .expect("valid rfc3339"),
        };
        match lc {
            KeyLifecycle::Revoked { effective_at } => {
                assert_eq!(effective_at.as_str(), "2026-01-01T00:00:00Z");
            }
            _ => panic!("expected revoked"),
        }
    }

    #[test]
    fn resolver_error_backend_wraps() {
        let inner: Box<dyn std::error::Error + Send + Sync> = "io error".into();
        let e = ResolverError::Backend(inner);
        assert_eq!(e.to_string(), "backend error: io error");
    }
}
