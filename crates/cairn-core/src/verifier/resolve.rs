//! [`resolve_issuer`] — async helper that turns
//! `(IdentityRegistry, Identity, KeyVersion)` into a [`super::ResolvedIssuer`]
//! ready for the synchronous [`super::EnvelopeVerifier::verify`].

use ed25519_dalek::VerifyingKey;

use crate::contract::identity_registry::{IdentityRegistry, IdentityVisibility, RegistryError};
use crate::domain::DomainError;
use crate::domain::Identity;
use crate::domain::identity::keys::KeyVersion;

use super::ResolvedIssuer;

/// Resolve `(identity, key_version)` against the registry, returning a
/// snapshot suitable for [`super::EnvelopeVerifier::verify`].
///
/// # Errors
/// - [`DomainError::Unauthorized`] if `identity` is unknown to the registry.
/// - [`DomainError::KeyVersionMismatch`] if no key row exists at
///   `key_version`.
/// - [`DomainError::Unauthorized`] for opaque registry backend errors.
pub async fn resolve_issuer(
    registry: &dyn IdentityRegistry,
    identity: &Identity,
    key_version: KeyVersion,
) -> Result<ResolvedIssuer, DomainError> {
    // 1. Look up the identity row. Use IncludingPurgePending so the
    //    verifier returns a precise lifecycle error rather than NotFound
    //    for non-Active states.
    let record = match registry
        .get_identity(identity, IdentityVisibility::IncludingPurgePending)
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err(DomainError::Unauthorized {
                message: format!("identity {identity} not in registry"),
            });
        }
        Err(e) => {
            return Err(DomainError::Unauthorized {
                message: format!("registry get_identity failed: {e}"),
            });
        }
    };

    // 2. Find the key row for the requested version.
    let keys = match registry.list_keys(identity).await {
        Ok(keys) => keys,
        Err(RegistryError::NotFound) => {
            return Err(DomainError::KeyVersionMismatch {
                intent: key_version,
                current: None,
            });
        }
        Err(e) => {
            return Err(DomainError::Unauthorized {
                message: format!("registry list_keys failed: {e}"),
            });
        }
    };
    let Some(key_row) = keys.iter().find(|k| k.key_version == key_version) else {
        return Err(DomainError::KeyVersionMismatch {
            intent: key_version,
            current: Some(record.current_key_version),
        });
    };

    // 3. Decode the verifying key bytes.
    let verifying_key =
        VerifyingKey::from_bytes(&key_row.public_key).map_err(|e| DomainError::Unauthorized {
            message: format!("registry public_key bytes invalid: {e}"),
        })?;

    Ok(ResolvedIssuer::from_registry_row(
        identity.clone(),
        key_version,
        verifying_key,
        record.provisioning_state,
    ))
}
