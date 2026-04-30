//! Identity purge: two-phase WAL tombstone that permanently erases identity
//! rows and key material from both stores (issue #50, D7; spec §3.10).
//!
//! # Algorithm
//!
//! 1. Acquire per-identity [`IdentityLockGuard`] (wait = true).
//! 2. Verify `.cairn/maintenance/purge-ack` exists and contains the target
//!    identity's wire form (trimmed).  If missing or mismatch →
//!    [`IdentityServiceError::PurgeAckMissing`].
//! 3. Idempotency: if the identity is already in `PurgePending` state skip
//!    `mark_purge_pending` so the function is re-entrant after a crash.
//! 4. `registry.mark_purge_pending(target, &ack, reason)` — transitions to
//!    `purge_pending`.
//! 5. For each version in `registry.list_keys(target)`: call
//!    `keystore.delete_keypair` (best-effort; `NotFound` is tolerated).
//! 6. For each pending rotation in `registry.list_pending_rotations(target)`:
//!    delete the keystore entry and the WAL row.
//! 7. `registry.finalise_purge(target)` — transitions to `purged`.
//! 8. Delete the purge-ack file (best-effort; log warning on failure).

use cairn_core::{
    contract::identity_registry::{
        IdentityVisibility, PurgeAcknowledgement, PurgeReason, RegistryError,
    },
    domain::identity::{Identity, keys::SecretHandle, records::ProvisioningState},
    error::identity::IdentityServiceError,
};

use super::{
    IdentityService,
    lock::{IdentityLockError, IdentityLockGuard},
};

/// Purge `target` from the vault (spec §3.10).
///
/// The caller must have written the target identity's wire form to
/// `.cairn/maintenance/purge-ack` before calling this function.
///
/// # Errors
///
/// - [`IdentityServiceError::PurgeAckMissing`] if the ack file is absent
///   or contains a different identity string.
/// - [`IdentityServiceError::Registry`] with [`RegistryError::NotFound`] if
///   the target identity does not exist.
/// - [`IdentityServiceError::IdentityLockBusy`] if the per-identity lock
///   cannot be acquired within 30 seconds.
/// - [`IdentityServiceError::Keystore`] or [`IdentityServiceError::Registry`]
///   on backend failures.
pub(super) async fn purge(
    svc: &IdentityService,
    target: &Identity,
    reason: PurgeReason,
) -> Result<(), IdentityServiceError> {
    // ── Step 1: acquire per-identity lock ─────────────────────────────────────
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

    // ── Step 2: verify purge-ack file ─────────────────────────────────────────
    let ack_path = cairn_dir.join("maintenance/purge-ack");
    let ack_valid = std::fs::read_to_string(&ack_path)
        .ok()
        .is_some_and(|contents| contents.trim() == target.as_str());

    if !ack_valid {
        return Err(IdentityServiceError::PurgeAckMissing);
    }

    // Construct the acknowledgement token now that the filesystem check passed.
    let ack = PurgeAcknowledgement::confirmed();

    // ── Step 3: idempotency guard — skip mark_purge_pending if already there ──
    let current_state = svc
        .registry
        .get_identity(target, IdentityVisibility::IncludingPurgePending)
        .await?
        .map(|r| r.provisioning_state);

    match current_state {
        Some(ProvisioningState::PurgePending) => {
            // Already in purge_pending — crash-resume path; skip step 4.
        }
        Some(ProvisioningState::Purged) => {
            // Already purged — idempotent success.
            return Ok(());
        }
        None => {
            return Err(IdentityServiceError::Registry(RegistryError::NotFound));
        }
        Some(_) => {
            // Normal forward path — mark purge pending.
            svc.registry
                .mark_purge_pending(target, &ack, reason)
                .await?;
        }
    }

    // ── Step 5: delete all key versions from keystore ────────────────────────
    let key_entries = svc.registry.list_keys(target).await?;
    for entry in &key_entries {
        let handle =
            SecretHandle::for_identity(svc.vault_id.clone(), target.clone(), entry.key_version);
        match svc.keystore.delete_keypair(&handle).await {
            Ok(()) | Err(cairn_core::contract::keystore::KeystoreError::NotFound) => {}
            Err(e) => return Err(IdentityServiceError::Keystore(e)),
        }
    }

    // ── Step 6: delete pending rotation WAL rows + their keystore entries ─────
    let pending_rotations = svc.registry.list_pending_rotations(target).await?;
    for (planned_version, _planned_handle) in pending_rotations {
        let handle =
            SecretHandle::for_identity(svc.vault_id.clone(), target.clone(), planned_version);
        match svc.keystore.delete_keypair(&handle).await {
            Ok(()) | Err(cairn_core::contract::keystore::KeystoreError::NotFound) => {}
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

    // ── Step 7: phase 2 — finalise purge ─────────────────────────────────────
    svc.registry.finalise_purge(target).await?;

    // ── Step 8: remove ack file (best-effort) ─────────────────────────────────
    // A failure here does not corrupt state — the purge is already committed.
    // If the file cannot be removed, the next run will re-verify that the
    // ack matches the (now-purged) identity and proceed idempotently.
    let _ = std::fs::remove_file(&ack_path);

    Ok(())
}

impl IdentityService {
    /// Purge `target` from the vault (spec §3.10).
    ///
    /// The operator must write the target identity's wire form to
    /// `.cairn/maintenance/purge-ack` before calling this method.
    /// If the file is absent or mismatched, returns
    /// [`IdentityServiceError::PurgeAckMissing`].
    ///
    /// Runs the two-phase WAL tombstone: `mark_purge_pending` → key eviction
    /// → `finalise_purge`.  The function is re-entrant: if it is called after
    /// a crash that left the identity in `PurgePending` state it resumes from
    /// step 5 without re-writing the WAL row.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityServiceError`] on missing ack, lock contention,
    /// keystore failures, or registry failures.
    pub async fn purge(
        &self,
        target: &Identity,
        reason: PurgeReason,
    ) -> Result<(), IdentityServiceError> {
        purge(self, target, reason).await
    }
}
