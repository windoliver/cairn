//! Top-level orchestrator error for the identity service (spec §4.1).
//!
//! [`IdentityServiceError`] wraps the two contract-level error types
//! ([`KeystoreError`] and [`RegistryError`]) and adds higher-level failure
//! modes that span both contracts — vault-bootstrap failures, lock contention,
//! and key-material desynchronisation detected by the service layer.

use crate::{
    contract::{identity_registry::RegistryError, keystore::KeystoreError},
    domain::identity::{Identity, keys::VaultId},
};

/// Orchestrator-level error for identity-service operations (spec §4.1).
///
/// Wraps [`KeystoreError`] and [`RegistryError`] for operations that cross
/// both contract boundaries, and adds higher-level failure modes detected by
/// the service layer itself (vault bootstrap, lock contention, key-material
/// desynchronisation).
///
/// This enum is `#[non_exhaustive]` — callers must handle an `_` arm to
/// remain forward-compatible when new variants are added.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IdentityServiceError {
    /// A [`KeystoreError`] propagated from the underlying keystore backend.
    #[error("keystore: {0}")]
    Keystore(
        /// The underlying keystore failure.
        #[from]
        KeystoreError,
    ),

    /// A [`RegistryError`] propagated from the identity registry backend.
    #[error("registry: {0}")]
    Registry(
        /// The underlying registry failure.
        #[from]
        RegistryError,
    ),

    /// The identity service has not been initialised with default identities.
    ///
    /// Run `cairn identity init-defaults` to bootstrap the vault before
    /// attempting this operation.
    #[error("defaults not initialised — run `cairn identity init-defaults`")]
    DefaultsNotInitialized,

    /// The key material held in the keystore disagrees with the registry's
    /// record for `id`.
    ///
    /// This indicates a desynchronisation between the keystore and the
    /// `identity_keys` table — possibly caused by a crash mid-rotation or
    /// a manual edit of the keystore. The `reason` field names the
    /// specific discrepancy detected.
    #[error("identity {id} key material is desynchronized: {reason}")]
    KeyMaterialDesynchronized {
        /// The identity for which the desynchronisation was detected.
        id: Identity,
        /// A human-readable description of the discrepancy.
        reason: String,
    },

    /// No live, attributable signer is available to produce a signature.
    ///
    /// This can occur when all active identities are locked, revoked, or
    /// have had their key material evicted from the keystore.
    #[error("no live attributable signer available")]
    NoLiveAttributableSigner,

    /// The `.cairn/vault.id` file is absent or unreadable.
    ///
    /// The vault has either never been bootstrapped or the `vault.id` file
    /// was removed after bootstrap. Re-run `cairn identity init` to restore it.
    #[error("vault id missing — bootstrap not run or `.cairn/vault.id` removed")]
    VaultIdMissing,

    /// The `vault.id` on disk disagrees with the `vault_id` stored in the
    /// registry's `vault_meta` table.
    ///
    /// `file_id` is the value read from `.cairn/vault.id`; `db_id` is the
    /// value committed in `vault_meta`. This split-brain state requires
    /// operator intervention.
    #[error("vault id conflict — file={file_id}, db={db_id}")]
    VaultIdConflict {
        /// The `vault_id` read from the `.cairn/vault.id` file.
        file_id: VaultId,
        /// The `vault_id` committed in the registry's `vault_meta` table.
        db_id: VaultId,
    },

    /// The vault is degraded: one or more identities have a
    /// `KeyMaterialMismatch` between the keystore and the registry.
    ///
    /// `mismatched_ids` lists every identity whose key material is
    /// inconsistent. The vault remains read-only until the mismatches are
    /// resolved via `cairn identity repair`.
    #[error("vault degraded — KeyMaterialMismatch for: {mismatched_ids:?}")]
    VaultDegraded {
        /// All identities with detected key-material mismatches.
        mismatched_ids: Vec<Identity>,
    },

    /// Another process has already claimed this vault namespace.
    ///
    /// The `vault_id` is already registered in the registry under a different
    /// process or node. Namespace sharing requires explicit federation
    /// configuration (spec §12).
    #[error("vault namespace already claimed: {vault_id}")]
    VaultNamespaceClaimed {
        /// The [`VaultId`] whose namespace is already claimed.
        vault_id: VaultId,
    },

    /// A first-bind operation is already in progress.
    ///
    /// The `.cairn/vault.binding.pending` lock file exists, indicating a
    /// concurrent first-bind that has not yet committed or been cleaned up.
    #[error("first bind in progress (.cairn/vault.binding.pending exists)")]
    FirstBindInProgress,

    /// The first-bind lock file could not be acquired because another thread
    /// or process holds it.
    ///
    /// Retry after the concurrent first-bind operation completes or times out.
    #[error("first bind lock busy")]
    FirstBindInFlight,

    /// The per-identity operation lock for `id` is held by another operation.
    ///
    /// Retry after the concurrent identity operation on `id` completes.
    #[error("identity lock busy: {id}")]
    IdentityLockBusy {
        /// The identity whose lock is currently held.
        id: Identity,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keystore_from_conversion_is_keystore_variant() {
        let ks_err = KeystoreError::Locked;
        let svc_err = IdentityServiceError::from(ks_err);
        assert!(matches!(svc_err, IdentityServiceError::Keystore(_)));
    }

    #[test]
    fn registry_from_conversion_is_registry_variant() {
        let reg_err = RegistryError::NotFound;
        let svc_err = IdentityServiceError::from(reg_err);
        assert!(matches!(svc_err, IdentityServiceError::Registry(_)));
    }

    #[test]
    fn display_defaults_not_initialized() {
        let msg = IdentityServiceError::DefaultsNotInitialized.to_string();
        assert!(msg.contains("init-defaults"), "got: {msg}");
    }

    #[test]
    fn display_vault_id_missing_mentions_vault_id() {
        let msg = IdentityServiceError::VaultIdMissing.to_string();
        assert!(msg.contains("vault"), "got: {msg}");
    }
}
