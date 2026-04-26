//! [`VerifiedSignedIntent`] — typed wrapper signaling a `SignedIntent`
//! has passed signature/replay verification at the trust boundary.
//!
//! `validate_against_intent` accepts only this type so the type system
//! catches "called with raw bytes off the wire" at compile time. The
//! constructor is **not public** — production code obtains an instance
//! only via the future verifier crate, which performs issuer-signature,
//! expiry, nonce, and sequence/replay checks before returning it. The
//! verifier-side construction path lives behind a sealed trait
//! (`SignedIntentVerifier::__from_verified`) so cairn-core stays the
//! sole authority for instantiation.
//!
//! Domain-level validation never re-derives crypto truth from the
//! wrapper; it only reads the already-verified fields. The wrapper
//! holds no extra state — it's a typed proof token.

use crate::generated::envelope::SignedIntent;

/// A `SignedIntent` whose signature, expiry, nonce, and sequence have
/// been verified at the trust boundary. Cannot be constructed outside
/// cairn-core: production callers go through a sealed verifier trait,
/// and tests are confined to `#[cfg(test)]` modules within this crate.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedSignedIntent(SignedIntent);

/// Sealed trait the future verifier crate implements to mint
/// [`VerifiedSignedIntent`] values. The required method has a
/// hidden-doc, leading-underscore name and is parameterized on a
/// private witness type, so external crates can implement the trait
/// only after invoking a constructor that already ran the verifier
/// pipeline. Cairn-core's verifier (P1+) plugs in here; meanwhile the
/// trait simply documents the boundary.
pub mod sealed {
    /// Private witness used to gate
    /// [`super::SignedIntentVerifier::__from_verified`] — only
    /// cairn-core constructs `VerifierWitness` values, so external
    /// crates cannot satisfy the trait method even by name-matching.
    #[derive(Debug)]
    pub struct VerifierWitness {
        _private: (),
    }

    impl VerifierWitness {
        /// Construct the witness inside cairn-core. Only the
        /// verification pipeline (P1+) calls this — see module docs.
        #[allow(
            dead_code,
            reason = "verifier-side caller arrives with the verifier crate"
        )]
        pub(crate) const fn new() -> Self {
            Self { _private: () }
        }
    }
}

/// Sealed trait — the future verifier crate constructs
/// [`VerifiedSignedIntent`] only by calling `__from_verified` after a
/// successful verification pipeline. The `VerifierWitness` parameter
/// is constructable only inside cairn-core, so external code cannot
/// bypass verification by implementing the trait.
pub trait SignedIntentVerifier {
    /// Mint a [`VerifiedSignedIntent`] after running issuer-signature,
    /// expiry, nonce, and sequence/replay checks. The witness proves
    /// the call originates from cairn-core's verification pipeline.
    #[doc(hidden)]
    #[must_use]
    fn __from_verified(
        intent: SignedIntent,
        _witness: sealed::VerifierWitness,
    ) -> VerifiedSignedIntent {
        VerifiedSignedIntent(intent)
    }
}

impl VerifiedSignedIntent {
    /// Borrow the underlying `SignedIntent` for read-only inspection
    /// during containment checks.
    #[must_use]
    pub fn as_inner(&self) -> &SignedIntent {
        &self.0
    }

    /// Test-only constructor — `#[cfg(test)]` confines this path to
    /// cairn-core's own unit/integration tests. Downstream crates
    /// cannot reach it under any feature flag or build profile.
    #[cfg(test)]
    pub(crate) fn from_verified_for_test(intent: SignedIntent) -> Self {
        Self(intent)
    }
}
