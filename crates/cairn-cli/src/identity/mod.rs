//! `IdentityService` — orchestrator gluing `IdentityRegistry` + `Keystore`
//! (issue #50, spec §3.5 / §4.1).
//!
//! This module exposes a single [`IdentityService`] struct that:
//! - validates the `vault.id` file ↔ `vault_meta` consistency on open,
//! - runs the reconciliation sweep (keystore liveness check) in `open`, and
//! - delegates cryptographic storage to [`OsKeystore`] and durable identity
//!   state to [`SqliteIdentityRegistry`].

mod first_bind;
mod lock;
mod purge;
mod recover;
mod revoke;
mod rotate;
pub mod status;

use std::{path::PathBuf, sync::Arc};

use cairn_core::{
    contract::{
        identity_registry::{IdentityRegistry, IdentityVisibility, MaintenanceMode, RegistryError},
        keystore::{Keystore, KeystoreError},
    },
    domain::identity::{
        Identity, IdentityKind, ProvisioningState,
        keys::{SecretHandle, VaultId},
        provision::{ProvisionInput, build_provisioning_plan},
        records::IdentityKeyEntry,
    },
    error::identity::IdentityServiceError,
};
use cairn_keychain::OsKeystore;
use cairn_store_sqlite::SqliteIdentityRegistry;
use chrono::Utc;

pub use first_bind::commit_first_identity;
pub use status::ReconciliationReport;

/// Orchestrator for the identity subsystem (spec §3.5 / §4.1).
///
/// Binds a durable [`IdentityRegistry`] (backed by `SQLite`) to a secure
/// [`Keystore`] (backed by the OS keychain) and exposes the combined
/// lifecycle verbs: open, first-bind, rotate, revoke, purge, recover.
///
/// Obtain an instance via [`IdentityService::open`] (normal operation) or
/// [`IdentityService::open_for_maintenance`] (administrative access).
pub struct IdentityService {
    /// Absolute path to the vault root (the directory that contains `.cairn/`).
    pub vault_path: PathBuf,
    /// The unique identifier for this vault instance.
    pub vault_id: VaultId,
    /// Durable identity registry — all row-level state lives here.
    pub registry: Arc<dyn IdentityRegistry>,
    /// Secure key-material store.
    pub keystore: Arc<dyn Keystore>,
}

impl IdentityService {
    /// Open in issuer mode: runs the `vault.id` ↔ `vault_meta` consistency
    /// check **and** a full keystore reconciliation sweep before returning.
    ///
    /// # Errors
    /// - [`IdentityServiceError::VaultIdMissing`] when `.cairn/vault.id` is
    ///   absent or unreadable.
    /// - [`IdentityServiceError::VaultIdConflict`] when the file and the DB
    ///   disagree on the vault identifier.
    /// - [`IdentityServiceError::Registry`] on any `SQLite` backend failure.
    /// - [`IdentityServiceError::Keystore`] on any keystore backend failure
    ///   other than `NotFound` or `Locked`.
    pub async fn open(
        vault_path: PathBuf,
    ) -> Result<(Self, ReconciliationReport), IdentityServiceError> {
        // 1. Read .cairn/vault.id.
        let vault_id_path = vault_path.join(".cairn/vault.id");
        let file_id_str = std::fs::read_to_string(&vault_id_path)
            .map_err(|_| IdentityServiceError::VaultIdMissing)?;
        let file_id = VaultId::parse(file_id_str.trim())
            .map_err(|e| IdentityServiceError::Registry(RegistryError::Backend(Box::new(e))))?;

        // 2. Open SqliteIdentityRegistry.
        let db_path = vault_path.join(".cairn/cairn.db");
        let registry: Arc<dyn IdentityRegistry> = Arc::new(SqliteIdentityRegistry::open(&db_path)?);

        // 3. Compare file_id ↔ db vault_meta.
        let (db_id, _witness) = registry
            .read_vault_meta()
            .await?
            .ok_or(IdentityServiceError::VaultIdMissing)?;
        if file_id != db_id {
            return Err(IdentityServiceError::VaultIdConflict { file_id, db_id });
        }

        // 4. Build OsKeystore bound to vault_id.
        let keystore: Arc<dyn Keystore> = Arc::new(OsKeystore::new(file_id.clone()));

        // 5. Reconciliation sweep.
        let mut report = ReconciliationReport::default();

        // Pending rows: check keystore liveness.
        for pending in registry.list_pending().await? {
            let handle = SecretHandle::for_identity(
                file_id.clone(),
                pending.identity.clone(),
                pending.key_version,
            );
            match keystore.load_signing_key(&handle).await {
                Err(KeystoreError::NotFound) => {
                    // Orphan pending row — not yet vault-degrading; caller decides.
                }
                Ok(sk) => {
                    let actual_pub = sk.verifying_key().to_bytes();
                    if actual_pub != pending.public_key {
                        report.record_mismatch(pending.identity.clone());
                    }
                }
                Err(KeystoreError::Locked) => {
                    // Keystore locked — sweep incomplete; stop without marking degraded.
                    break;
                }
                Err(e) => return Err(IdentityServiceError::Keystore(e)),
            }
        }

        // Active rows: same liveness check.
        for active in registry
            .list_identities(None, IdentityVisibility::Operational)
            .await?
        {
            if active.provisioning_state != ProvisioningState::Active {
                continue;
            }
            let handle = SecretHandle::for_identity(
                file_id.clone(),
                active.id.clone(),
                active.current_key_version,
            );
            match keystore.load_signing_key(&handle).await {
                Err(KeystoreError::NotFound) => {
                    report.record_active_desync(active.id);
                }
                Ok(sk) => {
                    let pubkeys: Vec<IdentityKeyEntry> = registry.list_keys(&active.id).await?;
                    if let Some(current_key) = pubkeys
                        .iter()
                        .find(|k| k.key_version == active.current_key_version)
                        && sk.verifying_key().to_bytes() != current_key.public_key
                    {
                        report.record_active_mismatch(active.id);
                    }
                }
                Err(KeystoreError::Locked) => break,
                Err(e) => return Err(IdentityServiceError::Keystore(e)),
            }
        }

        Ok((
            Self {
                vault_path,
                vault_id: file_id,
                registry,
                keystore,
            },
            report,
        ))
    }

    /// Construct an `IdentityService` directly from its parts.
    ///
    /// This constructor is intentionally not part of the public production API —
    /// it exists so that integration tests in `cairn-cli/tests/` can inject a
    /// [`MemoryKeystore`](cairn_test_fixtures::MemoryKeystore) and an
    /// in-memory [`SqliteIdentityRegistry`] without touching the OS keychain
    /// or writing files.
    ///
    /// Prefer [`IdentityService::open`] or [`IdentityService::open_for_maintenance`]
    /// in all non-test call sites.
    #[doc(hidden)]
    pub fn new_for_test(
        vault_path: std::path::PathBuf,
        vault_id: VaultId,
        registry: Arc<dyn IdentityRegistry>,
        keystore: Arc<dyn Keystore>,
    ) -> Self {
        Self {
            vault_path,
            vault_id,
            registry,
            keystore,
        }
    }

    /// Provision an identity, with self-healing reconciliation (spec §3.5).
    ///
    /// # Algorithm
    ///
    /// 1. If `vault_meta` is absent this is the **first identity**.  Build a
    ///    [`ProvisioningPlan`](cairn_core::domain::identity::provision::ProvisioningPlan)
    ///    and delegate to [`commit_first_identity`] which runs the full §3.7
    ///    six-step sequence.
    /// 2. Otherwise, for a non-first identity, sweep pending rows for
    ///    `input.id`:
    ///    - Key found in keystore and pubkey matches → pending was stalled
    ///      at the activate step; call `activate_identity` and return.
    ///    - Key absent in keystore (`NotFound`) → orphan; delete the pending
    ///      row and continue.
    ///    - Key found but pubkey mismatches → corrupt entry; delete pending
    ///      row and keystore entry, continue.
    /// 3. Check whether the identity is already `Active` (idempotent guard).
    /// 4. Fresh provision: reserve → store keypair → activate.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityServiceError`] for registry failures, keystore
    /// failures, or vault-consistency violations.
    pub async fn provision(
        &self,
        // `_kind` is accepted for API symmetry with other identity verbs, but
        // the kind is already encoded in `input.id` / `input.kind`.
        _kind: IdentityKind,
        input: ProvisionInput,
        rng: &mut impl rand_core::CryptoRngCore,
    ) -> Result<Identity, IdentityServiceError> {
        let target_id = input.id.clone();

        // ── Step 1: check whether first-bind has happened ────────────────────
        if self.registry.read_vault_meta().await?.is_none() {
            // This is THE first identity; delegate to the full §3.7 sequence.
            let plan = build_provisioning_plan(input, rng, Utc::now());
            commit_first_identity(
                &self.vault_path,
                self.vault_id.clone(),
                plan,
                &*self.registry,
                &*self.keystore,
            )
            .await?;
            return Ok(target_id);
        }

        // ── Step 2: self-healing sweep of pending rows ────────────────────────
        let pending_rows = self.registry.list_pending_by_identity(&target_id).await?;

        for pending in pending_rows {
            let handle = SecretHandle::for_identity(
                self.vault_id.clone(),
                pending.identity.clone(),
                pending.key_version,
            );
            match self.keystore.load_signing_key(&handle).await {
                Ok(sk) => {
                    let actual_pub = sk.verifying_key().to_bytes();
                    if actual_pub == pending.public_key {
                        // Key is valid and matches — the activate step was the
                        // only missing piece.  Activate now and return.
                        self.registry
                            .activate_identity(&pending.identity, pending.key_version)
                            .await?;
                        return Ok(target_id);
                    }
                    // Pubkey mismatch — corrupt entry; evict both sides.
                    self.registry
                        .delete_pending(&pending.identity, pending.key_version)
                        .await?;
                    // `delete_keypair` is a no-op if the handle is absent.
                    self.keystore.delete_keypair(&handle).await?;
                }
                Err(KeystoreError::NotFound) => {
                    // Orphan pending row — no key material.  Delete and continue.
                    self.registry
                        .delete_pending(&pending.identity, pending.key_version)
                        .await?;
                }
                Err(e) => return Err(IdentityServiceError::Keystore(e)),
            }
        }

        // ── Step 3: idempotency guard — already active? ──────────────────────
        if let Some(row) = self
            .registry
            .get_identity(&target_id, IdentityVisibility::Operational)
            .await?
            && row.provisioning_state == ProvisioningState::Active
        {
            return Ok(target_id);
        }

        // ── Step 4: fresh provision ───────────────────────────────────────────
        let plan = build_provisioning_plan(input, rng, Utc::now());
        let plan_id = plan.identity.id.clone();
        let plan_key_version = plan.key_entry.key_version;
        self.registry
            .reserve_identity(&plan.identity, &plan.key_entry)
            .await?;
        self.keystore
            .store_keypair(&plan.secret_handle, &plan.signing_key)
            .await?;
        self.registry
            .activate_identity(&plan_id, plan_key_version)
            .await?;
        Ok(target_id)
    }

    /// Open in maintenance mode.
    ///
    /// - [`MaintenanceMode::ReadOnly`]: skips the consistency check; uses
    ///   whatever `vault_id` is available (DB first, then file fallback).
    /// - [`MaintenanceMode::Mutating`]: enforces `vault.id` ↔ `vault_meta`
    ///   consistency but skips the reconciliation sweep.
    ///
    /// # Errors
    /// - [`IdentityServiceError::VaultIdMissing`] when neither the DB nor the
    ///   file provides a vault id.
    /// - [`IdentityServiceError::VaultIdConflict`] (Mutating mode only) when
    ///   the file and DB disagree.
    /// - [`IdentityServiceError::Registry`] on any `SQLite` backend failure.
    pub async fn open_for_maintenance(
        vault_path: PathBuf,
        mode: MaintenanceMode,
    ) -> Result<Self, IdentityServiceError> {
        let db_path = vault_path.join(".cairn/cairn.db");
        let registry: Arc<dyn IdentityRegistry> = Arc::new(SqliteIdentityRegistry::open(&db_path)?);

        let vault_id = match mode {
            MaintenanceMode::ReadOnly => {
                // No consistency check — prefer DB, fall back to file.
                if let Some((id, _)) = registry.read_vault_meta().await? {
                    id
                } else {
                    let p = vault_path.join(".cairn/vault.id");
                    let s = std::fs::read_to_string(&p)
                        .map_err(|_| IdentityServiceError::VaultIdMissing)?;
                    VaultId::parse(s.trim()).map_err(|e| {
                        IdentityServiceError::Registry(RegistryError::Backend(Box::new(e)))
                    })?
                }
            }
            MaintenanceMode::Mutating => {
                let p = vault_path.join(".cairn/vault.id");
                let s = std::fs::read_to_string(&p)
                    .map_err(|_| IdentityServiceError::VaultIdMissing)?;
                let file_id = VaultId::parse(s.trim()).map_err(|e| {
                    IdentityServiceError::Registry(RegistryError::Backend(Box::new(e)))
                })?;
                let (db_id, _) = registry
                    .read_vault_meta()
                    .await?
                    .ok_or(IdentityServiceError::VaultIdMissing)?;
                if file_id != db_id {
                    return Err(IdentityServiceError::VaultIdConflict { file_id, db_id });
                }
                file_id
            }
            // `#[non_exhaustive]` — forward-compatible fallthrough.
            _ => {
                return Err(IdentityServiceError::Registry(RegistryError::Backend(
                    "unknown MaintenanceMode variant".into(),
                )));
            }
        };

        let keystore: Arc<dyn Keystore> = Arc::new(OsKeystore::new(vault_id.clone()));

        Ok(Self {
            vault_path,
            vault_id,
            registry,
            keystore,
        })
    }
}
