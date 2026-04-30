//! Identity recovery: repair, reconcile, finalise-binding, vault-id-recover.
//!
//! These four commands cover the common crash states described in spec §3.7
//! (first-bind crash recovery) and §3.10 (per-identity state-machine repair).
//! Less-common crash states (keystore-witness-only recovery, simultaneous
//! partial crashes across multiple subsystems) are documented as TODO follow-ups
//! and return typed errors rather than silently guessing.
//!
//! # Coverage of the §3.7 crash matrix
//!
//! | Crash state                                                 | This module        |
//! |-------------------------------------------------------------|--------------------|
//! | `.pending` exists, `.binding` absent — abandon             | `finalise_binding` |
//! | `.pending` exists, `.binding` absent — resume, DB consistent | `finalise_binding` |
//! | `.binding` exists, DB absent                               | partial (error)    |
//! | Keystore-witness-only (no DB, no files)                    | TODO follow-up     |

use std::{
    collections::HashSet,
    fs::{self, File},
    path::PathBuf,
};

use cairn_core::{
    contract::{
        identity_registry::{IdentityRegistry as _, IdentityVisibility, RegistryError},
        keystore::{Keystore as _, KeystoreError},
    },
    domain::identity::{Identity, keys::SecretHandle, keys::VaultId},
    error::identity::IdentityServiceError,
};
use cairn_store_sqlite::SqliteIdentityRegistry;

use super::{
    IdentityService,
    lock::{IdentityLockError, IdentityLockGuard},
};

// ── repair ────────────────────────────────────────────────────────────────────

/// Per-identity state-machine reconciliation (spec §3.10).
///
/// Sweeps all pending rows, pending evictions, and pending key-disable rows
/// for `id` and removes orphaned or mismatched entries from both the registry
/// and the keystore.  No signed envelope is produced; this command only
/// rebalances the state machine.
///
/// # Crash cases handled
///
/// - Orphan pending rows (keystore `NotFound`) → deleted from registry.
/// - Pubkey-mismatched pending rows (keystore key present but hash differs)
///   → both the registry row and the keystore entry are removed.
/// - Pending eviction rows → keystore `delete_keypair` (best-effort, tolerates
///   `NotFound`) + `clear_pending_eviction`.
/// - Pending key-disable rows → keystore `delete_keypair` for each retained
///   version (best-effort) + `clear_pending_key_disable`.
///
/// Healthy pending rows (key present and pubkey matches) are left untouched —
/// the caller may re-issue `activate` or run `provision` to advance them.
///
/// Active identity with key missing from keystore ("desynchronised active") is
/// a no-op in this command — use `rotate` to recover that state.
///
/// # Errors
///
/// Returns [`IdentityServiceError::IdentityLockBusy`] if another operation
/// holds the per-identity lock, or [`IdentityServiceError::Registry`] /
/// [`IdentityServiceError::Keystore`] on backend failures.
#[allow(
    clippy::too_many_lines,
    reason = "sequential steps of the §3.10 spec algorithm"
)]
pub(super) async fn repair(
    svc: &IdentityService,
    id: &Identity,
) -> Result<(), IdentityServiceError> {
    let cairn_dir = svc.vault_path.join(".cairn");

    // ── Step 1: acquire per-identity lock ─────────────────────────────────────
    let _lock = IdentityLockGuard::acquire(&cairn_dir, id.as_str(), true).map_err(|e| match e {
        IdentityLockError::Busy => IdentityServiceError::IdentityLockBusy { id: id.clone() },
        IdentityLockError::Io(io_err) => {
            IdentityServiceError::Registry(RegistryError::Backend(Box::new(io_err)))
        }
    })?;

    // ── Step 2: sweep pending provisioning rows for this identity ─────────────
    let pending_rows = svc.registry.list_pending_by_identity(id).await?;
    for pending in pending_rows {
        let handle = SecretHandle::for_identity(
            svc.vault_id.clone(),
            pending.identity.clone(),
            pending.key_version,
        );
        match svc.keystore.load_signing_key(&handle).await {
            Err(KeystoreError::NotFound) => {
                // Orphan pending row — no key material exists.  Delete the
                // dangling registry row so a fresh provision can succeed.
                match svc
                    .registry
                    .delete_pending(&pending.identity, pending.key_version)
                    .await
                {
                    Ok(()) | Err(RegistryError::NotFound) => {}
                    Err(e) => return Err(IdentityServiceError::Registry(e)),
                }
            }
            Ok(sk) => {
                let actual_pub = sk.verifying_key().to_bytes();
                if actual_pub == pending.public_key {
                    // Healthy pending row — leave it for the caller to activate.
                } else {
                    // Pubkey mismatch — evict both the registry row and the
                    // keystore entry so a fresh provision can succeed.
                    match svc
                        .registry
                        .delete_pending(&pending.identity, pending.key_version)
                        .await
                    {
                        Ok(()) | Err(RegistryError::NotFound) => {}
                        Err(e) => return Err(IdentityServiceError::Registry(e)),
                    }
                    // Best-effort keystore eviction; ignore NotFound.
                    match svc.keystore.delete_keypair(&handle).await {
                        Ok(()) | Err(KeystoreError::NotFound) => {}
                        Err(e) => return Err(IdentityServiceError::Keystore(e)),
                    }
                }
            }
            Err(KeystoreError::Locked) => {
                // Keystore locked — truncate the sweep rather than hard-fail.
                break;
            }
            Err(e) => return Err(IdentityServiceError::Keystore(e)),
        }
    }

    // ── Step 3: active-row desync check (log-only, no mutation) ──────────────
    // An active identity without a keystore entry is a desynchronised state.
    // We do not mutate here; `rotate` is the correct recovery verb.
    if let Some(row) = svc
        .registry
        .get_identity(id, IdentityVisibility::Operational)
        .await?
    {
        use cairn_core::domain::identity::records::ProvisioningState;
        if row.provisioning_state == ProvisioningState::Active {
            let handle = SecretHandle::for_identity(
                svc.vault_id.clone(),
                id.clone(),
                row.current_key_version,
            );
            match svc.keystore.load_signing_key(&handle).await {
                Err(KeystoreError::NotFound | KeystoreError::Locked) | Ok(_) => {
                    // NotFound → desynchronised active (no mutation here).
                    // Locked   → cannot check; safe to skip.
                    // Ok(_)    → healthy; nothing to do.
                }
                Err(e) => return Err(IdentityServiceError::Keystore(e)),
            }
        }
    }

    // ── Step 4: sweep pending eviction rows for this identity ─────────────────
    //
    // Defense-in-depth: never delete the current_key_version even if a stale
    // receipt somehow points at it. The adapter-level fix (sourcing
    // evict_version from old_key_version) is the primary safeguard; this
    // guard catches future regressions.
    let current_active = svc
        .registry
        .get_identity(id, IdentityVisibility::Operational)
        .await?
        .map(|r| r.current_key_version);
    let evictions = svc.registry.list_pending_evictions().await?;
    for entry in evictions.iter().filter(|e| &e.identity == id) {
        if Some(entry.evict_version) == current_active {
            // Refuse to delete the active key. Leave the receipt's flag set
            // so a follow-up audit can investigate. Skip silently here.
            continue;
        }
        let handle =
            SecretHandle::for_identity(svc.vault_id.clone(), id.clone(), entry.evict_version);
        match svc.keystore.delete_keypair(&handle).await {
            Ok(()) | Err(KeystoreError::NotFound) => {}
            Err(e) => return Err(IdentityServiceError::Keystore(e)),
        }
        match svc.registry.clear_pending_eviction(&entry.receipt_id).await {
            Ok(()) | Err(RegistryError::NotFound) => {}
            Err(e) => return Err(IdentityServiceError::Registry(e)),
        }
    }

    // ── Step 5: sweep pending key-disable rows for this identity ──────────────
    let key_disables = svc.registry.list_pending_key_disables().await?;
    for entry in key_disables.iter().filter(|e| &e.identity == id) {
        for &retained_version in &entry.retained_versions {
            let handle =
                SecretHandle::for_identity(svc.vault_id.clone(), id.clone(), retained_version);
            match svc.keystore.delete_keypair(&handle).await {
                Ok(()) | Err(KeystoreError::NotFound) => {}
                Err(e) => return Err(IdentityServiceError::Keystore(e)),
            }
        }
        match svc
            .registry
            .clear_pending_key_disable(&entry.receipt_id)
            .await
        {
            Ok(()) | Err(RegistryError::NotFound) => {}
            Err(e) => return Err(IdentityServiceError::Registry(e)),
        }
    }

    Ok(())
}

// ── reconcile ─────────────────────────────────────────────────────────────────

/// Bulk version of [`repair`]: iterates all pending rows across all identities.
///
/// Collects the distinct identity set from `list_pending`,
/// `list_pending_evictions`, and `list_pending_key_disables`, then calls
/// `repair` for each identity.  Errors from individual repairs are returned
/// immediately (fail-fast).
///
/// # Errors
///
/// Returns the first [`IdentityServiceError`] encountered during any per-identity
/// repair.
pub(super) async fn reconcile(svc: &IdentityService) -> Result<(), IdentityServiceError> {
    let mut ids: HashSet<Identity> = HashSet::new();

    for entry in svc.registry.list_pending().await? {
        ids.insert(entry.identity);
    }
    for entry in svc.registry.list_pending_evictions().await? {
        ids.insert(entry.identity);
    }
    for entry in svc.registry.list_pending_key_disables().await? {
        ids.insert(entry.identity);
    }

    for id in ids {
        repair(svc, &id).await?;
    }

    Ok(())
}

// ── finalise_binding ──────────────────────────────────────────────────────────

/// Resume or abandon a partial `commit_first_identity` after a crash (spec §3.7).
///
/// This is a **static** function — it operates directly on the filesystem
/// and `SQLite` database without requiring an already-open [`IdentityService`].
/// That is intentional: the service may not be openable until the binding is
/// finalised.
///
/// # Crash states handled
///
/// | State                                                      | `abandon=false`              | `abandon=true`    |
/// |------------------------------------------------------------|------------------------------|-------------------|
/// | `.pending` exists, `.binding` absent, DB consistent        | rename `.pending` → `.binding` | delete `.pending` |
/// | `.pending` exists, `.binding` absent, DB has no `vault_meta` | error `PartialBindNeedsProvision` | delete `.pending` |
/// | `.binding` exists, DB has no `vault_meta`                    | error `PartialBindNeedsProvision` | n/a (binding kept)|
/// | Neither sentinel present                                   | `Ok(())` (nothing to do)     | `Ok(())`          |
///
/// The "keystore-witness-only" crash path (binding files absent AND DB absent,
/// but witness is in the OS keychain) is not handled here and returns
/// `PartialBindNeedsProvision` — a follow-up implementation task (TODO).
///
/// # Errors
///
/// - [`IdentityServiceError::PartialBindNeedsProvision`] when the crash left
///   an unrecoverable partial state that requires re-running `provision`.
/// - [`IdentityServiceError::Registry`] on `SQLite` backend failures.
/// - [`IdentityServiceError::Keystore`] on I/O errors writing binding files.
pub async fn finalise_binding(
    vault_path: PathBuf,
    abandon: bool,
    vault_id_override: Option<VaultId>,
) -> Result<(), IdentityServiceError> {
    let cairn_dir = vault_path.join(".cairn");
    let pending_path = cairn_dir.join("vault.binding.pending");
    let binding_path = cairn_dir.join("vault.binding");

    let pending_exists = pending_path.exists();
    let binding_exists = binding_path.exists();

    match (pending_exists, binding_exists) {
        (false, true) => {
            // `.binding` exists — check DB consistency.
            let db_path = cairn_dir.join("cairn.db");
            let registry = SqliteIdentityRegistry::open(&db_path)?;
            if registry.read_vault_meta().await?.is_none() {
                // Binding file present but DB never wrote vault_meta.
                // TODO: if `_vault_id_override` is provided and we can probe the
                // keystore for the witness, reconstruct vault_meta here.  For now,
                // surface a typed error so the caller can run `provision` again.
                return Err(IdentityServiceError::PartialBindNeedsProvision);
            }
            // Consistent state — nothing to do.
            Ok(())
        }
        (false, false) => {
            // No binding files at all — vault not yet bound.  Nothing to recover.
            Ok(())
        }
        (true, _) => {
            // `.pending` exists — crash occurred during first-bind.
            if abandon {
                // Removing only the pending sentinel while a keystore witness
                // remains under the same vault_id permanently claims the
                // namespace: future `provision` attempts hit
                // `VaultNamespaceClaimed` with no local recovery artifact.
                // Require an explicit vault_id (from DB vault_meta or
                // operator-supplied) so we can delete the keystore witness
                // first; otherwise fail closed.
                let db_path = cairn_dir.join("cairn.db");
                let db_vault_id = match SqliteIdentityRegistry::open(&db_path).ok() {
                    Some(r) => r.read_vault_meta().await.ok().flatten().map(|(id, _)| id),
                    None => None,
                };
                // If `reserve_first_identity` already committed `vault_meta`
                // and a pending identity row, abandoning here would orphan
                // committed registry state — the registry would still believe
                // the vault is bound while we destroy the recovery artifacts.
                // Refuse so the operator runs the resume path instead.
                if db_vault_id.is_some() {
                    return Err(IdentityServiceError::AbandonAfterCommit);
                }
                let target_vault_id = vault_id_override.or(db_vault_id);
                let Some(vault_id) = target_vault_id else {
                    // No way to identify the keystore witness — refuse abandon
                    // rather than orphan the namespace.
                    return Err(IdentityServiceError::AmbiguousVaultNamespaces);
                };

                // Best-effort delete the keystore witness for this vault_id.
                // Any error other than NotFound/Unsupported aborts abandon so
                // the operator can investigate.
                let keystore = cairn_keychain::OsKeystore::new(vault_id.clone());
                let witness_handle = SecretHandle::for_witness(vault_id);
                match keystore.delete_secret(&witness_handle).await {
                    Ok(()) | Err(KeystoreError::NotFound | KeystoreError::DiscoveryUnsupported) => {
                    }
                    Err(e) => return Err(IdentityServiceError::Keystore(e)),
                }

                fs::remove_file(&pending_path).map_err(|e| {
                    IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e)))
                })?;
                return Ok(());
            }

            // Resume path: check whether the DB has a consistent vault_meta.
            let db_path = cairn_dir.join("cairn.db");
            let registry = SqliteIdentityRegistry::open(&db_path)?;

            let Some((db_vault_id, db_witness_hash)) = registry.read_vault_meta().await? else {
                // DB has no vault_meta — crash was before reserve_first_identity.
                // Cannot resume without the original signing key material.
                return Err(IdentityServiceError::PartialBindNeedsProvision);
            };

            // Verify the pending file's bytes match the stored witness hash.
            let pending_bytes = fs::read(&pending_path)
                .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;

            // The witness must be exactly 32 bytes.
            let witness_arr: [u8; 32] = pending_bytes
                .try_into()
                .map_err(|_| IdentityServiceError::PartialBindNeedsProvision)?;

            let computed_hash =
                cairn_core::domain::identity::keys::WitnessHash::from_witness(&witness_arr);
            if computed_hash != db_witness_hash {
                // Content mismatch — cannot safely promote this pending file.
                return Err(IdentityServiceError::PartialBindNeedsProvision);
            }

            // Write the final binding file BEFORE removing the pending sentinel
            // so that a second crash still leaves one file rather than neither.
            fs::write(&binding_path, db_witness_hash.as_bytes())
                .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;
            File::open(&binding_path)
                .and_then(|f| f.sync_all())
                .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;

            // Remove the pending sentinel.
            fs::remove_file(&pending_path)
                .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;

            // Write vault.id for convenience (idempotent — only if absent).
            let vault_id_path = cairn_dir.join("vault.id");
            if !vault_id_path.exists() {
                fs::write(&vault_id_path, db_vault_id.as_str()).map_err(|e| {
                    IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e)))
                })?;
            }

            Ok(())
        }
    }
}

// ── vault_id_recover ──────────────────────────────────────────────────────────

/// Recover or reconstruct the `.cairn/vault.id` file (spec §3.7 / §4.5).
///
/// This is a **static** function — it does not require an already-open
/// [`IdentityService`].
///
/// # Algorithm
///
/// 0. If `.cairn/vault.binding.pending` exists, return an error — run
///    `finalise-binding` first.
/// 1. Read `vault_meta` from the `SQLite` registry. If present, write
///    `.cairn/vault.id` and rewrite `.cairn/vault.binding` with the stored
///    witness hash.  Return the recovered `VaultId`.
/// 2. If DB has no `vault_meta`, try fallbacks in order:
///
///    - **a.** If `probe_keychain`: call `keystore.list_vault_namespaces("cairn:")`.
///      - One result → use it.
///      - Multiple results → error [`IdentityServiceError::AmbiguousVaultNamespaces`].
///      - `DiscoveryUnsupported` → fall through.
///    - **b.** If `vault_id_override` is `Some`, write it and return it.
///    - **c.** Otherwise, error [`IdentityServiceError::VaultIdMissing`].
///
/// # Errors
///
/// - [`IdentityServiceError::FirstBindInProgress`] if `.cairn/vault.binding.pending`
///   exists (run `finalise-binding` first).
/// - [`IdentityServiceError::AmbiguousVaultNamespaces`] if the keychain probe
///   finds more than one vault namespace.
/// - [`IdentityServiceError::VaultIdMissing`] if no vault id can be determined.
/// - [`IdentityServiceError::Registry`] on `SQLite` backend failures.
/// - [`IdentityServiceError::Keystore`] on keystore or I/O failures.
pub async fn vault_id_recover(
    vault_path: PathBuf,
    probe_keychain: bool,
    vault_id_override: Option<VaultId>,
) -> Result<VaultId, IdentityServiceError> {
    let cairn_dir = vault_path.join(".cairn");

    // ── Step 0: pending-sentinel guard ────────────────────────────────────────
    if cairn_dir.join("vault.binding.pending").exists() {
        return Err(IdentityServiceError::FirstBindInProgress);
    }

    // ── Step 1: try reading vault_meta from the DB ─────────────────────────────
    let db_path = cairn_dir.join("cairn.db");
    if db_path.exists() {
        let registry = SqliteIdentityRegistry::open(&db_path)?;
        if let Some((db_vault_id, db_witness_hash)) = registry.read_vault_meta().await? {
            // Write (or overwrite) vault.id.
            fs::create_dir_all(&cairn_dir)
                .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;
            let vault_id_path = cairn_dir.join("vault.id");
            fs::write(&vault_id_path, db_vault_id.as_str())
                .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;

            // Rewrite vault.binding with the stored witness hash.
            let binding_path = cairn_dir.join("vault.binding");
            fs::write(&binding_path, db_witness_hash.as_bytes())
                .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;

            return Ok(db_vault_id);
        }
    }

    // ── Step 2a: probe keychain if requested ───────────────────────────────────
    //
    // SAFETY: a discovered namespace must NOT be auto-adopted on local
    // evidence alone — running recovery in a sibling directory could
    // otherwise silently bind this vault to an unrelated vault's keychain
    // namespace. Require either:
    //   (a) `.cairn/vault.binding` exists locally AND its bytes match the
    //       witness stored in the keystore for the discovered namespace, OR
    //   (b) operator passes `--vault-id <id>` explicitly.
    if probe_keychain {
        let probe_vault_id = VaultId::parse("00000000-0000-0000-0000-000000000000")
            .map_err(|e| IdentityServiceError::Registry(RegistryError::Backend(Box::new(e))))?;
        let probe_keystore = cairn_keychain::OsKeystore::new(probe_vault_id);
        match probe_keystore.list_vault_namespaces("cairn:").await {
            Ok(namespaces) => match namespaces.len() {
                0 => {
                    // No namespaces found; fall through to override.
                }
                1 => {
                    let candidate = namespaces
                        .into_iter()
                        .next()
                        .expect("invariant: len == 1 ⇒ first() is Some");
                    verify_and_adopt_namespace(&cairn_dir, candidate.clone()).await?;
                    return Ok(candidate);
                }
                _ => {
                    return Err(IdentityServiceError::AmbiguousVaultNamespaces);
                }
            },
            Err(KeystoreError::DiscoveryUnsupported) => {
                // Backend does not support enumeration — fall through.
            }
            Err(e) => return Err(IdentityServiceError::Keystore(e)),
        }
    }

    // ── Step 2b: caller-supplied override ─────────────────────────────────────
    //
    // SAFETY: an unverified `--vault-id` override could rebind this directory
    // to a different vault's keystore namespace (orphaning the real
    // identities and routing future operations to the wrong trust domain).
    // Require the same proof we require for `--probe-keychain`:
    //   the local `.cairn/vault.binding` exists (32 bytes) AND the keystore
    //   witness for `override_id` hashes to those bytes.
    if let Some(override_id) = vault_id_override {
        verify_and_adopt_namespace(&cairn_dir, override_id.clone()).await?;
        return Ok(override_id);
    }

    // ── Step 2c: nothing worked ────────────────────────────────────────────────
    Err(IdentityServiceError::VaultIdMissing)
}

/// Verify that `candidate` is the keystore namespace for the local vault and,
/// on success, write `.cairn/vault.id` to bind to it.
///
/// Requires:
///   1. `.cairn/vault.binding` exists locally and is exactly 32 bytes.
///   2. The keystore witness for `candidate` hashes to those 32 bytes.
///
/// Either condition failing returns [`IdentityServiceError::AmbiguousVaultNamespaces`]
/// — we never adopt a namespace on weaker evidence than a witness round-trip.
async fn verify_and_adopt_namespace(
    cairn_dir: &std::path::Path,
    candidate: VaultId,
) -> Result<(), IdentityServiceError> {
    let local_binding = cairn_dir.join("vault.binding");
    let local_hash: [u8; 32] = match fs::read(&local_binding) {
        Ok(b) if b.len() == 32 => b
            .as_slice()
            .try_into()
            .map_err(|_| IdentityServiceError::AmbiguousVaultNamespaces)?,
        _ => return Err(IdentityServiceError::AmbiguousVaultNamespaces),
    };
    let bound_keystore = cairn_keychain::OsKeystore::new(candidate.clone());
    let witness_handle =
        cairn_core::domain::identity::keys::SecretHandle::for_witness(candidate.clone());
    let witness_bytes = match bound_keystore.load_secret(&witness_handle).await {
        Ok(b) => b,
        Err(KeystoreError::NotFound) => {
            return Err(IdentityServiceError::AmbiguousVaultNamespaces);
        }
        Err(e) => return Err(IdentityServiceError::Keystore(e)),
    };
    let computed =
        cairn_core::domain::identity::keys::WitnessHash::from_witness(witness_bytes.as_slice());
    if computed.as_bytes() != &local_hash {
        return Err(IdentityServiceError::AmbiguousVaultNamespaces);
    }
    fs::create_dir_all(cairn_dir)
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;
    let vault_id_path = cairn_dir.join("vault.id");
    fs::write(&vault_id_path, candidate.as_str())
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;
    Ok(())
}

// ── IdentityService method wrappers ──────────────────────────────────────────

impl IdentityService {
    /// Per-identity state-machine reconciliation (spec §3.10).
    ///
    /// Sweeps orphaned and mismatched pending rows, pending evictions, and
    /// pending key-disable rows for `id`. Returns `Ok(())` on success or
    /// after best-effort cleanup.
    ///
    /// See the module-level documentation for the full crash-case table.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityServiceError::IdentityLockBusy`] if the per-identity
    /// lock is held, or [`IdentityServiceError::Registry`] /
    /// [`IdentityServiceError::Keystore`] on backend failures.
    pub async fn repair(&self, id: &Identity) -> Result<(), IdentityServiceError> {
        repair(self, id).await
    }

    /// Bulk reconciliation: calls [`Self::repair`] for every identity that has
    /// any outstanding pending rows, pending evictions, or pending key-disables.
    ///
    /// Errors are fail-fast — the first per-identity failure is returned and
    /// subsequent identities are not processed.
    ///
    /// # Errors
    ///
    /// Returns the first [`IdentityServiceError`] encountered.
    pub async fn reconcile(&self) -> Result<(), IdentityServiceError> {
        reconcile(self).await
    }
}
