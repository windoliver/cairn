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
        DefaultsState, Identity, IdentityKind, ProvisioningState,
        keys::{IdentityRevision, SecretHandle, VaultId},
        provision::{
            ProvisionInput, build_provisioning_plan, mint_agent_id, mint_human_id,
            normalize_human_slug,
        },
        records::{IdentityKeyEntry, PublicIdentityRecord},
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

    /// Provision default human and agent identities, applying the §3.5
    /// revision-bump rule (spec §4.5).
    ///
    /// For each default slot (human = OS username; agent = `claude-code /
    /// opus-4-7 / main`):
    ///
    /// 1. Query all rows for the slot kind at any lifecycle state.
    /// 2. Filter to rows whose identity string matches the slug/triple.
    /// 3. Find the highest-revision row among those.
    /// 4. Apply the bump rule:
    ///    - Highest row in a terminal state (`Revoked`, `RevokePending`,
    ///      `PurgePending`, `Purged`) → mint at `current_rev + 1`.
    ///    - Highest row is `Active` → no-op (idempotent).
    ///    - Highest row is `Pending` → self-heal via [`Self::provision`].
    ///    - No row at all → mint at [`IdentityRevision::FIRST`].
    ///
    /// # Errors
    ///
    /// Returns [`IdentityServiceError`] for registry failures, keystore
    /// failures, or revision-overflow.
    pub async fn init_defaults(
        &self,
        rng: &mut impl rand_core::CryptoRngCore,
    ) -> Result<DefaultsState, IdentityServiceError> {
        // ── Determine default slugs/triples ──────────────────────────────────
        let human_slug = normalize_human_slug(&whoami::username());
        let (agent_harness, agent_model, agent_role) = ("claude-code", "opus-4-7", "main");

        // ── Human slot ───────────────────────────────────────────────────────
        let human_id = self.provision_default_slot_human(&human_slug, rng).await?;

        // ── Agent slot ───────────────────────────────────────────────────────
        let agent_id = self
            .provision_default_slot_agent(agent_harness, agent_model, agent_role, rng)
            .await?;

        Ok(DefaultsState::Active {
            human: human_id,
            agent: agent_id,
        })
    }

    /// Inner helper: apply the revision-bump rule for the default human slot.
    async fn provision_default_slot_human(
        &self,
        slug: &str,
        rng: &mut impl rand_core::CryptoRngCore,
    ) -> Result<Identity, IdentityServiceError> {
        let all_rows = self
            .registry
            .list_identities(Some(IdentityKind::Human), IdentityVisibility::Audit)
            .await?;
        let slot_rows: Vec<&PublicIdentityRecord> = all_rows
            .iter()
            .filter(|r| slug_matches_human(r, slug))
            .collect();
        let highest = highest_revision_row(&slot_rows);
        self.apply_revision_bump_rule(
            IdentityKind::Human,
            highest,
            |rev| {
                mint_human_id(slug, rev).map_err(|e| {
                    IdentityServiceError::Registry(RegistryError::Backend(Box::new(e)))
                })
            },
            rng,
        )
        .await
    }

    /// Inner helper: apply the revision-bump rule for the default agent slot.
    async fn provision_default_slot_agent(
        &self,
        harness: &str,
        model: &str,
        role: &str,
        rng: &mut impl rand_core::CryptoRngCore,
    ) -> Result<Identity, IdentityServiceError> {
        let all_rows = self
            .registry
            .list_identities(Some(IdentityKind::Agent), IdentityVisibility::Audit)
            .await?;
        let slot_rows: Vec<&PublicIdentityRecord> = all_rows
            .iter()
            .filter(|r| slug_matches_agent(r, harness, model, role))
            .collect();
        let highest = highest_revision_row(&slot_rows);
        self.apply_revision_bump_rule(
            IdentityKind::Agent,
            highest,
            |rev| {
                mint_agent_id(harness, model, role, rev).map_err(|e| {
                    IdentityServiceError::Registry(RegistryError::Backend(Box::new(e)))
                })
            },
            rng,
        )
        .await
    }

    /// Core revision-bump rule applied to the highest-revision row for a slot.
    async fn apply_revision_bump_rule(
        &self,
        kind: IdentityKind,
        highest: Option<&PublicIdentityRecord>,
        mint: impl Fn(IdentityRevision) -> Result<Identity, IdentityServiceError>,
        rng: &mut impl rand_core::CryptoRngCore,
    ) -> Result<Identity, IdentityServiceError> {
        match highest {
            None => {
                // No existing row — mint at v1.
                let id = mint(IdentityRevision::FIRST)?;
                let input = ProvisionInput {
                    vault_id: self.vault_id.clone(),
                    id,
                    kind,
                    revision: IdentityRevision::FIRST,
                };
                self.provision(kind, input, rng).await
            }
            Some(row) => match row.provisioning_state {
                ProvisioningState::Active => {
                    // Already active — idempotent no-op.
                    Ok(row.id.clone())
                }
                ProvisioningState::Pending => {
                    // Self-heal: re-drive the pending row through provision.
                    let rev = revision_of(&row.id);
                    let input = ProvisionInput {
                        vault_id: self.vault_id.clone(),
                        id: row.id.clone(),
                        kind,
                        revision: rev,
                    };
                    self.provision(kind, input, rng).await
                }
                // Terminal / in-flight terminal states → bump revision.
                ProvisioningState::Revoked
                | ProvisioningState::RevokePending
                | ProvisioningState::PurgePending
                | ProvisioningState::Purged => {
                    let next_rev = revision_of(&row.id).next().map_err(|e| {
                        IdentityServiceError::Registry(RegistryError::Backend(Box::new(e)))
                    })?;
                    let id = mint(next_rev)?;
                    let input = ProvisionInput {
                        vault_id: self.vault_id.clone(),
                        id,
                        kind,
                        revision: next_rev,
                    };
                    self.provision(kind, input, rng).await
                }
            },
        }
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

// ── Module-level helpers ──────────────────────────────────────────────────────

/// Extract the [`IdentityRevision`] from an [`Identity`]'s wire form.
///
/// Parses the `:v<N>` suffix from the rightmost colon-delimited segment.
/// Returns [`IdentityRevision::FIRST`] if the suffix is absent or malformed.
fn revision_of(id: &Identity) -> IdentityRevision {
    id.as_str()
        .rsplit(':')
        .next()
        .and_then(|s| s.strip_prefix('v'))
        .and_then(|n| n.parse::<u32>().ok())
        .and_then(std::num::NonZeroU32::new)
        .map_or(IdentityRevision::FIRST, IdentityRevision::new)
}

/// Return the row with the highest revision from `rows`.
fn highest_revision_row<'a>(rows: &[&'a PublicIdentityRecord]) -> Option<&'a PublicIdentityRecord> {
    rows.iter()
        .copied()
        .max_by_key(|r| revision_of(&r.id).as_u32())
}

/// True if `record` belongs to the human default slot for `slug`.
///
/// Matches `hmn:<slug>:v<N>` — parses by splitting on `:` and comparing
/// the prefix-stripped body up to the final `:v<N>` component.
fn slug_matches_human(record: &PublicIdentityRecord, slug: &str) -> bool {
    // Wire form: "hmn:<slug>:v<N>"
    let s = record.id.as_str();
    if let Some(body) = s.strip_prefix("hmn:") {
        // body is "<slug>:v<N>"
        if let Some((candidate_slug, _rev)) = body.rsplit_once(':') {
            return candidate_slug == slug;
        }
    }
    false
}

/// True if `record` belongs to the agent default slot for `(harness, model, role)`.
///
/// Matches `agt:<harness>:<model>:<role>:v<N>`.
fn slug_matches_agent(
    record: &PublicIdentityRecord,
    harness: &str,
    model: &str,
    role: &str,
) -> bool {
    // Wire form: "agt:<harness>:<model>:<role>:v<N>"
    let s = record.id.as_str();
    if let Some(body) = s.strip_prefix("agt:") {
        // body is "<harness>:<model>:<role>:v<N>"
        // Strip the trailing ":v<N>" by finding the last ':'
        if let Some((triple, _rev)) = body.rsplit_once(':') {
            // triple is "<harness>:<model>:<role>"
            let expected = format!("{harness}:{model}:{role}");
            return triple == expected;
        }
    }
    false
}
