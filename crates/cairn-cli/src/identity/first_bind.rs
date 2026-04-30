//! First-bind commit: persist the signed witness file and promote the WAL row
//! from `pending` to `active` (issue #50, D3+).
//!
//! The six-step sequence implemented here matches spec §3.7 exactly. Every
//! intermediate file write is fsync'd so that a crash at any point leaves
//! enough artefacts on disk for the `finalise-binding` recovery command (D8)
//! to resume without data loss.

use std::{
    fs::{self, File},
    path::Path,
};

use rand_core::RngCore as _;

use cairn_core::{
    contract::{
        identity_registry::IdentityRegistry,
        keystore::{Keystore, KeystoreError},
    },
    domain::identity::keys::{SecretHandle, VaultId, WitnessHash},
    error::identity::IdentityServiceError,
};

use cairn_core::domain::identity::provision::ProvisioningPlan;

use super::lock::VaultBindingLock;

/// Commit a new first-bind atomically, following spec §3.7.
///
/// # Sequence
///
/// 0. Acquire `.cairn/vault.binding.lock` (exclusive, 30 s timeout).
/// 1. Namespace probe via `keystore.load_secret(&SecretHandle::for_witness(vault_id))`.
///    - `NotFound` → unclaimed, proceed.
///    - `Ok(_)` → return [`IdentityServiceError::VaultNamespaceClaimed`].
///    - `Locked | PermissionDenied` → return [`IdentityServiceError::Keystore`]
///      with [`KeystoreError::Locked`].
///    - Other errors → propagate.
/// 2. Generate 32 random witness bytes via `OsRng`. Write to
///    `<vault>/.cairn/vault.binding.pending`. fsync.
///    Compute `WitnessHash::from_witness(&witness_bytes)`.
/// 3. `keystore.store_secret(witness_handle, &witness_bytes)`.
/// 4. `registry.reserve_first_identity(...)` — validates the file's SHA-256
///    matches `witness_hash` inside the transaction.
/// 5. Write `<vault>/.cairn/vault.binding` with the witness hash (32 bytes).
///    fsync. Then remove `vault.binding.pending`. Order: write final, fsync,
///    remove pending — a crash leaves an extra recoverable file rather than
///    nothing.
/// 6. `keystore.store_keypair(&plan.secret_handle, &plan.signing_key)`.
/// 7. `registry.activate_identity(&plan.identity.id, plan.key_entry.key_version)`.
///
/// # Errors
///
/// - [`IdentityServiceError::VaultNamespaceClaimed`] — witness already exists
///   in the keystore; another process owns this vault namespace.
/// - [`IdentityServiceError::Keystore`] — any underlying keystore failure.
/// - [`IdentityServiceError::Registry`] — any underlying registry failure.
pub async fn commit_first_identity(
    vault_path: &Path,
    vault_id: VaultId,
    plan: ProvisioningPlan,
    registry: &dyn IdentityRegistry,
    keystore: &dyn Keystore,
) -> Result<(), IdentityServiceError> {
    // Ensure .cairn/ exists before any file operations.
    let cairn_dir = vault_path.join(".cairn");
    fs::create_dir_all(&cairn_dir)
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;

    // ── Acquire advisory exclusive lock (30 s timeout) ───────────────────────
    // `_lock` is intentionally named so it stays alive for the full scope.
    let _lock = VaultBindingLock::acquire(&cairn_dir)
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;

    // ── Step 0: namespace-ownership probe ────────────────────────────────────
    let witness_handle = SecretHandle::for_witness(vault_id.clone());
    match keystore.load_secret(&witness_handle).await {
        Err(KeystoreError::NotFound) => { /* unclaimed — proceed */ }
        Ok(_) => {
            return Err(IdentityServiceError::VaultNamespaceClaimed { vault_id });
        }
        Err(KeystoreError::Locked | KeystoreError::PermissionDenied) => {
            return Err(IdentityServiceError::Keystore(KeystoreError::Locked));
        }
        Err(e) => return Err(IdentityServiceError::Keystore(e)),
    }

    // ── Step 1: generate witness bytes + write .cairn/vault.binding.pending ──
    let mut witness = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut witness);

    let pending_path = cairn_dir.join("vault.binding.pending");
    fs::write(&pending_path, witness)
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;
    // fsync the pending file so the witness bytes survive a crash.
    File::open(&pending_path)
        .and_then(|f| f.sync_all())
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;
    // fsync the parent directory so the new dirent itself is durable —
    // file-content fsync alone doesn't guarantee the directory entry survives
    // a power loss on POSIX.
    fsync_dir(&cairn_dir)?;

    let witness_hash = WitnessHash::from_witness(&witness);

    // ── Step 2: store witness bytes in the keystore ───────────────────────────
    keystore.store_secret(&witness_handle, &witness).await?;

    // ── Step 3: registry.reserve_first_identity (validates witness hash) ──────
    registry
        .reserve_first_identity(
            &vault_id,
            &plan.identity,
            &plan.key_entry,
            witness_hash,
            &pending_path,
        )
        .await?;

    // ── Step 4: write vault.binding (hash only) then remove .pending ──────────
    // Write the final binding file BEFORE removing the pending sentinel so that
    // a crash between these two operations leaves both files rather than neither.
    let binding_path = cairn_dir.join("vault.binding");
    fs::write(&binding_path, witness_hash.as_bytes())
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;
    File::open(&binding_path)
        .and_then(|f| f.sync_all())
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;
    fsync_dir(&cairn_dir)?;
    fs::remove_file(&pending_path)
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;
    fsync_dir(&cairn_dir)?;

    // ── Step 5: store the identity keypair ────────────────────────────────────
    keystore
        .store_keypair(&plan.secret_handle, &plan.signing_key)
        .await?;

    // ── Step 6: activate the identity in the registry ─────────────────────────
    registry
        .activate_identity(&plan.identity.id, plan.key_entry.key_version)
        .await?;

    Ok(())
}

/// fsync the directory at `dir` so its dirent changes (file creates, removes,
/// renames) are durable across crashes. POSIX file-content fsync does NOT
/// imply directory metadata durability; without this, a crash between two
/// sentinel transitions can leave neither file present.
fn fsync_dir(dir: &std::path::Path) -> Result<(), IdentityServiceError> {
    use std::fs::File;
    File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))
}
