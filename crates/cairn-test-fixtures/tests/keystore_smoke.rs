//! Smoke tests for [`MemoryKeystore`].
//!
//! Verifies the basic store/load/delete round-trip and the locked-state knob.

#![allow(missing_docs)]

use cairn_core::contract::keystore::{Keystore, KeystoreError};
use cairn_core::domain::identity::{
    Identity,
    keys::{KeyVersion, SecretHandle, SigningKey, VaultId},
};
use cairn_test_fixtures::MemoryKeystore;

#[tokio::test]
async fn memory_keystore_round_trip() {
    let v = VaultId::mint();
    let id = Identity::parse("hmn:alice:v1").unwrap();
    let h = SecretHandle::for_identity(v, id, KeyVersion::FIRST);
    let ks = MemoryKeystore::new();
    let mut rng = rand_core::OsRng;
    let key = SigningKey::generate(&mut rng);
    ks.store_keypair(&h, &key).await.unwrap();
    let loaded = ks.load_signing_key(&h).await.unwrap();
    assert_eq!(
        loaded.verifying_key().to_bytes(),
        key.verifying_key().to_bytes()
    );
}

#[tokio::test]
async fn memory_keystore_locked_returns_err() {
    let v = VaultId::mint();
    let id = Identity::parse("hmn:bob:v1").unwrap();
    let h = SecretHandle::for_identity(v, id, KeyVersion::FIRST);
    let ks = MemoryKeystore::new();
    ks.lock();
    let mut rng = rand_core::OsRng;
    let key = SigningKey::generate(&mut rng);
    let err = ks.store_keypair(&h, &key).await.unwrap_err();
    assert!(matches!(err, KeystoreError::Locked));
}

#[tokio::test]
async fn memory_keystore_not_found_after_delete() {
    let v = VaultId::mint();
    let id = Identity::parse("hmn:carol:v1").unwrap();
    let h = SecretHandle::for_identity(v, id, KeyVersion::FIRST);
    let ks = MemoryKeystore::new();
    let mut rng = rand_core::OsRng;
    let key = SigningKey::generate(&mut rng);
    ks.store_keypair(&h, &key).await.unwrap();
    ks.delete_keypair(&h).await.unwrap();
    let err = ks.load_signing_key(&h).await.unwrap_err();
    assert!(matches!(err, KeystoreError::NotFound));
}
