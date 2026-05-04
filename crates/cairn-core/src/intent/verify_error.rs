//! Typed errors returned by [`crate::verifier::verify_signed_intent`].
//!
//! Separate from [`crate::domain::DomainError`] so the wire-layer's
//! `policy_trace` (§8.0.b) can map verification failures to stable codes
//! without depending on record-validation variants.

use thiserror::Error;

use crate::contract::issuer_key_resolver::ResolverError;
use crate::domain::identity::{Identity, IdentityKind, keys::KeyVersion};
use crate::generated::envelope::SignedIntentScopeTier;

/// Outcome of a single envelope verification attempt.
///
/// Variants are non-exhaustive so `Replay`, `OutOfOrderSequence`, and
/// `ChallengeMismatch` can land in #52 without breaking call sites.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VerifyError {
    /// A syntactic invariant tripped post-deserialization (defense in
    /// depth — most syntactic checks already run in
    /// `RawSignedIntent::TryFrom`).
    #[error("malformed envelope: {field}: {reason}")]
    Malformed {
        /// Static identifier of the failing field, e.g. `"issuer"`.
        field: &'static str,
        /// Human-readable detail. Never contains record bodies or
        /// signature bytes.
        reason: String,
    },

    /// A timestamp window check failed.
    #[error("expired intent ({kind:?}): issued_at={issued_at} expires_at={expires_at} now={now}")]
    ExpiredIntent {
        /// `intent.issued_at`, echoed verbatim.
        issued_at: String,
        /// `intent.expires_at`, echoed verbatim.
        expires_at: String,
        /// Server-supplied `now` formatted as RFC3339.
        now: String,
        /// Which window check failed.
        kind: ExpiryReason,
    },

    /// The issuer's identity kind is not allowed to sign envelopes at
    /// the requested tier (brief §4.2 P0 baseline).
    #[error("scope denied: issuer kind {issuer_kind:?} cannot sign tier {requested_tier:?}")]
    ScopeDenied {
        /// Parsed kind of `intent.issuer`.
        issuer_kind: IdentityKind,
        /// `intent.scope.tier` echoed back.
        requested_tier: SignedIntentScopeTier,
    },

    /// Resolver returned `None` (no key entry for that
    /// `(issuer, key_version)` pair) or a non-operational lifecycle.
    #[error("unknown key: issuer={issuer} key_version={key_version}")]
    UnknownKey {
        /// Identity that signed (or claimed to sign) the envelope.
        issuer: Identity,
        /// Requested key version.
        key_version: KeyVersion,
    },

    /// Resolver returned a revoked key whose `effective_at` is on or
    /// before `intent.issued_at`. Earlier ops remain valid (brief §4.2).
    #[error("revoked key: issuer={issuer} key_version={key_version} effective_at={effective_at}")]
    RevokedKey {
        /// Identity that issued the envelope.
        issuer: Identity,
        /// Key version that was revoked.
        key_version: KeyVersion,
        /// Revocation `effective_at` formatted as RFC3339.
        effective_at: String,
    },

    /// Ed25519 verification rejected the signature, OR canonicalization
    /// of the envelope failed. Opaque on purpose — no oracle for
    /// differential timing.
    #[error("invalid signature")]
    InvalidSignature,

    /// The resolver itself failed to talk to its backing store.
    #[error("resolver failure")]
    ResolverFailure(#[source] ResolverError),
}

/// Sub-variant of [`VerifyError::ExpiredIntent`] identifying which
/// window predicate fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpiryReason {
    /// `|now − issued_at| > 2 min`. Bounds backdating against a stolen
    /// key (brief §4.2).
    Skewed,
    /// `now > expires_at`. Receipt has aged past its TTL.
    Past,
    /// `expires_at − issued_at > max_ttl`. Caller tried to extend their
    /// own TTL (brief §4.2: "clients can't extend their own TTLs").
    TtlExceeded,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::{Identity, IdentityKind, keys::KeyVersion};
    use crate::generated::envelope::SignedIntentScopeTier;

    #[test]
    fn malformed_display() {
        let e = VerifyError::Malformed {
            field: "issuer",
            reason: "bad prefix".to_owned(),
        };
        assert_eq!(e.to_string(), "malformed envelope: issuer: bad prefix");
    }

    #[test]
    fn expired_intent_display_kind_skewed() {
        let e = VerifyError::ExpiredIntent {
            issued_at: "2026-01-01T00:00:00Z".to_owned(),
            expires_at: "2026-01-01T00:05:00Z".to_owned(),
            now: "2026-01-01T01:00:00Z".to_owned(),
            kind: ExpiryReason::Skewed,
        };
        let s = e.to_string();
        assert!(s.contains("Skewed"));
        assert!(s.contains("issued_at"));
    }

    #[test]
    fn scope_denied_display() {
        let e = VerifyError::ScopeDenied {
            issuer_kind: IdentityKind::Agent,
            requested_tier: SignedIntentScopeTier::Team,
        };
        assert!(e.to_string().contains("Agent"));
        assert!(e.to_string().contains("Team"));
    }

    #[test]
    fn unknown_key_display() {
        let e = VerifyError::UnknownKey {
            issuer: Identity::parse("hmn:alice").expect("parse"),
            key_version: KeyVersion::FIRST,
        };
        assert!(e.to_string().contains("hmn:alice"));
    }

    #[test]
    fn invalid_signature_display_is_opaque() {
        let e = VerifyError::InvalidSignature;
        assert_eq!(e.to_string(), "invalid signature");
    }
}
