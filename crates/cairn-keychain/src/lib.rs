//! OS-keychain backed [`Keystore`] for Cairn.
//!
//! Wraps the `keyring` crate to provide per-vault namespaced
//! identity-key + opaque-witness storage. Also offers a file-backed
//! [`FileKeystore`] for headless environments (CI, containers) selectable
//! via the `CAIRN_KEYSTORE` env var.
//!
//! [`Keystore`]: cairn_core::contract::keystore::Keystore

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod file;
mod os;

use std::path::PathBuf;
use std::sync::Arc;

pub use file::FileKeystore;
pub use os::OsKeystore;

use cairn_core::contract::keystore::{Keystore, KeystoreError};
use cairn_core::domain::identity::keys::VaultId;

/// Environment variable that selects the keystore backend.
///
/// Recognized values:
/// - unset / `os` / `keychain` → [`OsKeystore`] (default).
/// - `file` → [`FileKeystore`] at `<vault_root>/.cairn/keystore.json`.
/// - `file:<path>` → [`FileKeystore`] at `<path>` (absolute or relative).
pub const CAIRN_KEYSTORE_ENV: &str = "CAIRN_KEYSTORE";

/// Open a vault-scoped keystore using the backend selected by [`CAIRN_KEYSTORE_ENV`].
///
/// `vault_root` is the on-disk vault directory; the default file backend
/// places its store at `<vault_root>/.cairn/keystore.json`.
///
/// # Errors
/// Returns [`KeystoreError::Backend`] if the file backend cannot read its
/// existing state. The OS backend is constructed lazily and never returns
/// an error from this function.
pub fn keystore_for_vault(
    vault_id: VaultId,
    vault_root: &std::path::Path,
) -> Result<Arc<dyn Keystore>, KeystoreError> {
    match std::env::var(CAIRN_KEYSTORE_ENV).ok().as_deref() {
        None | Some("" | "os" | "keychain") => Ok(Arc::new(OsKeystore::new(vault_id))),
        Some("file") => {
            let path = vault_root.join(".cairn").join("keystore.json");
            Ok(Arc::new(FileKeystore::new(vault_id, path)?))
        }
        Some(other) if let Some(rest) = other.strip_prefix("file:") => {
            let path = PathBuf::from(rest);
            Ok(Arc::new(FileKeystore::new(vault_id, path)?))
        }
        Some(other) => Err(KeystoreError::Backend(
            format!("unknown CAIRN_KEYSTORE value: {other}").into(),
        )),
    }
}

/// Open an unscoped keystore for vault-id discovery, selecting the backend
/// via [`CAIRN_KEYSTORE_ENV`].
///
/// # Errors
/// Returns [`KeystoreError::Backend`] for the file backend's load failure
/// or unknown env values.
pub fn keystore_for_discovery(
    vault_root: &std::path::Path,
) -> Result<Arc<dyn Keystore>, KeystoreError> {
    match std::env::var(CAIRN_KEYSTORE_ENV).ok().as_deref() {
        None | Some("" | "os" | "keychain") => Ok(Arc::new(OsKeystore::for_discovery())),
        Some("file") => {
            let path = vault_root.join(".cairn").join("keystore.json");
            Ok(Arc::new(FileKeystore::for_discovery(path)?))
        }
        Some(other) if let Some(rest) = other.strip_prefix("file:") => {
            let path = PathBuf::from(rest);
            Ok(Arc::new(FileKeystore::for_discovery(path)?))
        }
        Some(other) => Err(KeystoreError::Backend(
            format!("unknown CAIRN_KEYSTORE value: {other}").into(),
        )),
    }
}
