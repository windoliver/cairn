//! `CredentialStore` — opaque secret vault behind a trait so adapters
//! never see raw bytes outside the borrow handed to them per-call.
//!
//! Issue #130, brief §9.1 source sensors, §19 v0.3.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::error::ConnectorError;

/// Borrowed credential handed to a connector on one call.
///
/// The inner bytes are private; callers access them through [`bytes`].
/// The `Debug` impl never prints actual credential content — it always
/// prints `"<redacted>"` to prevent accidental secret exposure in logs.
///
/// [`bytes`]: CredentialHandle::bytes
pub struct CredentialHandle {
    bytes: Vec<u8>,
}

impl CredentialHandle {
    /// Access the raw credential bytes for this call.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Construct a handle with no credential bytes.
    ///
    /// Used when the framework's poll loop has no credential to hand
    /// (e.g. fixture connectors). Real adapters call
    /// `CredentialStore::get` instead.
    #[must_use]
    pub fn empty() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Construct a handle from raw bytes.
    ///
    /// Called by `CredentialStore` implementations (e.g. T10
    /// `KeychainCredentialStore`) when unwrapping stored secrets.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

impl std::fmt::Debug for CredentialHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialHandle")
            .field("bytes", &"<redacted>")
            .finish()
    }
}

/// Async trait over a secret store that the connector framework calls
/// to obtain, update, or revoke credentials.
///
/// Implementations must be `Send + Sync` — the registry holds them
/// behind an `Arc` and calls them from tokio tasks.
#[async_trait::async_trait]
pub trait CredentialStore: Send + Sync {
    /// Retrieve credentials for `scope`. Returns
    /// [`ConnectorError::AuthExpired`] when no credential is registered
    /// for the scope.
    async fn get(&self, scope: &str) -> Result<Arc<CredentialHandle>, ConnectorError>;

    /// Store or replace credentials for `scope`.
    async fn put(&self, scope: &str, value: Vec<u8>) -> Result<(), ConnectorError>;

    /// Remove credentials for `scope`. A subsequent `get` will return
    /// [`ConnectorError::AuthExpired`].
    async fn delete(&self, scope: &str) -> Result<(), ConnectorError>;
}

/// Process-local in-memory credential store, primarily for tests and
/// fixture connectors.
///
/// Derive [`Default`] so callers can create an empty store with
/// `InMemoryCredentialStore::default()`.
#[derive(Default)]
pub struct InMemoryCredentialStore {
    inner: RwLock<HashMap<String, Vec<u8>>>,
}

#[async_trait::async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn get(&self, scope: &str) -> Result<Arc<CredentialHandle>, ConnectorError> {
        let map = self.inner.read().await;
        match map.get(scope) {
            Some(v) => Ok(Arc::new(CredentialHandle::from_bytes(v.clone()))),
            None => Err(ConnectorError::AuthExpired {
                scope: scope.to_owned(),
            }),
        }
    }

    async fn put(&self, scope: &str, value: Vec<u8>) -> Result<(), ConnectorError> {
        self.inner.write().await.insert(scope.to_owned(), value);
        Ok(())
    }

    async fn delete(&self, scope: &str) -> Result<(), ConnectorError> {
        self.inner.write().await.remove(scope);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_round_trip() {
        let store = InMemoryCredentialStore::default();
        store.put("fixture", b"tok-1".to_vec()).await.unwrap();
        let handle = store.get("fixture").await.unwrap();
        assert_eq!(handle.bytes(), b"tok-1");
    }

    #[tokio::test]
    async fn missing_returns_auth_expired() {
        let store = InMemoryCredentialStore::default();
        match store.get("absent").await {
            Err(crate::error::ConnectorError::AuthExpired { scope }) => {
                assert_eq!(scope, "absent");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_then_get_returns_auth_expired() {
        let store = InMemoryCredentialStore::default();
        store.put("temp-scope", b"secret".to_vec()).await.unwrap();
        // Confirm it's there first
        let handle = store.get("temp-scope").await.unwrap();
        assert_eq!(handle.bytes(), b"secret");
        // Delete it
        store.delete("temp-scope").await.unwrap();
        // Now it must be gone
        match store.get("temp-scope").await {
            Err(crate::error::ConnectorError::AuthExpired { scope }) => {
                assert_eq!(scope, "temp-scope");
            }
            other => panic!("expected AuthExpired, got: {other:?}"),
        }
    }

    #[test]
    fn debug_does_not_leak_bytes() {
        let handle = CredentialHandle::from_bytes(b"super-secret-token".to_vec());
        let debug_str = format!("{handle:?}");
        assert!(debug_str.contains("<redacted>"));
        assert!(!debug_str.contains("super-secret-token"));
    }

    #[test]
    fn empty_handle_has_no_bytes() {
        let handle = CredentialHandle::empty();
        assert_eq!(handle.bytes(), b"");
    }

    #[test]
    fn from_bytes_round_trips() {
        let raw = b"my-credential".to_vec();
        let handle = CredentialHandle::from_bytes(raw.clone());
        assert_eq!(handle.bytes(), raw.as_slice());
    }
}
