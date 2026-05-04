//! [`ResolvedIssuer`] — pre-resolved issuer-key snapshot consumed by
//! [`super::EnvelopeVerifier::verify`].
//!
//! Built only by [`super::resolve::resolve_issuer`]; no public constructor
//! outside cairn-core. `Debug` redacts the verifying-key bytes — defensive,
//! since they are public, but avoids accidental leakage in logs.

use ed25519_dalek::VerifyingKey;

use crate::domain::Identity;
use crate::domain::identity::keys::KeyVersion;
use crate::domain::identity::records::ProvisioningState;

/// Snapshot of an issuer's verifying key + lifecycle state at the time
/// the registry was queried. Pass to [`super::EnvelopeVerifier::verify`].
///
/// Fields are private — out-of-crate callers cannot construct or mutate
/// a [`ResolvedIssuer`] except via [`super::resolve::resolve_issuer`],
/// preventing forged snapshots that would let a malicious in-process
/// caller mint a `VerifiedSignedIntent` with attacker-chosen identity
/// or key bytes.
///
/// # Per-envelope freshness contract — load-bearing
///
/// Snapshots **must** be obtained immediately before each
/// `EnvelopeVerifier::verify` call. The verifier trusts every field on
/// the snapshot — `key_version`, `verifying_key`, `state` — and never
/// re-checks the registry. Reusing a snapshot across envelopes (e.g.,
/// caching it on a long-lived adapter handle) is **unsafe**: a rotation
/// or revocation between resolve and verify would leave the verifier
/// accepting envelopes signed by a now-superseded private key.
///
/// To make reuse hard to do by accident,
/// [`super::EnvelopeVerifier::verify`] consumes `ResolvedIssuer` by
/// value — every verification requires a fresh snapshot.
///
/// The recommended pattern is the resolve-then-verify pair:
///
/// ```ignore
/// let resolved = cairn_core::verifier::resolve_issuer(&registry, &id, kv).await?;
/// let verified = verifier.verify(intent, resolved)?;
/// ```
///
/// # Known race window — closed by issue #55 (WAL state machine)
///
/// Even with single-use snapshots, a rotation or revocation that lands
/// **between** `resolve_issuer` (at time T0) and the eventual WAL
/// commit (at T1) creates a TOCTOU window in which an envelope signed
/// by stale-but-then-current authority can be minted into a
/// `VerifiedSignedIntent` and reach WAL prepare. Per spec design, full
/// closure requires re-checking identity state inside the WAL
/// transaction itself (issue #55, §5.6 step PREPARE). The verifier's
/// guarantee here is: "this envelope was authentic at T0 against the
/// registry snapshot". The WAL state machine at T1 owns the freshness
/// re-check before COMMIT.
pub struct ResolvedIssuer {
    pub(crate) identity: Identity,
    pub(crate) key_version: KeyVersion,
    pub(crate) verifying_key: VerifyingKey,
    pub(crate) state: ProvisioningState,
}

impl ResolvedIssuer {
    /// Construct a [`ResolvedIssuer`] from the registry-row primitives.
    /// Only callable inside cairn-core; downstream callers go through
    /// [`super::resolve::resolve_issuer`].
    #[must_use]
    pub(crate) fn from_registry_row(
        identity: Identity,
        key_version: KeyVersion,
        verifying_key: VerifyingKey,
        state: ProvisioningState,
    ) -> Self {
        Self {
            identity,
            key_version,
            verifying_key,
            state,
        }
    }

    /// Identity this snapshot describes.
    #[must_use]
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Key version represented by [`Self::verifying_key`].
    #[must_use]
    pub fn key_version(&self) -> KeyVersion {
        self.key_version
    }

    /// Public Ed25519 key bytes (already validated by
    /// [`ed25519_dalek::VerifyingKey::from_bytes`] at resolve time).
    #[must_use]
    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    /// Lifecycle state of the identity at lookup time. The verifier
    /// rejects anything other than [`ProvisioningState::Active`].
    #[must_use]
    pub fn state(&self) -> ProvisioningState {
        self.state
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl ResolvedIssuer {
    /// Test-only constructor. Only available behind `cfg(test)` and
    /// `feature = "test-helpers"`.
    #[must_use]
    pub fn for_test(
        identity: Identity,
        key_version: KeyVersion,
        verifying_key: VerifyingKey,
        state: ProvisioningState,
    ) -> Self {
        Self::from_registry_row(identity, key_version, verifying_key, state)
    }
}

impl std::fmt::Debug for ResolvedIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedIssuer")
            .field("identity", &self.identity)
            .field("key_version", &self.key_version)
            .field("verifying_key", &"<redacted>")
            .field("state", &self.state)
            .finish()
    }
}
