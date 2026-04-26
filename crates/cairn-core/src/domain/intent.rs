//! [`VerifiedSignedIntent`] — typed wrapper signaling a `SignedIntent`
//! has passed signature/replay verification at the trust boundary.
//!
//! `validate_against_intent` accepts only this type so the type system
//! catches "called with raw bytes off the wire" at compile time. The
//! constructor is **not public** — only the future verifier crate (and
//! cairn-core's own tests) can produce one. External code that needs a
//! verified intent must go through the verifier API, which performs
//! issuer-signature, expiry, nonce, and sequence/replay checks.
//!
//! Domain-level validation never re-derives crypto truth from the
//! wrapper; it only reads the already-verified fields. The wrapper
//! holds no extra state — it's a typed proof token.

use crate::generated::envelope::SignedIntent;

/// A `SignedIntent` whose signature, expiry, nonce, and sequence have
/// been verified at the trust boundary. Construction is gated to
/// cairn-core (and its tests) — external code obtains one only through
/// the verifier API.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedSignedIntent(SignedIntent);

impl VerifiedSignedIntent {
    /// `pub(crate)` so cairn-core's own modules can construct the
    /// wrapper after running internal verification, and so the verifier
    /// crate (added in a follow-up) can call this through a sealed
    /// trait. External code cannot bypass verification. Currently
    /// unused — production callers will arrive once the verifier ships.
    #[allow(
        dead_code,
        reason = "production callers arrive with the verifier crate"
    )]
    pub(crate) fn from_verified(intent: SignedIntent) -> Self {
        Self(intent)
    }

    /// Test-only constructor for cairn-core integration tests, gated to
    /// `#[cfg(any(test, feature = "test-helpers"))]`. The
    /// `test-helpers` feature is **not** enabled in any production
    /// build profile and is intended for downstream test code only.
    #[cfg(any(test, feature = "test-helpers"))]
    #[doc(hidden)]
    #[must_use]
    pub fn dangerous_unverified_for_testing(intent: SignedIntent) -> Self {
        Self(intent)
    }

    /// Borrow the underlying `SignedIntent` for read-only inspection
    /// during containment checks.
    #[must_use]
    pub fn as_inner(&self) -> &SignedIntent {
        &self.0
    }
}
