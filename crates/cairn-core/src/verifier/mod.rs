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
//! Replay / nonce / sequence / handshake-challenge enforcement is **not**
//! handled here — see issue #52.

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
    /// # Errors
    /// One of [`DomainError::Unauthorized`], [`DomainError::RevokedKey`],
    /// [`DomainError::ExpiredIntent`], [`DomainError::ScopeDenied`],
    /// [`DomainError::InvalidSignature`].
    pub fn verify(
        &self,
        intent: SignedIntent,
        resolved: &ResolvedIssuer,
    ) -> Result<VerifiedSignedIntent, DomainError> {
        Self::check_issuer_match(&intent, resolved)?;
        Self::check_key_version(&intent, resolved)?;
        Self::check_lifecycle(resolved)?;
        self.check_expiry(&intent)?;
        self.check_scope(&intent)?;
        Self::check_signature(&intent, resolved)?;
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
