//! `IdentityRegistry` contract per spec §4.1.
//!
//! The registry owns the durable store of provisioned identities, their key
//! entries, and all in-flight WAL rows that drive the provisioning state
//! machine (§3.5).  Adapters back this trait with an `SQLite` database; the
//! trait itself is pure I/O-free until an adapter is wired in at startup.
//!
//! # Visibility filtering
//!
//! Most read paths accept an [`IdentityVisibility`] argument that controls
//! which lifecycle states are returned.  Use [`IdentityVisibility::Operational`]
//! for normal issuer-dependent verbs; promote to [`IdentityVisibility::Audit`]
//! only for privileged audit reads.
//!
//! # Maintenance mode
//!
//! The registry can be opened in two modes (see [`MaintenanceMode`]):
//! - [`MaintenanceMode::ReadOnly`] — no keystore handle; safe for `list`,
//!   `show`, and dry-run sweeps.
//! - [`MaintenanceMode::Mutating`] — requires a live keystore handle and
//!   enforces `vault.id ↔ vault_meta.vault_id` consistency on open.

use std::path::Path;

use async_trait::async_trait;

use crate::domain::identity::{
    Identity, IdentityKind,
    keys::{KeyVersion, VaultId, WitnessHash},
    receipts::{RevocationReceipt, RotationReceipt},
    records::{
        FirstBindState, IdentityKeyEntry, PendingEvictionEntry, PendingIdentityEntry,
        PendingKeyDisableEntry, ProvisioningState, PublicIdentityRecord, PurgePendingEntry,
        ReceiptId, RevokePendingEntry,
    },
};

/// Controls which lifecycle states appear in registry read operations.
///
/// This enum is `#[non_exhaustive]` — callers must handle an `_` arm to
/// remain forward-compatible if new visibility tiers are introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdentityVisibility {
    /// Return only `active` and `revoked` identities.
    ///
    /// This is the default for issuer-dependent verbs (sign, verify).
    /// Pending and purged rows are hidden.
    Operational,

    /// Return `active`, `revoked`, **and** `pending` identities.
    ///
    /// Used by the §3.5 reconciliation sweep to detect stale pending rows
    /// that should be retried or expired.
    IncludingPending,

    /// Return `active`, `revoked`, `pending`, **and** `purge_pending` identities.
    ///
    /// Used by `purge --resume` to locate in-flight purge operations that
    /// survived a process crash and need to be continued.
    IncludingPurgePending,

    /// Return all states including `purged`.
    ///
    /// Used exclusively by privileged audit reads.  Callers must hold an
    /// appropriate capability before using this tier.
    Audit,
}

/// Whether the registry is opened for read-only access or mutating access.
///
/// This enum is `#[non_exhaustive]` — callers must handle an `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaintenanceMode {
    /// No keystore handle is required; no `vault.id ↔ vault_meta.vault_id`
    /// consistency check is performed on open.
    ///
    /// Suitable for `list`, `show`, and the dry-run sweep that
    /// `cairn identity status` runs.
    ReadOnly,

    /// Registry + keystore handle are both required.
    ///
    /// Enforces that `vault.id` stored inside the `SQLite` database matches
    /// `vault_meta.vault_id` in the `vault_meta` table before allowing any
    /// write; refuses with [`RegistryError::VaultIdConflict`] on disagreement.
    Mutating,
}

/// A proof that the caller has verified the purge path via the filesystem
/// verifier in `cairn-cli`.
///
/// Construction is intentionally restricted to within `cairn-core` via the
/// `pub(crate)` inner field — external callers cannot manufacture a value of
/// this type without going through the verifier.
#[derive(Debug, Clone, Copy)]
pub struct PurgeAcknowledgement(
    /// Private token: construction is private — only via the `cairn-cli`
    /// filesystem-path verifier.
    pub(crate) (),
);

/// A human-readable reason for a purge request (e.g., GDPR erasure).
///
/// Stored verbatim in the `purge_pending` WAL row and surfaced in audit logs.
#[derive(Debug, Clone)]
pub struct PurgeReason(
    /// The free-form reason string supplied by the operator.
    pub String,
);

/// Errors returned by [`IdentityRegistry`] operations.
///
/// This enum is `#[non_exhaustive]` — callers must handle an `_` arm to
/// remain compatible with future backend-specific variants.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// The requested identity does not exist in the registry.
    #[error("identity not found")]
    NotFound,

    /// An identity with the given id already exists in the registry.
    #[error("identity already exists: {id}")]
    IdentityExists {
        /// The identity that caused the collision.
        id: Identity,
    },

    /// A provisioning operation is already in-flight for the given identity.
    #[error("provisioning in flight for {id}")]
    ProvisioningInFlight {
        /// The identity whose provisioning is in-flight.
        id: Identity,
    },

    /// The identity has already been revoked; the operation is not permitted
    /// on revoked identities.
    #[error("identity already revoked: {id}")]
    AlreadyRevoked {
        /// The revoked identity.
        id: Identity,
    },

    /// The key material supplied by the caller does not match what the
    /// registry has stored for the identity.
    #[error("key material mismatch for {id}")]
    KeyMaterialMismatch {
        /// The identity for which the mismatch was detected.
        id: Identity,
    },

    /// A key-version conflict was detected during a CAS rotation: the caller
    /// expected `existing` to be the current version but the registry holds
    /// a different one.
    #[error("key version conflict: existing={existing}, attempted={attempted}")]
    KeyVersionConflict {
        /// The version the caller believed was current.
        existing: KeyVersion,
        /// The version the caller attempted to install as the next version.
        attempted: KeyVersion,
    },

    /// The identity is not in a state that allows a purge to begin.
    #[error("invalid purge start state for {id}: {state:?}")]
    InvalidPurgeStartState {
        /// The identity for which the purge was attempted.
        id: Identity,
        /// The actual state of the identity at the time of the attempt.
        state: ProvisioningState,
    },

    /// The purge cannot be finalised because some key versions have not yet
    /// been evicted from the keystore.
    #[error("purge incomplete for {id}: {remaining_versions} versions remaining")]
    PurgeIncomplete {
        /// The identity whose purge is incomplete.
        id: Identity,
        /// How many key versions still need to be evicted before finalisation.
        remaining_versions: u64,
    },

    /// The `vault_id` supplied during first-bind does not match the one
    /// already stored in `vault_meta`.
    #[error("first-bind mismatch: stored={stored}, attempted={attempted}")]
    FirstBindMismatch {
        /// The `vault_id` already committed in `vault_meta`.
        stored: VaultId,
        /// The `vault_id` the caller attempted to bind.
        attempted: VaultId,
    },

    /// The `vault_meta` row is absent, meaning first-bind has not committed
    /// yet and the registry is not safe for mutating operations.
    #[error("vault_meta missing — first-bind has not committed yet")]
    VaultMetaMissing,

    /// A first-bind was attempted but the registry already has a committed
    /// first-bind entry.
    #[error("first-bind already committed for this registry")]
    FirstBindAlreadyCommitted,

    /// The contents of `binding_path` do not match `witness_hash`.
    ///
    /// This indicates either a corrupted binding file or a replay of a
    /// stale binding token.
    #[error("witness mismatch — binding_path contents do not match witness_hash argument")]
    WitnessMismatch,

    /// A `vault.id ↔ vault_meta.vault_id` consistency check failed when
    /// opening the registry in [`MaintenanceMode::Mutating`].
    #[error("vault_id conflict: registry holds {stored}, open request presents {presented}")]
    VaultIdConflict {
        /// The `vault_id` stored in `vault_meta`.
        stored: VaultId,
        /// The `vault_id` supplied by the open request.
        presented: VaultId,
    },

    /// An opaque error from the underlying registry backend (`SQLite`, etc.).
    #[error("backend error: {0}")]
    Backend(
        /// The underlying backend error.
        #[source]
        Box<dyn std::error::Error + Send + Sync>,
    ),
}

/// Durable store for provisioned identities, key entries, and WAL rows
/// that drive the provisioning state machine (spec §4.1, §3.5).
///
/// All mutating methods run through the WAL state machine (brief §5.6):
/// adapters must write WAL rows atomically with their associated record
/// mutations and never mutate records directly.
///
/// All methods are `async` to accommodate adapters that perform blocking I/O.
/// Sync backends must use `tokio::task::spawn_blocking` internally.
#[async_trait]
pub trait IdentityRegistry: Send + Sync {
    // ── Provisioning state machine (§3.5) ────────────────────────────────────

    /// Write a pending provisioning row for `record` + `key` into the registry.
    ///
    /// Fails with [`RegistryError::IdentityExists`] if the identity is already
    /// active or with [`RegistryError::ProvisioningInFlight`] if a pending row
    /// already exists for the same identity.
    ///
    /// # Errors
    /// Returns [`RegistryError::IdentityExists`], [`RegistryError::ProvisioningInFlight`],
    /// or [`RegistryError::Backend`] on failure.
    async fn reserve_identity(
        &self,
        record: &PublicIdentityRecord,
        key: &IdentityKeyEntry,
    ) -> Result<(), RegistryError>;

    /// Transition a pending identity to `Active` at `key_version`.
    ///
    /// # Errors
    /// Returns [`RegistryError::NotFound`] if no pending row exists for `id`,
    /// [`RegistryError::KeyVersionConflict`] if `key_version` does not match
    /// the pending row, or [`RegistryError::Backend`] on failure.
    async fn activate_identity(
        &self,
        id: &Identity,
        key_version: KeyVersion,
    ) -> Result<(), RegistryError>;

    /// Delete a pending provisioning row for `id` at `key_version`.
    ///
    /// Used to roll back a failed provisioning attempt.
    ///
    /// # Errors
    /// Returns [`RegistryError::NotFound`] if no matching pending row exists,
    /// or [`RegistryError::Backend`] on failure.
    async fn delete_pending(
        &self,
        id: &Identity,
        key_version: KeyVersion,
    ) -> Result<(), RegistryError>;

    /// List all pending provisioning entries across all identities.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn list_pending(&self) -> Result<Vec<PendingIdentityEntry>, RegistryError>;

    /// List pending provisioning entries for a single identity.
    ///
    /// Returns an empty `Vec` if the identity has no pending rows.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn list_pending_by_identity(
        &self,
        id: &Identity,
    ) -> Result<Vec<PendingIdentityEntry>, RegistryError>;

    // ── Read paths ───────────────────────────────────────────────────────────

    /// Look up the public identity record for `id`.
    ///
    /// Returns `None` if the identity is not found at the requested
    /// `visibility` level (e.g., the identity is `Purged` but `visibility`
    /// is `Operational`).
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn get_identity(
        &self,
        id: &Identity,
        visibility: IdentityVisibility,
    ) -> Result<Option<PublicIdentityRecord>, RegistryError>;

    /// List all identities, optionally filtered to a single [`IdentityKind`].
    ///
    /// The `visibility` argument controls which lifecycle states are included.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn list_identities(
        &self,
        kind: Option<IdentityKind>,
        visibility: IdentityVisibility,
    ) -> Result<Vec<PublicIdentityRecord>, RegistryError>;

    /// List all key entries for `id` across all versions.
    ///
    /// # Errors
    /// Returns [`RegistryError::NotFound`] if the identity does not exist,
    /// or [`RegistryError::Backend`] on failure.
    async fn list_keys(&self, id: &Identity) -> Result<Vec<IdentityKeyEntry>, RegistryError>;

    // ── Counts ───────────────────────────────────────────────────────────────

    /// Return the total number of key entries stored across all identities.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn count_keys(&self) -> Result<u64, RegistryError>;

    /// List every key entry across all identities.
    ///
    /// Used during reconciliation and purge sweeps where a global key census
    /// is required.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn list_all_keys(&self) -> Result<Vec<IdentityKeyEntry>, RegistryError>;

    // ── Rotation (CAS-protected) ─────────────────────────────────────────────

    /// Apply a signed rotation receipt using compare-and-swap on `expected_current`.
    ///
    /// Fails atomically if the current version stored in the registry is not
    /// `expected_current` at the time of the write, preventing lost-update
    /// races during concurrent rotations.
    ///
    /// # Errors
    /// Returns [`RegistryError::KeyVersionConflict`] if the CAS fails,
    /// [`RegistryError::NotFound`] if the identity does not exist, or
    /// [`RegistryError::Backend`] on failure.
    async fn apply_rotation(
        &self,
        receipt: &RotationReceipt,
        expected_current: KeyVersion,
    ) -> Result<(), RegistryError>;

    // ── Pre-commit rotation intent (§3.6 step 0a) ────────────────────────────

    /// Record a pending rotation intent row so that a crash mid-rotation can
    /// be detected and resumed (spec §3.6 step 0a).
    ///
    /// # Errors
    /// Returns [`RegistryError::NotFound`] if `identity` does not exist,
    /// or [`RegistryError::Backend`] on failure.
    async fn insert_pending_rotation(
        &self,
        identity: &Identity,
        planned_version: KeyVersion,
        planned_handle: &str,
    ) -> Result<(), RegistryError>;

    /// Delete the pending rotation intent row for `identity` at
    /// `planned_version`.
    ///
    /// Called after the rotation has either succeeded or been rolled back.
    ///
    /// # Errors
    /// Returns [`RegistryError::NotFound`] if no matching row exists,
    /// or [`RegistryError::Backend`] on failure.
    async fn delete_pending_rotation(
        &self,
        identity: &Identity,
        planned_version: KeyVersion,
    ) -> Result<(), RegistryError>;

    /// Return all pending rotation intent rows for `identity`.
    ///
    /// Each tuple is `(planned_version, planned_handle)`.  Returns an empty
    /// `Vec` if there are no in-flight rotations.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn list_pending_rotations(
        &self,
        identity: &Identity,
    ) -> Result<Vec<(KeyVersion, String)>, RegistryError>;

    // ── Revocation two-phase tombstone ───────────────────────────────────────

    /// Write a `revoke_pending` WAL row from the signed revocation receipt
    /// (phase 1 of the two-phase tombstone).
    ///
    /// # Errors
    /// Returns [`RegistryError::NotFound`] if the identity does not exist,
    /// [`RegistryError::AlreadyRevoked`] if it is already revoked, or
    /// [`RegistryError::Backend`] on failure.
    async fn begin_revocation(&self, receipt: &RevocationReceipt) -> Result<(), RegistryError>;

    /// Commit the revocation and delete the WAL row for `id` (phase 2).
    ///
    /// # Errors
    /// Returns [`RegistryError::NotFound`] if no `revoke_pending` row exists
    /// for `id`, or [`RegistryError::Backend`] on failure.
    async fn finalise_revocation(&self, id: &Identity) -> Result<(), RegistryError>;

    /// List all in-flight revocation WAL rows.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn list_revoke_pending(&self) -> Result<Vec<RevokePendingEntry>, RegistryError>;

    // ── First-bind transaction ───────────────────────────────────────────────

    /// Atomically write the first-bind pending row: `vault_meta`, a
    /// `PublicIdentityRecord`, and an `IdentityKeyEntry` together with a
    /// `WitnessHash` over the binding file at `binding_path`.
    ///
    /// Fails with [`RegistryError::FirstBindAlreadyCommitted`] if `vault_meta`
    /// already holds a committed entry, or [`RegistryError::WitnessMismatch`]
    /// if `witness_hash` does not match the contents at `binding_path`.
    ///
    /// # Errors
    /// Returns [`RegistryError::FirstBindAlreadyCommitted`],
    /// [`RegistryError::WitnessMismatch`], [`RegistryError::IdentityExists`],
    /// or [`RegistryError::Backend`] on failure.
    async fn reserve_first_identity(
        &self,
        vault_id: &VaultId,
        record: &PublicIdentityRecord,
        key: &IdentityKeyEntry,
        witness_hash: WitnessHash,
        binding_path: &Path,
    ) -> Result<(), RegistryError>;

    /// Query the current first-bind state for `vault_id`.
    ///
    /// Returns one of [`FirstBindState::Absent`], [`FirstBindState::Reserved`],
    /// or [`FirstBindState::Activated`].
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn get_first_bind_state(
        &self,
        vault_id: &VaultId,
    ) -> Result<FirstBindState, RegistryError>;

    // ── vault_meta read ──────────────────────────────────────────────────────

    /// Read the committed `(vault_id, witness_hash)` pair from `vault_meta`.
    ///
    /// Returns `None` if first-bind has not yet been committed.  Used by
    /// the bootstrap fail-closed guard and by the
    /// [`MaintenanceMode::Mutating`] consistency check on open.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn read_vault_meta(&self) -> Result<Option<(VaultId, WitnessHash)>, RegistryError>;

    // ── Receipt reconciliation flag clears ───────────────────────────────────

    /// Delete the `pending_eviction` WAL row identified by `receipt_id`.
    ///
    /// Called after the corresponding keystore eviction has succeeded.
    ///
    /// # Errors
    /// Returns [`RegistryError::NotFound`] if no matching row exists,
    /// or [`RegistryError::Backend`] on failure.
    async fn clear_pending_eviction(&self, receipt_id: &ReceiptId) -> Result<(), RegistryError>;

    /// List all outstanding `pending_eviction` WAL rows.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn list_pending_evictions(&self) -> Result<Vec<PendingEvictionEntry>, RegistryError>;

    /// Delete the `pending_key_disable` WAL row identified by `receipt_id`.
    ///
    /// Called after the keystore has successfully disabled the keys.
    ///
    /// # Errors
    /// Returns [`RegistryError::NotFound`] if no matching row exists,
    /// or [`RegistryError::Backend`] on failure.
    async fn clear_pending_key_disable(&self, receipt_id: &ReceiptId) -> Result<(), RegistryError>;

    /// List all outstanding `pending_key_disable` WAL rows.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn list_pending_key_disables(&self)
    -> Result<Vec<PendingKeyDisableEntry>, RegistryError>;

    // ── Two-phase purge tombstone ────────────────────────────────────────────

    /// Write a `purge_pending` WAL row for `id` (phase 1 of the two-phase
    /// purge tombstone).
    ///
    /// `ack` proves the caller has run the filesystem-path verifier.
    /// `reason` is stored verbatim in the WAL row for audit purposes.
    ///
    /// # Errors
    /// Returns [`RegistryError::InvalidPurgeStartState`] if the identity is
    /// not in a state that allows a purge to begin (must be `Revoked`),
    /// [`RegistryError::NotFound`] if the identity does not exist, or
    /// [`RegistryError::Backend`] on failure.
    async fn mark_purge_pending(
        &self,
        id: &Identity,
        ack: &PurgeAcknowledgement,
        reason: PurgeReason,
    ) -> Result<(), RegistryError>;

    /// Finalise a purge: delete the identity row, all key entries, and the
    /// `purge_pending` WAL row for `id` (phase 2).
    ///
    /// # Errors
    /// Returns [`RegistryError::PurgeIncomplete`] if any key versions have
    /// not yet been evicted from the keystore,
    /// [`RegistryError::NotFound`] if no `purge_pending` row exists for `id`,
    /// or [`RegistryError::Backend`] on failure.
    async fn finalise_purge(&self, id: &Identity) -> Result<(), RegistryError>;

    /// List all in-flight `purge_pending` WAL rows.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] on failure.
    async fn list_purge_pending(&self) -> Result<Vec<PurgePendingEntry>, RegistryError>;
}
