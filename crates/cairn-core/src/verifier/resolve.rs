//! [`resolve_issuer`] — async helper that turns
//! `(IdentityRegistry, Identity, KeyVersion)` into a [`super::ResolvedIssuer`]
//! ready for the synchronous [`super::EnvelopeVerifier::verify`].
//!
//! # Freshness model — load-bearing
//!
//! `resolve_issuer` answers a single question: at the moment the
//! registry was queried, what was the issuer's authentication state and
//! what was the registered verifying key for the requested
//! `key_version`? It is **not** a fresh-state authorization check. The
//! TOCTOU re-check below narrows the window where the resolver itself
//! could observe an inconsistent state-vs-key snapshot, but a revocation
//! or rotation that lands after `resolve_issuer` returns and before the
//! state-changing consumer commits is structurally **not** in scope
//! here. The architectural split is intentional, per spec §5.6:
//!
//! - **Verifier (this module + EnvelopeVerifier):** authentication —
//!   "the envelope was signed by the holder of the registered private
//!   key for this issuer, at the time the registry was read."
//!
//! - **WAL state machine (issue #55):** fresh-state authorization —
//!   "this issuer is *still* permitted to write at PREPARE/COMMIT time."
//!   The WAL transaction re-reads identity state inside its own
//!   transaction and rejects on any change since the verified envelope
//!   was minted. That is where the resolve-to-write race window is
//!   closed; doing it here would conflate authn with authz and force
//!   the verifier to be async, breaking spec decision Q1.

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
#[allow(
    clippy::too_many_lines,
    reason = "linear pipeline (identity → keys → superseded check → TOCTOU re-check → \
              decode key) is best read top-to-bottom; splitting helpers would obscure the \
              ordering invariants comment authors are documenting per-step"
)]
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
            tracing::error!(
                identity = %identity,
                error = ?e,
                "resolve_issuer: registry get_identity failed"
            );
            return Err(DomainError::Unauthorized {
                message: "registry unavailable".into(),
            });
        }
    };

    // 2. Find the key row for the requested version.
    let keys = match registry.list_keys(identity).await {
        Ok(keys) => keys,
        Err(RegistryError::NotFound) => {
            return Err(DomainError::KeyVersionMismatch {
                id: identity.clone(),
                intent: key_version,
                current: None,
            });
        }
        Err(e) => {
            tracing::error!(
                identity = %identity,
                error = ?e,
                "resolve_issuer: registry list_keys failed"
            );
            return Err(DomainError::Unauthorized {
                message: "registry unavailable".into(),
            });
        }
    };
    let Some(key_row) = keys.iter().find(|k| k.key_version == key_version) else {
        return Err(DomainError::KeyVersionMismatch {
            id: identity.clone(),
            intent: key_version,
            current: Some(record.current_key_version),
        });
    };

    // 2a. Reject superseded keys. After rotation, prior key rows are kept
    //     in `identity_keys` for audit but are signed off with
    //     `superseded_at = Some(_)` — accepting them here would let
    //     envelopes signed by a rotated-away private key still verify.
    //     Belt-and-braces: also require the requested version equal
    //     `current_key_version` so an out-of-band invariant violation
    //     (key row not marked superseded after rotation) still fails closed.
    if key_row.superseded_at.is_some() || key_row.key_version != record.current_key_version {
        return Err(DomainError::KeyVersionMismatch {
            id: identity.clone(),
            intent: key_version,
            current: Some(record.current_key_version),
        });
    }

    // 2b. TOCTOU re-check: the IdentityRegistry contract does not
    //     guarantee that `get_identity` and `list_keys` observe a single
    //     atomic snapshot of the registry. A concurrent revocation that
    //     lands after step 1 but before step 2 would leave us with a
    //     stale `Active` state, which the verifier would then trust.
    //     Re-read the identity row and reject if any field that affects
    //     authentication moved underneath us. Issue #55 closes the
    //     surviving resolve-to-WAL window at PREPARE time.
    let Ok(Some(record_after)) = registry
        .get_identity(identity, IdentityVisibility::IncludingPurgePending)
        .await
    else {
        tracing::error!(
            identity = %identity,
            "resolve_issuer: identity vanished or registry failed during TOCTOU re-check"
        );
        return Err(DomainError::Unauthorized {
            message: "registry unavailable".into(),
        });
    };
    if record_after.provisioning_state != record.provisioning_state
        || record_after.current_key_version != record.current_key_version
    {
        tracing::warn!(
            identity = %identity,
            before_state = ?record.provisioning_state,
            after_state = ?record_after.provisioning_state,
            before_key = %record.current_key_version,
            after_key = %record_after.current_key_version,
            "resolve_issuer: concurrent registry change detected — failing closed"
        );
        return Err(DomainError::Unauthorized {
            message: "concurrent registry change during resolve".into(),
        });
    }

    // 3. Decode the verifying key bytes.
    let verifying_key = VerifyingKey::from_bytes(&key_row.public_key).map_err(|e| {
        tracing::error!(
            identity = %identity,
            key_version = %key_version,
            error = ?e,
            "resolve_issuer: stored public_key bytes failed VerifyingKey::from_bytes"
        );
        DomainError::Unauthorized {
            message: "issuer key material is corrupt".into(),
        }
    })?;

    Ok(ResolvedIssuer::from_registry_row(
        identity.clone(),
        key_version,
        verifying_key,
        record.provisioning_state,
    ))
}
