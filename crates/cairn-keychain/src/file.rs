//! File-backed [`Keystore`] implementation.
//!
//! Stores secrets as a JSON map at `<keystore_path>`. Intended for
//! headless environments (CI containers, CI runners without a configured
//! Secret Service / Keychain) where the OS keyring is not available.
//!
//! **Security posture:** the file is written with `0o600` permissions but
//! is otherwise not encrypted at rest. This is the same posture as
//! [`OsKeystore`] when an unencrypted keyring is mounted, but callers
//! should not point this implementation at a multi-tenant host. Opt-in
//! only via `CAIRN_KEYSTORE=file` (see `cairn-keychain::keystore_for_vault`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use cairn_core::contract::keystore::{Keystore, KeystoreError};
use cairn_core::domain::identity::{
    Identity,
    keys::{KeyVersion, SecretBytes, SecretHandle, SigningKey, VaultId},
};

/// File-backed keystore scoped to a single vault.
pub struct FileKeystore {
    bound_vault: Option<VaultId>,
    path: PathBuf,
    state: Mutex<FileState>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileState {
    /// Map of `service|account` → base64-encoded secret bytes.
    entries: HashMap<String, String>,
}

impl FileKeystore {
    /// Create a vault-scoped keystore writing to `path`.
    ///
    /// # Errors
    /// Returns an error if the file exists but cannot be read or parsed.
    pub fn new(vault_id: VaultId, path: PathBuf) -> Result<Self, KeystoreError> {
        let state = load_or_default(&path)?;
        Ok(Self {
            bound_vault: Some(vault_id),
            path,
            state: Mutex::new(state),
        })
    }

    /// Create an unscoped keystore handle for vault-id discovery.
    ///
    /// # Errors
    /// Returns an error if the file exists but cannot be read or parsed.
    pub fn for_discovery(path: PathBuf) -> Result<Self, KeystoreError> {
        let state = load_or_default(&path)?;
        Ok(Self {
            bound_vault: None,
            path,
            state: Mutex::new(state),
        })
    }

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

    fn key_for(handle: &SecretHandle) -> String {
        format!("{}|{}", handle.service(), handle.account_string())
    }

    fn persist(&self, state: &FileState) -> Result<(), KeystoreError> {
        let bytes =
            serde_json::to_vec_pretty(state).map_err(|e| KeystoreError::Backend(Box::new(e)))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| KeystoreError::Backend(Box::new(e)))?;
        }
        // Write to a sibling tempfile + atomic rename so a crash mid-write
        // leaves the previous content intact.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| KeystoreError::Backend(Box::new(e)))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = std::fs::metadata(&tmp)
                .map_err(|e| KeystoreError::Backend(Box::new(e)))?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&tmp, perms)
                .map_err(|e| KeystoreError::Backend(Box::new(e)))?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| KeystoreError::Backend(Box::new(e)))?;
        Ok(())
    }
}

fn load_or_default(path: &Path) -> Result<FileState, KeystoreError> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(FileState::default()),
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| KeystoreError::Backend(Box::new(e)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileState::default()),
        Err(e) => Err(KeystoreError::Backend(Box::new(e))),
    }
}

fn b64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn b64_decode(s: &str) -> Result<Vec<u8>, KeystoreError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| KeystoreError::Backend(format!("base64 decode: {e}").into()))
}

#[async_trait]
impl Keystore for FileKeystore {
    async fn store_keypair(
        &self,
        handle: &SecretHandle,
        secret: &SigningKey,
    ) -> Result<(), KeystoreError> {
        self.ensure_bound_match(handle)?;
        let key = Self::key_for(handle);
        let bytes = secret.expose_secret_bytes();
        let mut guard = self
            .state
            .lock()
            .map_err(|_| KeystoreError::Backend("file keystore mutex poisoned".into()))?;
        guard.entries.insert(key, b64_encode(&bytes));
        self.persist(&guard)?;
        Ok(())
    }

    async fn load_signing_key(&self, handle: &SecretHandle) -> Result<SigningKey, KeystoreError> {
        self.ensure_bound_match(handle)?;
        let key = Self::key_for(handle);
        let guard = self
            .state
            .lock()
            .map_err(|_| KeystoreError::Backend("file keystore mutex poisoned".into()))?;
        let raw_b64 = guard.entries.get(&key).ok_or(KeystoreError::NotFound)?;
        let raw = b64_decode(raw_b64)?;
        let arr: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| KeystoreError::Backend("signing key bytes != 32".into()))?;
        Ok(SigningKey::from_bytes(&arr))
    }

    async fn delete_keypair(&self, handle: &SecretHandle) -> Result<(), KeystoreError> {
        self.ensure_bound_match(handle)?;
        let key = Self::key_for(handle);
        let mut guard = self
            .state
            .lock()
            .map_err(|_| KeystoreError::Backend("file keystore mutex poisoned".into()))?;
        guard.entries.remove(&key);
        self.persist(&guard)?;
        Ok(())
    }

    async fn store_secret(&self, handle: &SecretHandle, bytes: &[u8]) -> Result<(), KeystoreError> {
        self.ensure_bound_match(handle)?;
        let key = Self::key_for(handle);
        let mut guard = self
            .state
            .lock()
            .map_err(|_| KeystoreError::Backend("file keystore mutex poisoned".into()))?;
        guard.entries.insert(key, b64_encode(bytes));
        self.persist(&guard)?;
        Ok(())
    }

    async fn load_secret(&self, handle: &SecretHandle) -> Result<SecretBytes, KeystoreError> {
        self.ensure_bound_match(handle)?;
        let key = Self::key_for(handle);
        let guard = self
            .state
            .lock()
            .map_err(|_| KeystoreError::Backend("file keystore mutex poisoned".into()))?;
        let raw_b64 = guard.entries.get(&key).ok_or(KeystoreError::NotFound)?;
        let raw = b64_decode(raw_b64)?;
        Ok(SecretBytes::new(raw))
    }

    async fn delete_secret(&self, handle: &SecretHandle) -> Result<(), KeystoreError> {
        self.delete_keypair(handle).await
    }

    async fn list_vault_namespaces(
        &self,
        _service_prefix: &str,
    ) -> Result<Vec<VaultId>, KeystoreError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;

    fn fresh_vault() -> VaultId {
        VaultId::mint()
    }

    #[tokio::test]
    async fn round_trip_keypair() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keystore.json");
        let v = fresh_vault();
        let ks = FileKeystore::new(v.clone(), path).unwrap();
        let key = SigningKey::generate(&mut OsRng);
        let handle = SecretHandle::for_witness(v.clone());
        ks.store_secret(&handle, &key.expose_secret_bytes())
            .await
            .unwrap();
        let loaded = ks.load_secret(&handle).await.unwrap();
        assert_eq!(loaded.as_slice(), key.expose_secret_bytes().as_slice());
    }

    #[tokio::test]
    async fn missing_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keystore.json");
        let v = fresh_vault();
        let ks = FileKeystore::new(v.clone(), path).unwrap();
        let handle = SecretHandle::for_witness(v);
        let err = ks.load_secret(&handle).await.unwrap_err();
        assert!(matches!(err, KeystoreError::NotFound));
    }

    #[tokio::test]
    async fn delete_then_load_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keystore.json");
        let v = fresh_vault();
        let ks = FileKeystore::new(v.clone(), path).unwrap();
        let key = SigningKey::generate(&mut OsRng);
        let handle = SecretHandle::for_witness(v.clone());
        ks.store_secret(&handle, &key.expose_secret_bytes())
            .await
            .unwrap();
        ks.delete_secret(&handle).await.unwrap();
        let err = ks.load_secret(&handle).await.unwrap_err();
        assert!(matches!(err, KeystoreError::NotFound));
    }

    #[tokio::test]
    async fn persists_across_handle_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keystore.json");
        let v = fresh_vault();
        let handle = SecretHandle::for_witness(v.clone());

        let ks1 = FileKeystore::new(v.clone(), path.clone()).unwrap();
        let key = SigningKey::generate(&mut OsRng);
        ks1.store_secret(&handle, &key.expose_secret_bytes())
            .await
            .unwrap();
        drop(ks1);

        let ks2 = FileKeystore::new(v, path).unwrap();
        let loaded = ks2.load_secret(&handle).await.unwrap();
        assert_eq!(loaded.as_slice(), key.expose_secret_bytes().as_slice());
    }
}
