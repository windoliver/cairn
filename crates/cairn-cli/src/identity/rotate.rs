//! Key rotation: mint a new Ed25519 keypair, persist it in the keystore, and
//! advance `current_key_version` in the registry (issue #50, D6).
//!
//! # Algorithm (spec §3.6)
//!
//! 1. Acquire per-identity [`IdentityLockGuard`] (wait = true).
//! 2. Insert `pending_rotation` WAL row (spec step 0a) so crashes mid-rotation
//!    can be detected and resumed.
//! 3. Generate a fresh Ed25519 signing key.
//! 4. Store the new keypair in the keystore.
//! 5. Read-back verify: load the new key and assert the public key matches.
//! 6. Sign a rotation receipt with the **old** signing key.
//! 7. Build the new [`IdentityKeyEntry`] with `signed_predecessor` attestation.
//! 8. CAS-apply the rotation via `registry.apply_rotation`.
//! 9. Evict the oldest key version from the keystore when the total
//!    per-identity key count exceeds [`MAX_KEY_HISTORY`] (currently 3).
//!
//! # N-history retention policy
//!
//! We retain at most [`MAX_KEY_HISTORY`] (`= 3`) key versions per identity.
//! This gives: the active key (`v_new`), its immediate predecessor
//! (`v_current`), and one additional predecessor for verification of recent
//! signatures.  Eviction begins only when the count exceeds this limit — so
//! the first two rotations produce no eviction, and the third triggers the
//! first eviction.

use chrono::Utc;

use cairn_core::{
    contract::identity_registry::{IdentityVisibility, RegistryError},
    domain::identity::{
        Identity,
        keys::{KeyVersion, SecretHandle, SigningKey},
        receipts::{ReceiptOpKind, ReceiptPayload, RotationReceipt},
        records::{IdentityKeyEntry, ProvisioningState, ReceiptId},
    },
    error::identity::IdentityServiceError,
};

use super::{
    IdentityService,
    lock::{IdentityLockError, IdentityLockGuard},
};

/// Maximum number of key versions retained per identity in the keystore.
///
/// When the total count exceeds this limit the oldest version is evicted.
/// A value of 3 means we keep: active, immediate predecessor, one older
/// predecessor (sufficient for recent-signature verification).
const MAX_KEY_HISTORY: usize = 3;

/// Rotate the signing key for `id` (spec §3.6).
///
/// Returns the committed [`RotationReceipt`] on success.
///
/// # Errors
///
/// - [`IdentityServiceError::Registry`] with [`RegistryError::NotFound`] if
///   `id` does not exist or is not [`ProvisioningState::Active`].
/// - [`IdentityServiceError::IdentityLockBusy`] if the per-identity lock
///   cannot be acquired within 30 seconds.
/// - [`IdentityServiceError::KeyMaterialDesynchronized`] if the new keypair
///   read-back does not match the stored public key.
/// - [`IdentityServiceError::Keystore`] or [`IdentityServiceError::Registry`]
///   on backend failures.
pub(super) async fn rotate(
    svc: &IdentityService,
    id: &Identity,
) -> Result<RotationReceipt, IdentityServiceError> {
    // ── Step 0: acquire per-identity lock ────────────────────────────────────
    let cairn_dir = svc.vault_path.join(".cairn");
    let _lock = IdentityLockGuard::acquire(&cairn_dir, id.as_str(), true).map_err(|e| match e {
        IdentityLockError::Busy => IdentityServiceError::IdentityLockBusy { id: id.clone() },
        IdentityLockError::Io(io_err) => {
            IdentityServiceError::Registry(RegistryError::Backend(Box::new(io_err)))
        }
    })?;

    // ── Step 0a: snapshot current state; verify Active ───────────────────────
    let current = svc
        .registry
        .get_identity(id, IdentityVisibility::Operational)
        .await?
        .ok_or_else(|| IdentityServiceError::Registry(RegistryError::NotFound))?;

    if current.provisioning_state != ProvisioningState::Active {
        return Err(IdentityServiceError::Registry(RegistryError::Backend(
            format!(
                "identity {} is not Active (state: {:?})",
                id.as_str(),
                current.provisioning_state
            )
            .into(),
        )));
    }

    let old_key_version = current.current_key_version;

    // ── Compute new key version ───────────────────────────────────────────────
    let new_key_version = old_key_version
        .next()
        .map_err(|e| IdentityServiceError::Registry(RegistryError::Backend(Box::new(e))))?;

    let new_handle = SecretHandle::for_identity(svc.vault_id.clone(), id.clone(), new_key_version);

    // ── Step 0a: insert pending_rotation WAL row ──────────────────────────────
    svc.registry
        .insert_pending_rotation(id, new_key_version, &new_handle.account_string())
        .await?;

    // ── Step 1: generate new signing key ──────────────────────────────────────
    let new_signing_key = SigningKey::generate(&mut rand_core::OsRng);

    // ── Step 2: store the new keypair in the keystore ─────────────────────────
    svc.keystore
        .store_keypair(&new_handle, &new_signing_key)
        .await?;

    // ── Step 3: read-back verify (with rollback on failure) ─────────────────
    verify_or_rollback(svc, id, new_key_version, &new_handle, &new_signing_key).await?;

    // ── Step 4: load OLD signing key + build + sign rotation receipt ──────────
    let old_handle = SecretHandle::for_identity(svc.vault_id.clone(), id.clone(), old_key_version);
    let old_signing_key = svc.keystore.load_signing_key(&old_handle).await?;

    let payload = ReceiptPayload {
        op_kind: ReceiptOpKind::Rotation,
        target: id.clone(),
        // Self-signed for now; cross-signing (§3.6 cross-authority) comes in D7+.
        signer: id.clone(),
        signer_key_version: old_key_version,
        old_key_version: Some(old_key_version),
        new_key_version: Some(new_key_version),
        issued_at: Utc::now(),
    };

    let sig = payload
        .sign(&old_signing_key)
        .map_err(|e| IdentityServiceError::Registry(RegistryError::Backend(Box::new(e))))?;

    // Set `pending_eviction` only when this rotation will push the retained
    // key count over MAX_KEY_HISTORY. Setting it unconditionally turns every
    // rotation into a follow-up keystore delete, which combined with the
    // adapter-side fix that maps eviction → old_key_version means repair
    // would correctly delete the predecessor — but on the first rotation the
    // total count is still ≤ MAX_KEY_HISTORY so no eviction is wanted.
    let existing_keys = svc.registry.list_keys(id).await?;
    let post_rotation_count = existing_keys.len() + 1; // current keys + the new row
    let needs_eviction = post_rotation_count > MAX_KEY_HISTORY;

    let receipt = RotationReceipt {
        // Placeholder rowid — `apply_rotation` returns the real id after
        // it commits the receipt row, which we substitute below.
        id: ReceiptId(0),
        payload,
        signature: sig.to_bytes().to_vec(),
        pending_eviction: needs_eviction,
    };

    // ── Step 5: build signed_predecessor attestation ──────────────────────────
    // Sign the new verifying key bytes with the OLD signing key, proving the
    // new key was authorized by the holder of the old key.
    let signed_predecessor = {
        let new_pub_bytes = new_signing_key.verifying_key().to_bytes();
        let pred_sig = old_signing_key.sign(&new_pub_bytes);
        Some(pred_sig.to_bytes().to_vec())
    };

    let new_key_entry = IdentityKeyEntry {
        identity_id: id.clone(),
        key_version: new_key_version,
        public_key: new_signing_key.verifying_key().to_bytes(),
        signed_predecessor,
        created_at: Utc::now(),
        superseded_at: None,
    };

    // ── Step 6: CAS-apply the rotation ───────────────────────────────────────
    // `apply_rotation` atomically:
    //   - advances current_key_version (CAS)
    //   - stamps predecessor's superseded_at
    //   - inserts the new identity_keys row
    //   - deletes the pending_rotations WAL row (the one we inserted in step 0a)
    //   - inserts the identity_receipts row
    let receipt_id = svc
        .registry
        .apply_rotation(&receipt, old_key_version, &new_key_entry)
        .await?;

    // NOTE: `delete_pending_rotation` is intentionally NOT called here.
    // `apply_rotation` already deletes the pending_rotations row atomically
    // in the same transaction, so calling it again would return NotFound.

    // ── Step 7: evict eldest key if over MAX_KEY_HISTORY ─────────────────────
    evict_eldest_if_needed(svc, id, new_key_version).await?;

    Ok(RotationReceipt {
        id: receipt_id,
        ..receipt
    })
}

/// Read back the just-stored keypair and verify the round-tripped public key
/// matches what we generated. On any failure, best-effort delete the keystore
/// entry and the `pending_rotations` row before surfacing the error so a
/// follow-up rotation can retry cleanly. Without this rollback, a backend
/// that lied about `store_keypair` success would leave durable signing
/// material the registry does not point at and a `pending_rotations` row
/// whose unique key blocks every retry.
async fn verify_or_rollback(
    svc: &IdentityService,
    id: &Identity,
    new_key_version: KeyVersion,
    new_handle: &SecretHandle,
    new_signing_key: &SigningKey,
) -> Result<(), IdentityServiceError> {
    let reason: Option<String> = match svc.keystore.load_signing_key(new_handle).await {
        Ok(loaded) => {
            let loaded_pub = loaded.verifying_key().to_bytes();
            let expected_pub = new_signing_key.verifying_key().to_bytes();
            if loaded_pub == expected_pub {
                None
            } else {
                Some("new key read-back pubkey mismatch after store".to_owned())
            }
        }
        Err(e) => Some(format!("new key read-back load failed: {e}")),
    };
    if let Some(reason) = reason {
        let _ = svc.keystore.delete_keypair(new_handle).await;
        let _ = svc
            .registry
            .delete_pending_rotation(id, new_key_version)
            .await;
        return Err(IdentityServiceError::KeyMaterialDesynchronized {
            id: id.clone(),
            reason,
        });
    }
    Ok(())
}

/// Evict the oldest key version from the keystore when the per-identity key
/// count exceeds [`MAX_KEY_HISTORY`].
///
/// Eviction order: the version with the smallest version number (eldest) is
/// deleted first.  The pending-eviction WAL flag is cleared after the keystore
/// delete succeeds.
async fn evict_eldest_if_needed(
    svc: &IdentityService,
    id: &Identity,
    _new_key_version: KeyVersion,
) -> Result<(), IdentityServiceError> {
    let keys = svc.registry.list_keys(id).await?;

    if keys.len() <= MAX_KEY_HISTORY {
        return Ok(());
    }

    // Find the oldest version (smallest key_version number).
    let eldest = keys
        .iter()
        .min_by_key(|k| k.key_version.as_u32())
        .expect("invariant: keys is non-empty (checked above)");

    let eldest_version = eldest.key_version;
    let eldest_handle =
        SecretHandle::for_identity(svc.vault_id.clone(), id.clone(), eldest_version);

    // Delete from keystore.
    svc.keystore.delete_keypair(&eldest_handle).await?;

    // Clear the pending_eviction WAL flag for this version.
    let evictions = svc.registry.list_pending_evictions().await?;
    if let Some(entry) = evictions
        .iter()
        .find(|e| &e.identity == id && e.evict_version == eldest_version)
    {
        svc.registry
            .clear_pending_eviction(&entry.receipt_id)
            .await?;
    }

    Ok(())
}

impl IdentityService {
    /// Rotate the signing key for `id` (spec §3.6).
    ///
    /// Acquires the per-identity advisory lock, generates a fresh Ed25519
    /// keypair, stores it in the keystore, signs a rotation receipt with the
    /// old key, and CAS-applies the rotation in the registry.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityServiceError`] on lock contention, keystore failures,
    /// registry failures, or key-material desynchronisation.
    pub async fn rotate(&self, id: &Identity) -> Result<RotationReceipt, IdentityServiceError> {
        rotate(self, id).await
    }
}
