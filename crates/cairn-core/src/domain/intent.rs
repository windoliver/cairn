//! [`VerifiedSignedIntent`] — typed wrapper signaling a `SignedIntent`
//! has passed signature/replay verification at the trust boundary.
//!
//! `validate_against_intent` accepts only this type so the type system
//! catches "called with raw bytes off the wire" at compile time. The
//! verifier crate (P1+) constructs it via [`VerifiedSignedIntent::assume_verified`]
//! after checking issuer signature, expiry, nonce, and sequence. At P0
//! the constructor is the only path; once a real verifier exists the
//! constructor moves behind a sealed trait so external callers must go
//! through the verifier.
//!
//! This is type-system signaling, not crypto. The wrapper holds no
//! additional state — it's a marker that the intent was verified
//! upstream. Misuse (constructing without verification) is caught by
//! review and the unsafe-style `assume_verified` naming.

use crate::generated::envelope::SignedIntent;

/// A `SignedIntent` whose signature, expiry, nonce, and sequence have
/// been verified at the trust boundary. Construct via
/// [`Self::assume_verified`] only after running the upstream verifier.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedSignedIntent(SignedIntent);

impl VerifiedSignedIntent {
    /// Wrap a `SignedIntent` whose verification has already happened.
    /// The name is deliberately sharp — call sites that pass an
    /// unverified intent are easy to grep for, and the verifier crate
    /// is the only intended caller. Domain-level validation never
    /// re-derives crypto truth from this wrapper; it only consumes the
    /// already-verified fields.
    #[must_use]
    pub fn assume_verified(intent: SignedIntent) -> Self {
        Self(intent)
    }

    /// Borrow the underlying `SignedIntent` for read-only inspection
    /// during containment checks.
    #[must_use]
    pub fn as_inner(&self) -> &SignedIntent {
        &self.0
    }
}
