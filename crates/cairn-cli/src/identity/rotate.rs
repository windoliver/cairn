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

    // ── Step 3: read-back verify ──────────────────────────────────────────────
    let loaded = svc.keystore.load_signing_key(&new_handle).await?;
    let loaded_pub = loaded.verifying_key().to_bytes();
    let expected_pub = new_signing_key.verifying_key().to_bytes();

    if loaded_pub != expected_pub {
        return Err(IdentityServiceError::KeyMaterialDesynchronized {
            id: id.clone(),
            reason: "new key read-back pubkey mismatch after store".to_owned(),
        });
    }

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

    let receipt = RotationReceipt {
        // Placeholder rowid — `apply_rotation` inserts the actual row and
        // returns via `list_pending_evictions` after the fact.
        id: ReceiptId(0),
        payload,
        signature: sig.to_bytes().to_vec(),
        pending_eviction: true,
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
    svc.registry
        .apply_rotation(&receipt, old_key_version, &new_key_entry)
        .await?;

    // NOTE: `delete_pending_rotation` is intentionally NOT called here.
    // `apply_rotation` already deletes the pending_rotations row atomically
    // in the same transaction, so calling it again would return NotFound.

    // ── Step 7: evict eldest key if over MAX_KEY_HISTORY ─────────────────────
    evict_eldest_if_needed(svc, id, new_key_version).await?;

    // Build the final receipt with the actual receipt rowid from the DB.
    // Query via list_pending_evictions to find our receipt.
    let actual_receipt = resolve_receipt(svc, id, new_key_version, receipt).await?;

    Ok(actual_receipt)
}

/// Resolve the actual `ReceiptId` assigned by the DB for the rotation receipt
/// we just applied.  We query `list_pending_evictions` and find the entry
/// whose `evict_version` is the old version for this identity.
///
/// If no matching eviction entry is found (e.g., `pending_eviction = false`
/// was set by the adapter), we fall back to `ReceiptId(0)`.
async fn resolve_receipt(
    svc: &IdentityService,
    id: &Identity,
    new_key_version: KeyVersion,
    placeholder: RotationReceipt,
) -> Result<RotationReceipt, IdentityServiceError> {
    let evictions = svc.registry.list_pending_evictions().await?;
    // The eviction entry for this rotation has evict_version = old_key_version
    // (the version we just replaced).
    let old_version = placeholder
        .payload
        .old_key_version
        .unwrap_or(KeyVersion::FIRST);

    if let Some(entry) = evictions
        .iter()
        .find(|e| &e.identity == id && e.evict_version == old_version)
    {
        Ok(RotationReceipt {
            id: entry.receipt_id.clone(),
            ..placeholder
        })
    } else {
        // No pending eviction entry — the adapter may not have written one
        // (e.g., key count was below MAX_KEY_HISTORY and eviction was not
        // needed yet).  Return the placeholder with id=0 as a best-effort.
        let _ = new_key_version; // silence unused-variable lint
        Ok(placeholder)
    }
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
