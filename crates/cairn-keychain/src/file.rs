//! File-backed [`Keystore`] implementation.
//!
//! Stores secrets as a JSON map at `<keystore_path>`. Intended for
//! headless environments (CI containers, CI runners without a configured
//! Secret Service / Keychain) where the OS keyring is not available.
//!
//! **Security posture:** the file is written with `0o600` permissions but
//! is otherwise not encrypted at rest. This is the same posture as
//! [`crate::OsKeystore`] when an unencrypted keyring is mounted, but
//! callers should not point this implementation at a multi-tenant host.
//! Opt-in only via `CAIRN_KEYSTORE=file` (see [`crate::keystore_for_vault`]).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};

use cairn_core::contract::keystore::{Keystore, KeystoreError};
use cairn_core::domain::identity::{
    Identity,
    keys::{KeyVersion, SecretBytes, SecretHandle, SigningKey, VaultId},
};

/// File-backed keystore scoped to a single vault.
///
/// Concurrency model: every read/write opens the JSON file under an OS-level
/// advisory exclusive lock on a sibling `.lock` file, reloads the on-disk
/// state, mutates, persists via temp-file-with-mode-0600 + fsync + atomic
/// rename + parent dir fsync. The in-memory [`Mutex`] guards in-process
/// concurrent operations on the same handle; the file lock guards
/// cross-process concurrent operations on the same vault.
pub struct FileKeystore {
    bound_vault: Option<VaultId>,
    path: PathBuf,
    lock_path: PathBuf,
    /// In-process lock to keep `&self` async methods safe; cross-process
    /// safety comes from the `fs4` flock on `lock_path` during persist.
    op_lock: Mutex<()>,
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
        // Probe-load to surface parse errors early. The state is re-read
        // under the file lock on every operation.
        let _ = load_or_default(&path)?;
        let lock_path = path.with_extension("json.lock");
        Ok(Self {
            bound_vault: Some(vault_id),
            path,
            lock_path,
            op_lock: Mutex::new(()),
        })
    }

    /// Create an unscoped keystore handle for vault-id discovery.
    ///
    /// # Errors
    /// Returns an error if the file exists but cannot be read or parsed.
    pub fn for_discovery(path: PathBuf) -> Result<Self, KeystoreError> {
        let _ = load_or_default(&path)?;
        let lock_path = path.with_extension("json.lock");
        Ok(Self {
            bound_vault: None,
            path,
            lock_path,
            op_lock: Mutex::new(()),
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

    /// Atomically read the current on-disk state under an exclusive file lock.
    fn locked_read(&self) -> Result<(LockGuard, FileState), KeystoreError> {
        {
            // In-process lock held only long enough to ensure two async tasks
            // on the same handle don't race acquiring the file lock. The
            // returned `LockGuard` owns the cross-process exclusive flock for
            // the caller's scope.
            let _in_proc = self.op_lock.lock().map_err(|_| {
                KeystoreError::Backend("file keystore in-process mutex poisoned".into())
            })?;
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| KeystoreError::Backend(Box::new(e)))?;
            }
        }
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.lock_path)
            .map_err(|e| KeystoreError::Backend(Box::new(e)))?;
        FileExt::lock_exclusive(&lock_file).map_err(|e| KeystoreError::Backend(Box::new(e)))?;
        let state = load_or_default(&self.path)?;
        Ok((LockGuard { file: lock_file }, state))
    }

    /// Persist `state` under the existing file lock. Caller must hold a
    /// `LockGuard` returned by [`Self::locked_read`].
    ///
    /// Uses a per-call random temp filename + `O_CREAT | O_EXCL` so a
    /// pre-existing world-readable temp file or symlink at a predictable
    /// path cannot be used to siphon secrets or redirect the write. The
    /// rename target is the keystore path itself, which is also kept at
    /// 0600 by virtue of the temp file's permissions.
    fn locked_write(&self, _guard: &LockGuard, state: &FileState) -> Result<(), KeystoreError> {
        let bytes =
            serde_json::to_vec_pretty(state).map_err(|e| KeystoreError::Backend(Box::new(e)))?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| KeystoreError::Backend("keystore path has no parent".into()))?;
        std::fs::create_dir_all(parent).map_err(|e| KeystoreError::Backend(Box::new(e)))?;

        // Build a per-call random temp filename in the same dir so the
        // rename is atomic. `O_EXCL` (`create_new`) refuses to follow an
        // existing path or symlink — if the path is occupied we retry with
        // a fresh suffix, bounded by a small attempt limit.
        let stem = self
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("keystore.json");
        let mut last_err: Option<std::io::Error> = None;
        let mut tmp_path: Option<PathBuf> = None;
        let mut tmp_file_opt: Option<File> = None;
        for attempt in 0..8u32 {
            let suffix = random_suffix();
            let candidate = parent.join(format!(".{stem}.{suffix}.tmp"));
            let mut opts = OpenOptions::new();
            opts.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                opts.mode(0o600);
            }
            match opts.open(&candidate) {
                Ok(f) => {
                    tmp_file_opt = Some(f);
                    tmp_path = Some(candidate);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    let _ = attempt; // retry with a fresh suffix
                }
            }
        }
        let mut tmp_file = tmp_file_opt.ok_or_else(|| {
            KeystoreError::Backend(last_err.map_or_else(
                || "could not create exclusive temp file".into(),
                |e| format!("could not create exclusive temp file: {e}").into(),
            ))
        })?;
        let Some(tmp) = tmp_path else {
            // Invariant: tmp_path is set whenever tmp_file_opt is set.
            return Err(KeystoreError::Backend(
                "internal invariant: tmp_path unset after successful open".into(),
            ));
        };

        let write_result = tmp_file
            .write_all(&bytes)
            .and_then(|()| tmp_file.sync_all());
        if let Err(e) = write_result {
            // Best-effort cleanup of the temp file on failure so we don't
            // leak partial-write material into the vault.
            let _ = std::fs::remove_file(&tmp);
            return Err(KeystoreError::Backend(Box::new(e)));
        }
        drop(tmp_file);

        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(KeystoreError::Backend(Box::new(e)));
        }

        // Fsync the parent directory so the rename is durable across crashes.
        #[cfg(unix)]
        {
            let dir = File::open(parent).map_err(|e| KeystoreError::Backend(Box::new(e)))?;
            dir.sync_all()
                .map_err(|e| KeystoreError::Backend(Box::new(e)))?;
        }
        Ok(())
    }
}

/// RAII handle holding the file lock; releases on drop.
struct LockGuard {
    file: File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Generate a short random suffix for the temp filename. Uses
/// `std::time` and process id for entropy — does not need
/// cryptographic randomness because the file lock guarantees only one
/// writer at a time and `O_EXCL` covers any residual collision.
fn random_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let pid = std::process::id();
    format!("{nanos:x}-{pid:x}")
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
        let (guard, mut state) = self.locked_read()?;
        state.entries.insert(key, b64_encode(&bytes));
        self.locked_write(&guard, &state)?;
        Ok(())
    }

    async fn load_signing_key(&self, handle: &SecretHandle) -> Result<SigningKey, KeystoreError> {
        self.ensure_bound_match(handle)?;
        let key = Self::key_for(handle);
        let (_guard, state) = self.locked_read()?;
        let raw_b64 = state.entries.get(&key).ok_or(KeystoreError::NotFound)?;
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
        let (guard, mut state) = self.locked_read()?;
        state.entries.remove(&key);
        self.locked_write(&guard, &state)?;
        Ok(())
    }

    async fn store_secret(&self, handle: &SecretHandle, bytes: &[u8]) -> Result<(), KeystoreError> {
        self.ensure_bound_match(handle)?;
        let key = Self::key_for(handle);
        let (guard, mut state) = self.locked_read()?;
        state.entries.insert(key, b64_encode(bytes));
        self.locked_write(&guard, &state)?;
        Ok(())
    }

    async fn load_secret(&self, handle: &SecretHandle) -> Result<SecretBytes, KeystoreError> {
        self.ensure_bound_match(handle)?;
        let key = Self::key_for(handle);
        let (_guard, state) = self.locked_read()?;
        let raw_b64 = state.entries.get(&key).ok_or(KeystoreError::NotFound)?;
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

    /// A stale predictable-name temp file in the keystore directory must
    /// NOT block `store_secret` — the per-call random suffix + `O_EXCL`
    /// makes the write resilient.
    #[tokio::test]
    async fn store_succeeds_when_old_predictable_tmp_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keystore.json");
        // Pre-create a world-readable "keystore.json.tmp" — the original
        // predictable name. The new write must not use it.
        std::fs::write(dir.path().join("keystore.json.tmp"), b"junk").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                dir.path().join("keystore.json.tmp"),
                std::fs::Permissions::from_mode(0o666),
            )
            .unwrap();
        }
        let v = fresh_vault();
        let handle = SecretHandle::for_witness(v.clone());
        let ks = FileKeystore::new(v, path.clone()).unwrap();
        let key = SigningKey::generate(&mut OsRng);
        ks.store_secret(&handle, &key.expose_secret_bytes())
            .await
            .unwrap();

        // The final keystore.json must be 0o600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "keystore.json permissions: {mode:o}");
        }
    }
}
