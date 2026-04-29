//! In-process [`Keystore`] implementation for tests.
//!
//! [`MemoryKeystore`] stores all cryptographic material in a `HashMap`
//! protected by a `parking_lot::RwLock`. No filesystem, no OS keychain,
//! and no process isolation — ideal for unit and integration tests that
//! want deterministic, fast key operations without ambient OS credentials.
//!
//! [`Keystore`]: cairn_core::contract::keystore::Keystore

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use cairn_core::contract::keystore::{Keystore, KeystoreError};
use cairn_core::domain::identity::{
    Identity,
    keys::{KeyVersion, SecretBytes, SecretHandle, SigningKey, VaultId},
};

/// Shared map of `(service, account)` → raw key bytes.
type KeyMap = Arc<RwLock<HashMap<(String, String), Vec<u8>>>>;

/// Shared boolean flag.
type Flag = Arc<RwLock<bool>>;

/// An in-process [`Keystore`] backed by a `HashMap`.
///
/// All state is stored in `Arc<RwLock<…>>` so clones share the same
/// underlying storage — handy when a single keystore instance must be
/// handed to multiple components under test.
///
/// # Knobs for failure injection
///
/// - [`MemoryKeystore::lock`] / [`MemoryKeystore::unlock`] — toggle the
///   locked state.  While locked, every read and write method returns
///   [`KeystoreError::Locked`].
/// - [`MemoryKeystore::set_discovery_unsupported`] — when `true`,
///   [`Keystore::list_vault_namespaces`] and
///   [`Keystore::list_identity_versions`] return
///   [`KeystoreError::DiscoveryUnsupported`], simulating backends (e.g.,
///   DPAPI) that do not support enumeration.
#[derive(Default, Clone)]
pub struct MemoryKeystore {
    /// Raw key material, keyed by `(service, account)`.
    inner: KeyMap,
    /// Whether the store is currently locked.
    locked: Flag,
    /// Whether discovery operations are disabled.
    discovery_unsupported: Flag,
}

impl MemoryKeystore {
    /// Create a new, empty, unlocked keystore.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lock the keystore.  All subsequent read/write operations will return
    /// [`KeystoreError::Locked`] until [`MemoryKeystore::unlock`] is called.
    pub fn lock(&self) {
        *self.locked.write() = true;
    }

    /// Unlock the keystore.
    pub fn unlock(&self) {
        *self.locked.write() = false;
    }

    /// Control whether discovery operations (`list_vault_namespaces`,
    /// `list_identity_versions`) return [`KeystoreError::DiscoveryUnsupported`].
    pub fn set_discovery_unsupported(&self, v: bool) {
        *self.discovery_unsupported.write() = v;
    }

    /// Return the raw `(service, account)` pairs currently stored.
    ///
    /// Useful in tests that want to assert on key presence without going
    /// through the public `Keystore` API.
    #[must_use]
    pub fn raw_keys(&self) -> Vec<(String, String)> {
        self.inner.read().keys().cloned().collect()
    }
}

/// Map a [`SecretHandle`] to the `(service, account)` pair used as the
/// `HashMap` key.
fn key_of(handle: &SecretHandle) -> (String, String) {
    (handle.service(), handle.account_string())
}

#[async_trait]
impl Keystore for MemoryKeystore {
    async fn store_keypair(
        &self,
        handle: &SecretHandle,
        secret: &SigningKey,
    ) -> Result<(), KeystoreError> {
        if *self.locked.read() {
            return Err(KeystoreError::Locked);
        }
        self.inner
            .write()
            .insert(key_of(handle), secret.expose_secret_bytes().to_vec());
        Ok(())
    }

    async fn load_signing_key(&self, handle: &SecretHandle) -> Result<SigningKey, KeystoreError> {
        if *self.locked.read() {
            return Err(KeystoreError::Locked);
        }
        let raw = self
            .inner
            .read()
            .get(&key_of(handle))
            .cloned()
            .ok_or(KeystoreError::NotFound)?;
        let arr: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| KeystoreError::Backend("bad key bytes: expected 32".into()))?;
        Ok(SigningKey::from_bytes(&arr))
    }

    async fn delete_keypair(&self, handle: &SecretHandle) -> Result<(), KeystoreError> {
        if *self.locked.read() {
            return Err(KeystoreError::Locked);
        }
        self.inner.write().remove(&key_of(handle));
        Ok(())
    }

    async fn store_secret(&self, handle: &SecretHandle, bytes: &[u8]) -> Result<(), KeystoreError> {
        if *self.locked.read() {
            return Err(KeystoreError::Locked);
        }
        self.inner.write().insert(key_of(handle), bytes.to_vec());
        Ok(())
    }

    async fn load_secret(&self, handle: &SecretHandle) -> Result<SecretBytes, KeystoreError> {
        if *self.locked.read() {
            return Err(KeystoreError::Locked);
        }
        let raw = self
            .inner
            .read()
            .get(&key_of(handle))
            .cloned()
            .ok_or(KeystoreError::NotFound)?;
        Ok(SecretBytes::new(raw))
    }

    async fn delete_secret(&self, handle: &SecretHandle) -> Result<(), KeystoreError> {
        // Secrets and keypairs share the same slot — delegate to the keypair path.
        self.delete_keypair(handle).await
    }

    async fn list_vault_namespaces(
        &self,
        service_prefix: &str,
    ) -> Result<Vec<VaultId>, KeystoreError> {
        if *self.discovery_unsupported.read() {
            return Err(KeystoreError::DiscoveryUnsupported);
        }
        let mut out = Vec::new();
        for (svc, acct) in self.inner.read().keys() {
            if svc.starts_with(service_prefix) && acct == "__vault_witness__" {
                // Service is `cairn:<vault_id>`; strip the prefix to get the raw ULID.
                let ulid_str = svc.strip_prefix("cairn:").unwrap_or(svc.as_str());
                if let Ok(id) = VaultId::parse(ulid_str) {
                    out.push(id);
                }
            }
        }
        Ok(out)
    }

    async fn list_identity_versions(
        &self,
        vault_id: &VaultId,
        id: &Identity,
    ) -> Result<Vec<KeyVersion>, KeystoreError> {
        if *self.discovery_unsupported.read() {
            return Err(KeystoreError::DiscoveryUnsupported);
        }
        let svc = format!("cairn:{vault_id}");
        // Account string for a versioned keypair is `<identity-wire>#k<version>`.
        let prefix = format!("{id}#k");
        let mut out = Vec::new();
        for (s, a) in self.inner.read().keys() {
            if s == &svc
                && let Some(ver_str) = a.strip_prefix(&prefix)
                && let Ok(n) = ver_str.parse::<u32>()
                && let Some(nz) = std::num::NonZeroU32::new(n)
            {
                out.push(KeyVersion::new(nz));
            }
        }
        out.sort_by_key(|k| k.as_u32());
        Ok(out)
    }
}
