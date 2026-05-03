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
pub struct ResolvedIssuer {
    /// The identity this snapshot describes.
    pub identity: Identity,
    /// Key version represented by `verifying_key`.
    pub key_version: KeyVersion,
    /// Public Ed25519 key bytes already validated by
    /// [`ed25519_dalek::VerifyingKey::from_bytes`].
    pub verifying_key: VerifyingKey,
    /// Lifecycle state of the identity at lookup time. Verifier rejects
    /// anything other than [`ProvisioningState::Active`].
    pub state: ProvisioningState,
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
