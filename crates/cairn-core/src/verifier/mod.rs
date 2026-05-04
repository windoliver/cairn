//! Envelope verifier — the single trust boundary between adapter input
//! and pipeline / WAL code.
//!
//! Adapters call [`resolve::resolve_issuer`] once to materialise a
//! [`ResolvedIssuer`] from the registry, then build a long-lived
//! [`EnvelopeVerifier`] (cheap, borrows policy + clock) and call
//! [`EnvelopeVerifier::verify`] on every incoming envelope. The verifier is
//! synchronous so it can drop into the existing
//! [`crate::domain::intent::SignedIntentVerifier`] sealed-witness mint
//! path without forcing every caller to be `async`.
//!
//! # Out of scope for #51
//!
//! - **Replay / nonce / sequence / handshake-challenge** — see issue #52.
//! - **WAL-transaction-level freshness** (close the resolve-then-write
//!   race window for revocation/rotation) — see issue #55. The verifier
//!   guarantees authenticity at resolve time; the WAL state machine
//!   re-checks at PREPARE/COMMIT.
//! - **Constant-time / timing-oracle resistance.** Pre-auth failures
//!   take measurably different code paths (unknown issuer returns after
//!   `get_identity`; wrong key after `list_keys`; bad signature after
//!   the Ed25519 verify). The wire envelope is uniform — bytes are
//!   indistinguishable — but elapsed time is not. Brief §14 covers
//!   privacy-by-construction; closing the timing oracle requires
//!   uniform work (dummy verifications on unknown identities, padding,
//!   or rate limiting) that is out of scope at P0. Tracked as a
//!   follow-up; reconsider when remote unauthenticated callers become
//!   reachable (today the verifier sits behind in-process adapters
//!   only).

mod policy;
mod resolve;
mod resolved;

pub use policy::ScopePolicy;
pub use resolve::resolve_issuer;
pub use resolved::ResolvedIssuer;

use std::time::Duration;

use ed25519_dalek::{Signature, Verifier};

use crate::domain::DomainError;
use crate::domain::Identity;
use crate::domain::canonical::canonical_bytes_signed_intent;
use crate::domain::identity::keys::KeyVersion;
use crate::domain::identity::records::ProvisioningState;
use crate::domain::intent::{SignedIntentVerifier, sealed::VerifierWitness};
use crate::domain::time::Clock;
use crate::domain::{Rfc3339Timestamp, VerifiedSignedIntent};
use crate::generated::envelope::SignedIntent;

/// Hard-coded P0 clock-skew tolerance. Configurable in a follow-up issue.
const P0_SKEW: Duration = Duration::from_mins(1);

/// Hard-coded P0 maximum envelope time-to-live: `expires_at - issued_at`
/// must not exceed five minutes. Caps the blast radius of a leaked
/// signing key — even with the private key, an attacker cannot mint
/// envelopes that remain valid for hours or days. Configurable in a
/// follow-up issue alongside `P0_SKEW`.
const P0_MAX_TTL: Duration = Duration::from_mins(5);

/// Long-lived envelope verifier. Cheap to construct (borrows config).
pub struct EnvelopeVerifier<'a> {
    policy: &'a ScopePolicy,
    clock: &'a dyn Clock,
    skew: Duration,
}

impl<'a> EnvelopeVerifier<'a> {
    /// Build a verifier bound to the supplied scope policy and clock.
    /// Skew tolerance is fixed at 60 s for P0.
    #[must_use]
    pub fn new(policy: &'a ScopePolicy, clock: &'a dyn Clock) -> Self {
        Self {
            policy,
            clock,
            skew: P0_SKEW,
        }
    }

    /// Verify a [`SignedIntent`] against the resolved issuer key + the
    /// vault's scope policy and the wall-clock window.
    ///
    /// `resolved` is consumed by value: the snapshot is single-use, by
    /// design, to make the per-envelope freshness contract on
    /// [`ResolvedIssuer`] hard to misuse — a caller cannot reuse the
    /// snapshot across envelopes without explicitly re-resolving from
    /// the registry.
    ///
    /// # Errors
    /// One of [`DomainError::Unauthorized`], [`DomainError::RevokedKey`],
    /// [`DomainError::ExpiredIntent`], [`DomainError::ScopeDenied`],
    /// [`DomainError::InvalidSignature`].
    #[allow(
        clippy::needless_pass_by_value,
        reason = "intent ownership is consumed via shadowing — the round-trip below replaces \
                  the binding with a wire-validated SignedIntent, and the validated value is \
                  moved into VerifiedSignedIntent on success"
    )]
    pub fn verify(
        &self,
        intent: SignedIntent,
        resolved: ResolvedIssuer,
    ) -> Result<VerifiedSignedIntent, DomainError> {
        // Full wire-shape validation: in-process callers can construct a
        // `SignedIntent` directly, bypassing the IDL's
        // `RawSignedIntent::try_from` guards (ULID shape, base64 nonce
        // shape, sha256 target_hash, scope.entity non-empty, sequence
        // JSON safe-integer cap, chain_parents bounds + uniqueness, etc.).
        // Round-trip through serde to run every guard exactly as a
        // wire-deserialized envelope would.
        let intent: SignedIntent = serde_json::to_string(&intent)
            .ok()
            .and_then(|json| serde_json::from_str::<SignedIntent>(&json).ok())
            .ok_or(DomainError::InvalidSignature)?;

        // Authn first: bind the envelope to the resolved key, then verify
        // the signature, before any policy or lifecycle check. Order
        // matters — running scope/lifecycle/expiry first would let an
        // unauthenticated caller observe policy/key-rotation detail
        // through error-envelope `data` payloads (brief §14).
        Self::check_issuer_match(&intent, &resolved)?;
        Self::check_key_version(&intent, &resolved)?;
        Self::check_signature(&intent, &resolved)?;

        // Authz: only after the caller has proven control of the issuer
        // key do we surface lifecycle/expiry/scope detail.
        Self::check_lifecycle(&resolved)?;
        self.check_expiry(&intent)?;
        self.check_scope(&intent)?;
        Ok(<Self as SignedIntentVerifier>::__from_verified(
            intent,
            VerifierWitness::new(),
        ))
    }

    fn check_issuer_match(
        intent: &SignedIntent,
        resolved: &ResolvedIssuer,
    ) -> Result<(), DomainError> {
        let issuer =
            Identity::parse(intent.issuer.0.clone()).map_err(|_| DomainError::Unauthorized {
                message: format!(
                    "envelope issuer {} is not a parseable identity",
                    intent.issuer.0
                ),
            })?;
        if issuer != resolved.identity {
            return Err(DomainError::Unauthorized {
                message: format!(
                    "envelope issuer {issuer} does not match resolved {resolved}",
                    resolved = resolved.identity
                ),
            });
        }
        Ok(())
    }

    fn check_key_version(
        intent: &SignedIntent,
        resolved: &ResolvedIssuer,
    ) -> Result<(), DomainError> {
        let intent_version = u32::try_from(intent.key_version)
            .ok()
            .and_then(|n| std::num::NonZeroU32::new(n).map(KeyVersion::new))
            .ok_or_else(|| DomainError::Unauthorized {
                message: format!(
                    "envelope key_version {} is not a valid non-zero u32",
                    intent.key_version
                ),
            })?;
        if intent_version != resolved.key_version {
            return Err(DomainError::Unauthorized {
                message: format!(
                    "envelope key_version {intent_version} does not match resolved {}",
                    resolved.key_version
                ),
            });
        }
        Ok(())
    }

    fn check_lifecycle(resolved: &ResolvedIssuer) -> Result<(), DomainError> {
        if !matches!(resolved.state, ProvisioningState::Active) {
            return Err(DomainError::RevokedKey {
                id: resolved.identity.clone(),
                key_version: resolved.key_version,
                state: resolved.state,
            });
        }
        Ok(())
    }

    fn check_expiry(&self, intent: &SignedIntent) -> Result<(), DomainError> {
        let now = self.clock.now();
        let issued_at = Rfc3339Timestamp::parse(intent.issued_at.clone()).map_err(|_| {
            DomainError::ExpiredIntent {
                issued_at: intent.issued_at.clone(),
                expires_at: intent.expires_at.clone(),
                now: now.to_rfc3339(),
            }
        })?;
        let expires_at = Rfc3339Timestamp::parse(intent.expires_at.clone()).map_err(|_| {
            DomainError::ExpiredIntent {
                issued_at: intent.issued_at.clone(),
                expires_at: intent.expires_at.clone(),
                now: now.to_rfc3339(),
            }
        })?;

        let issued_chrono = issued_at.as_chrono();
        let expires_chrono = expires_at.as_chrono();

        let skew_chrono =
            chrono::Duration::from_std(self.skew).map_err(|_| DomainError::Unauthorized {
                message: "skew duration overflowed chrono::Duration".into(),
            })?;
        let max_ttl_chrono =
            chrono::Duration::from_std(P0_MAX_TTL).map_err(|_| DomainError::Unauthorized {
                message: "max_ttl duration overflowed chrono::Duration".into(),
            })?;

        // Well-formed window: expires_at must lie strictly after
        // issued_at. Caller-supplied timestamps are untrusted; an
        // empty or inverted window is a fail-closed signal.
        if expires_chrono <= issued_chrono {
            return Err(DomainError::ExpiredIntent {
                issued_at: intent.issued_at.clone(),
                expires_at: intent.expires_at.clone(),
                now: now.to_rfc3339(),
            });
        }

        // Bounded acceptance — measured from the verifier's wall clock,
        // not from the signer's `issued_at`. Caps real validity at
        // `P0_MAX_TTL` regardless of whether the signer chose an
        // `issued_at` in the past, present, or (within `skew`) future.
        // Anchoring on `issued_at` would let a future-issued envelope
        // (issued_at = now + skew, expires_at = issued_at + P0_MAX_TTL)
        // extend real acceptance to `P0_MAX_TTL + skew`, defeating the
        // leaked-key blast-radius cap that this check exists for.
        if expires_chrono > now + max_ttl_chrono {
            return Err(DomainError::ExpiredIntent {
                issued_at: intent.issued_at.clone(),
                expires_at: intent.expires_at.clone(),
                now: now.to_rfc3339(),
            });
        }

        let earliest = issued_chrono - skew_chrono;
        if now < earliest || now >= expires_chrono {
            return Err(DomainError::ExpiredIntent {
                issued_at: intent.issued_at.clone(),
                expires_at: intent.expires_at.clone(),
                now: now.to_rfc3339(),
            });
        }
        Ok(())
    }

    fn check_scope(&self, intent: &SignedIntent) -> Result<(), DomainError> {
        if intent.scope.tenant != self.policy.tenant {
            return Err(DomainError::ScopeDenied {
                message: format!(
                    "tenant: expected {}, got {}",
                    self.policy.tenant, intent.scope.tenant
                ),
            });
        }
        if intent.scope.workspace != self.policy.workspace {
            return Err(DomainError::ScopeDenied {
                message: format!(
                    "workspace: expected {}, got {}",
                    self.policy.workspace, intent.scope.workspace
                ),
            });
        }
        if !self.policy.allowed_tiers.contains(&intent.scope.tier) {
            return Err(DomainError::ScopeDenied {
                message: format!("tier {:?} not in allow-list", intent.scope.tier),
            });
        }
        Ok(())
    }

    fn check_signature(
        intent: &SignedIntent,
        resolved: &ResolvedIssuer,
    ) -> Result<(), DomainError> {
        let bytes = canonical_bytes_signed_intent(intent)?;
        let hex_tail = intent
            .signature
            .0
            .strip_prefix("ed25519:")
            .ok_or(DomainError::InvalidSignature)?;
        // Defensive: Ed25519Signature exposes pub String, so an in-process caller
        // (test, SDK) can construct an invalid value bypassing the IDL deserializer.
        // chunks_exact(2) below would silently truncate an odd-length tail; explicit
        // length check makes the failure deterministic.
        if hex_tail.len() != 128 {
            return Err(DomainError::InvalidSignature);
        }
        let mut sig_bytes = [0u8; 64];
        for (i, chunk) in hex_tail.as_bytes().chunks_exact(2).enumerate() {
            let hi = decode_hex_nibble(chunk[0]).ok_or(DomainError::InvalidSignature)?;
            let lo = decode_hex_nibble(chunk[1]).ok_or(DomainError::InvalidSignature)?;
            sig_bytes[i] = (hi << 4) | lo;
        }
        let sig = Signature::from_bytes(&sig_bytes);
        resolved
            .verifying_key
            .verify(&bytes, &sig)
            .map_err(|_| DomainError::InvalidSignature)
    }
}

impl SignedIntentVerifier for EnvelopeVerifier<'_> {}

fn decode_hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod proptests;
