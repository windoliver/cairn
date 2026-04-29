//! Identity revocation: two-phase WAL tombstone that evicts all key material
//! from the keystore and lands the identity in `Revoked` state
//! (issue #50, D7; spec §3.10).
//!
//! # Algorithm
//!
//! 1. Acquire per-identity [`IdentityLockGuard`] (wait = true).
//! 2. Load the signer's current `Active` row and signing key from the keystore.
//! 3. Build a [`RevocationReceipt`] signed with the signer's key.
//! 4. `registry.begin_revocation(&receipt)` — transitions target to
//!    `revoke_pending`.
//! 5. For each version in `registry.list_keys(target)`: call
//!    `keystore.delete_keypair` (best-effort; `NotFound` is tolerated).
//! 6. For each pending rotation in `registry.list_pending_rotations(target)`:
//!    delete the keystore entry and the WAL row.
//! 7. `registry.finalise_revocation(target)` — transitions to `revoked`.
//! 8. Return the receipt.

use chrono::Utc;

use cairn_core::{
    contract::identity_registry::{IdentityVisibility, RegistryError},
    domain::identity::{
        Identity,
        keys::SecretHandle,
        receipts::{ReceiptOpKind, ReceiptPayload, RevocationReceipt},
        records::{ProvisioningState, ReceiptId},
    },
    error::identity::IdentityServiceError,
};

use super::{
    IdentityService,
    lock::{IdentityLockError, IdentityLockGuard},
};

/// Revoke `target` using `signer`'s current signing key (spec §3.10).
///
/// Returns the committed [`RevocationReceipt`] on success.
///
/// When `signer == target` (self-revocation), the signing key is loaded
/// before `begin_revocation` writes the WAL row so it is still available.
///
/// # Errors
///
/// - [`IdentityServiceError::Registry`] with [`RegistryError::NotFound`] if
///   the target identity does not exist.
/// - [`IdentityServiceError::Registry`] with [`RegistryError::AlreadyRevoked`]
///   if the target is already revoked or in a non-active state.
/// - [`IdentityServiceError::IdentityLockBusy`] if the per-identity lock
///   cannot be acquired within 30 seconds.
/// - [`IdentityServiceError::Keystore`] or [`IdentityServiceError::Registry`]
///   on backend failures.
pub(super) async fn revoke(
    svc: &IdentityService,
    target: &Identity,
    signer: &Identity,
) -> Result<RevocationReceipt, IdentityServiceError> {
    // ── Step 1: acquire per-identity lock on target ───────────────────────────
    let cairn_dir = svc.vault_path.join(".cairn");
    let _lock =
        IdentityLockGuard::acquire(&cairn_dir, target.as_str(), true).map_err(|e| match e {
            IdentityLockError::Busy => {
                IdentityServiceError::IdentityLockBusy { id: target.clone() }
            }
            IdentityLockError::Io(io_err) => {
                IdentityServiceError::Registry(RegistryError::Backend(Box::new(io_err)))
            }
        })?;

    // ── Step 2: load target current row ──────────────────────────────────────
    let target_row = svc
        .registry
        .get_identity(target, IdentityVisibility::Operational)
        .await?
        .ok_or_else(|| IdentityServiceError::Registry(RegistryError::NotFound))?;

    if target_row.provisioning_state != ProvisioningState::Active {
        return Err(IdentityServiceError::Registry(
            RegistryError::AlreadyRevoked { id: target.clone() },
        ));
    }

    let target_key_version = target_row.current_key_version;

    // ── Step 2b: load signer current row + signing key ────────────────────────
    // For self-revocation (signer == target) we load the key *before*
    // begin_revocation so the key is still present.
    let signer_row = svc
        .registry
        .get_identity(signer, IdentityVisibility::Operational)
        .await?
        .ok_or_else(|| IdentityServiceError::Registry(RegistryError::NotFound))?;

    let signer_key_version = signer_row.current_key_version;
    let signer_handle =
        SecretHandle::for_identity(svc.vault_id.clone(), signer.clone(), signer_key_version);
    let signer_key = svc.keystore.load_signing_key(&signer_handle).await?;

    // ── Step 3: build + sign revocation receipt ───────────────────────────────
    let payload = ReceiptPayload {
        op_kind: ReceiptOpKind::Revocation,
        target: target.clone(),
        signer: signer.clone(),
        signer_key_version,
        old_key_version: Some(target_key_version),
        new_key_version: None,
        issued_at: Utc::now(),
    };

    let sig = payload
        .sign(&signer_key)
        .map_err(|e| IdentityServiceError::Registry(RegistryError::Backend(Box::new(e))))?;

    let receipt = RevocationReceipt {
        id: ReceiptId(0),
        payload,
        signature: sig.to_bytes().to_vec(),
        pending_key_disable: true,
    };

    // ── Step 4: phase 1 — write revoke_pending WAL row ────────────────────────
    svc.registry.begin_revocation(&receipt).await?;

    // ── Step 5: delete + verify-absent every keystore handle for target ──────
    delete_and_verify_target_keys(svc, target).await?;

    // ── Step 7: phase 2 — finalise revocation (only after read-back proves
    //              every targeted handle is absent) ──────────────────────────
    svc.registry.finalise_revocation(target).await?;

    Ok(receipt)
}

impl IdentityService {
    /// Revoke `target` using `signer`'s current signing key (spec §3.10).
    ///
    /// Runs the two-phase WAL tombstone: writes a `revoke_pending` WAL row,
    /// evicts all key material from the keystore, then transitions the
    /// identity to `Revoked`.
    ///
    /// For self-revocation pass `signer = target`.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityServiceError`] on lock contention, keystore failures,
    /// registry failures, or when the target is already revoked.
    pub async fn revoke(
        &self,
        target: &Identity,
        signer: &Identity,
    ) -> Result<RevocationReceipt, IdentityServiceError> {
        revoke(self, target, signer).await
    }
}

/// Delete every keystore handle associated with `target` and read-back verify
/// each one is absent before returning. Covers both the active key versions
/// (`identity_keys`) and any in-flight pending rotation handles.
///
/// Returns [`IdentityServiceError::KeyMaterialDesynchronized`] if any handle
/// still loads after the delete call — this signals a backend that lied about
/// success or a concurrent rewrite, and prevents the caller from advancing the
/// registry to a terminal state while signing material survives.
async fn delete_and_verify_target_keys(
    svc: &IdentityService,
    target: &Identity,
) -> Result<(), IdentityServiceError> {
    use cairn_core::contract::keystore::KeystoreError;

    // Active key versions.
    for entry in svc.registry.list_keys(target).await? {
        let handle =
            SecretHandle::for_identity(svc.vault_id.clone(), target.clone(), entry.key_version);
        match svc.keystore.delete_keypair(&handle).await {
            Ok(()) | Err(KeystoreError::NotFound) => {}
            Err(e) => return Err(IdentityServiceError::Keystore(e)),
        }
        match svc.keystore.load_signing_key(&handle).await {
            Err(KeystoreError::NotFound) => {}
            Ok(_) => {
                return Err(IdentityServiceError::KeyMaterialDesynchronized {
                    id: target.clone(),
                    reason: format!(
                        "revoke: keystore entry survived delete for key_version={}",
                        entry.key_version
                    ),
                });
            }
            Err(e) => return Err(IdentityServiceError::Keystore(e)),
        }
    }

    // Pending rotation handles.
    for (planned_version, _planned_handle) in svc.registry.list_pending_rotations(target).await? {
        let handle =
            SecretHandle::for_identity(svc.vault_id.clone(), target.clone(), planned_version);
        match svc.keystore.delete_keypair(&handle).await {
            Ok(()) | Err(KeystoreError::NotFound) => {}
            Err(e) => return Err(IdentityServiceError::Keystore(e)),
        }
        match svc.keystore.load_signing_key(&handle).await {
            Err(KeystoreError::NotFound) => {}
            Ok(_) => {
                return Err(IdentityServiceError::KeyMaterialDesynchronized {
                    id: target.clone(),
                    reason: format!(
                        "revoke: keystore entry survived delete for pending_rotation \
                         key_version={planned_version}"
                    ),
                });
            }
            Err(e) => return Err(IdentityServiceError::Keystore(e)),
        }
        match svc
            .registry
            .delete_pending_rotation(target, planned_version)
            .await
        {
            Ok(()) | Err(RegistryError::NotFound) => {}
            Err(e) => return Err(IdentityServiceError::Registry(e)),
        }
    }

    Ok(())
}
