//! OS-keychain backed [`Keystore`] implementation.

use async_trait::async_trait;
use keyring::Entry;

use cairn_core::contract::keystore::{Keystore, KeystoreError};
use cairn_core::domain::identity::{
    Identity,
    keys::{KeyVersion, SecretBytes, SecretHandle, SigningKey, VaultId},
};

/// OS-keychain backed [`Keystore`] scoped to a single vault.
///
/// Wraps the `keyring` crate so that every handle maps to a service
/// string of `cairn:<vault_id>` and an account string derived from the
/// [`SecretHandle`] discriminant.  All keyring calls are synchronous and
/// are dispatched via [`tokio::task::spawn_blocking`].
pub struct OsKeystore {
    /// Optional bound vault id.
    ///
    /// - [`OsKeystore::new`] sets this; every operation asserts the incoming
    ///   handle belongs to the same vault.
    /// - [`OsKeystore::for_discovery`] leaves it `None`, permitting
    ///   cross-vault reads needed during vault-id recovery.
    bound_vault: Option<VaultId>,
}

impl OsKeystore {
    /// Create a vault-scoped keystore handle.
    ///
    /// All operations that receive a [`SecretHandle`] whose `vault_id`
    /// differs from `vault_id` will return a [`KeystoreError::Backend`]
    /// mismatch error.
    #[must_use]
    pub fn new(vault_id: VaultId) -> Self {
        Self {
            bound_vault: Some(vault_id),
        }
    }

    /// Create an unscoped keystore handle for vault-id discovery.
    ///
    /// Skips the vault-id bound check so callers can probe handles without
    /// knowing the vault id in advance.
    #[must_use]
    pub fn for_discovery() -> Self {
        Self { bound_vault: None }
    }

    /// Assert that `handle.vault_id` matches the bound vault (if any).
    fn ensure_bound_match(&self, handle: &SecretHandle) -> Result<(), KeystoreError> {
        if let Some(bound) = &self.bound_vault
            && bound != &handle.vault_id
        {
            return Err(KeystoreError::Backend(
                format!("handle vault {} != bound vault {}", handle.vault_id, bound).into(),
            ));
        }
        Ok(())
    }

    /// Build a [`keyring::Entry`] for `handle`, asserting vault bounds first.
    fn entry(&self, handle: &SecretHandle) -> Result<Entry, KeystoreError> {
        self.ensure_bound_match(handle)?;
        Entry::new(&handle.service(), &handle.account_string()).map_err(map_keyring_err)
    }
}

/// Map a [`keyring::Error`] to the closest [`KeystoreError`] variant.
fn map_keyring_err(e: keyring::Error) -> KeystoreError {
    use keyring::Error as K;
    match e {
        K::NoEntry => KeystoreError::NotFound,
        K::PlatformFailure(inner) => KeystoreError::Backend(inner),
        // `NoStorageAccess` means the OS keychain is locked; map to `Locked`.
        K::NoStorageAccess(_inner) => KeystoreError::Locked,
        K::BadEncoding(_) | K::TooLong(_, _) | K::Invalid(_, _) | K::Ambiguous(_) => {
            KeystoreError::Backend(Box::new(e))
        }
        // Catch future variants from a `#[non_exhaustive]` upstream enum.
        _ => KeystoreError::Backend(Box::new(e)),
    }
}

#[async_trait]
impl Keystore for OsKeystore {
    async fn store_keypair(
        &self,
        handle: &SecretHandle,
        secret: &SigningKey,
    ) -> Result<(), KeystoreError> {
        let entry = self.entry(handle)?;
        let bytes = secret.expose_secret_bytes();
        tokio::task::spawn_blocking(move || entry.set_secret(&bytes).map_err(map_keyring_err))
            .await
            .map_err(|e| KeystoreError::Backend(Box::new(e)))?
    }

    async fn load_signing_key(&self, handle: &SecretHandle) -> Result<SigningKey, KeystoreError> {
        let entry = self.entry(handle)?;
        let raw = tokio::task::spawn_blocking(move || entry.get_secret().map_err(map_keyring_err))
            .await
            .map_err(|e| KeystoreError::Backend(Box::new(e)))??;
        let arr: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| KeystoreError::Backend("signing key bytes != 32".into()))?;
        Ok(SigningKey::from_bytes(&arr))
    }

    async fn delete_keypair(&self, handle: &SecretHandle) -> Result<(), KeystoreError> {
        let entry = self.entry(handle)?;
        tokio::task::spawn_blocking(move || entry.delete_credential().map_err(map_keyring_err))
            .await
            .map_err(|e| KeystoreError::Backend(Box::new(e)))?
    }

    async fn store_secret(&self, handle: &SecretHandle, bytes: &[u8]) -> Result<(), KeystoreError> {
        let entry = self.entry(handle)?;
        let owned = bytes.to_vec();
        tokio::task::spawn_blocking(move || entry.set_secret(&owned).map_err(map_keyring_err))
            .await
            .map_err(|e| KeystoreError::Backend(Box::new(e)))?
    }

    async fn load_secret(&self, handle: &SecretHandle) -> Result<SecretBytes, KeystoreError> {
        let entry = self.entry(handle)?;
        let raw = tokio::task::spawn_blocking(move || entry.get_secret().map_err(map_keyring_err))
            .await
            .map_err(|e| KeystoreError::Backend(Box::new(e)))??;
        Ok(SecretBytes::new(raw))
    }

    async fn delete_secret(&self, handle: &SecretHandle) -> Result<(), KeystoreError> {
        self.delete_keypair(handle).await
    }

    async fn list_vault_namespaces(
        &self,
        _service_prefix: &str,
    ) -> Result<Vec<VaultId>, KeystoreError> {
        // The `keyring` crate v3 does not expose namespace enumeration on
        // every backend — DPAPI in particular does not support it. Return
        // DiscoveryUnsupported uniformly; per-backend enumeration is a
        // follow-up issue (see spec §3.7 risk note).
        Err(KeystoreError::DiscoveryUnsupported)
    }

    async fn list_identity_versions(
        &self,
        _vault_id: &VaultId,
        _id: &Identity,
    ) -> Result<Vec<KeyVersion>, KeystoreError> {
        Err(KeystoreError::DiscoveryUnsupported)
    }
}
