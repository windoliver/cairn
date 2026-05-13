//! `Keystore` contract per spec §4.1.
//!
//! The keystore is responsible for durable, secure storage and retrieval of
//! cryptographic material (Ed25519 signing keys and opaque secret bytes)
//! keyed by [`SecretHandle`]. Adapters may delegate to OS keystores (macOS
//! Keychain, Windows DPAPI, Secret Service), file-system encrypted stores,
//! or in-memory stores for testing.

use async_trait::async_trait;

use crate::domain::identity::{
    Identity,
    keys::{KeyVersion, SecretBytes, SecretHandle, SigningKey, VaultId},
};

/// Errors returned by [`Keystore`] operations.
///
/// This enum is `#[non_exhaustive]` — callers must handle an `_` arm to
/// remain compatible with future backend-specific variants.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KeystoreError {
    /// The requested handle does not exist in the keystore.
    #[error("keystore handle not found")]
    NotFound,

    /// The keystore is locked and must be unlocked by an operator before use.
    #[error("keystore is locked (operator must unlock)")]
    Locked,

    /// The caller does not have permission to access the requested handle.
    #[error("permission denied accessing keystore")]
    PermissionDenied,

    /// The backend does not support namespace or identity enumeration (e.g., DPAPI).
    #[error("backend does not support enumeration (e.g., DPAPI)")]
    DiscoveryUnsupported,

    /// An opaque error from the underlying keystore backend.
    #[error("backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Secure storage contract for cryptographic material (spec §4.1).
///
/// Each method operates on a [`SecretHandle`] — a composite key of
/// (`vault_id`, `identity`, `version`) — or its witness form.  Adapters are
/// responsible for translating these handles into backend-specific account
/// or item identifiers.
///
/// All methods are `async` to accommodate backends that perform blocking I/O
/// (e.g., OS keychain prompts, HSM round-trips).  Use
/// `tokio::task::spawn_blocking` inside an adapter if the underlying call is
/// synchronous.
#[async_trait]
pub trait Keystore: Send + Sync {
    // ── Ed25519 keypair operations ───────────────────────────────────────────

    /// Persist an Ed25519 signing key under `handle`.
    ///
    /// Overwrites any previously stored key for the same handle.
    ///
    /// # Errors
    /// Returns [`KeystoreError::Locked`] if the store is locked,
    /// [`KeystoreError::PermissionDenied`] if access is denied, or
    /// [`KeystoreError::Backend`] on any other backend failure.
    async fn store_keypair(
        &self,
        handle: &SecretHandle,
        secret: &SigningKey,
    ) -> Result<(), KeystoreError>;

    /// Retrieve the Ed25519 signing key stored under `handle`.
    ///
    /// # Errors
    /// Returns [`KeystoreError::NotFound`] if no key exists for `handle`,
    /// [`KeystoreError::Locked`] if the store is locked, or
    /// [`KeystoreError::Backend`] on any other backend failure.
    async fn load_signing_key(&self, handle: &SecretHandle) -> Result<SigningKey, KeystoreError>;

    /// Delete the Ed25519 keypair stored under `handle`.
    ///
    /// A no-op (success) if `handle` does not exist.
    ///
    /// # Errors
    /// Returns [`KeystoreError::Locked`] if the store is locked,
    /// [`KeystoreError::PermissionDenied`] if access is denied, or
    /// [`KeystoreError::Backend`] on any other backend failure.
    async fn delete_keypair(&self, handle: &SecretHandle) -> Result<(), KeystoreError>;

    // ── Opaque-bytes operations (vault witness) ──────────────────────────────

    /// Persist opaque secret bytes under `handle` (e.g., a vault witness MAC).
    ///
    /// # Errors
    /// Returns [`KeystoreError::Locked`] if the store is locked,
    /// [`KeystoreError::PermissionDenied`] if access is denied, or
    /// [`KeystoreError::Backend`] on any other backend failure.
    async fn store_secret(&self, handle: &SecretHandle, bytes: &[u8]) -> Result<(), KeystoreError>;

    /// Retrieve opaque secret bytes stored under `handle`.
    ///
    /// # Errors
    /// Returns [`KeystoreError::NotFound`] if no secret exists for `handle`,
    /// [`KeystoreError::Locked`] if the store is locked, or
    /// [`KeystoreError::Backend`] on any other backend failure.
    async fn load_secret(&self, handle: &SecretHandle) -> Result<SecretBytes, KeystoreError>;

    /// Delete the opaque secret stored under `handle`.
    ///
    /// A no-op (success) if `handle` does not exist.
    ///
    /// # Errors
    /// Returns [`KeystoreError::Locked`] if the store is locked,
    /// [`KeystoreError::PermissionDenied`] if access is denied, or
    /// [`KeystoreError::Backend`] on any other backend failure.
    async fn delete_secret(&self, handle: &SecretHandle) -> Result<(), KeystoreError>;

    // ── Discovery ────────────────────────────────────────────────────────────

    /// List all vault namespaces whose service label begins with `service_prefix`.
    ///
    /// Used during reconciliation to discover vaults stored in the backend
    /// without consulting the `SQLite` registry.
    ///
    /// # Errors
    /// Returns [`KeystoreError::DiscoveryUnsupported`] if the backend does not
    /// support enumeration, or [`KeystoreError::Backend`] on failure.
    async fn list_vault_namespaces(
        &self,
        service_prefix: &str,
    ) -> Result<Vec<VaultId>, KeystoreError>;

    /// Enumerate every `<identity-wire-form>#k<version>` account stored for
    /// `id` under `vault_id`.
    ///
    /// Used by revoke and purge operations to catch orphan private keys not
    /// represented in the `identity_keys` table.
    ///
    /// # Errors
    /// Returns [`KeystoreError::DiscoveryUnsupported`] if the backend does not
    /// support enumeration, [`KeystoreError::NotFound`] if `vault_id` is
    /// unknown, or [`KeystoreError::Backend`] on failure.
    async fn list_identity_versions(
        &self,
        vault_id: &VaultId,
        id: &Identity,
    ) -> Result<Vec<KeyVersion>, KeystoreError>;
}
