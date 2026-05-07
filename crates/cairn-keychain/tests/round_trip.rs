//! Per-OS round-trip integration test for [`OsKeystore`].
//!
//! Requires a live keychain daemon (macOS Keychain, Secret Service, or Windows
//! Credential Manager).  Run manually with:
//!
//! ```text
//! cargo nextest run -p cairn-keychain --features integration
//! ```

#![cfg(feature = "integration")]
// Integration test binary — docs on test functions would be noise.
#![allow(missing_docs)]

use cairn_core::contract::keystore::{Keystore, KeystoreError};
use cairn_core::domain::identity::{
    Identity,
    keys::{KeyVersion, SecretHandle, SigningKey, VaultId},
};
use cairn_keychain::OsKeystore;

#[tokio::test]
async fn keypair_round_trip() {
    let vault = VaultId::mint();
    let keystore = OsKeystore::new(vault.clone());
    let id = Identity::parse("hmn:test-roundtrip:v1").unwrap();
    let handle = SecretHandle::for_identity(vault, id, KeyVersion::FIRST);

    let mut rng = rand_core::OsRng;
    let key = SigningKey::generate(&mut rng);
    let pub_a = key.verifying_key();
    keystore.store_keypair(&handle, &key).await.unwrap();

    let loaded = keystore.load_signing_key(&handle).await.unwrap();
    assert_eq!(loaded.verifying_key().to_bytes(), pub_a.to_bytes());

    keystore.delete_keypair(&handle).await.unwrap();
    let err = keystore.load_signing_key(&handle).await.unwrap_err();
    assert!(matches!(err, KeystoreError::NotFound));
}

#[tokio::test]
async fn handle_vault_mismatch_rejected() {
    let bound = VaultId::mint();
    let other = VaultId::mint();
    let keystore = OsKeystore::new(bound);
    let id = Identity::parse("hmn:test:v1").unwrap();
    let foreign_handle = SecretHandle::for_identity(other, id, KeyVersion::FIRST);
    let mut rng = rand_core::OsRng;
    let key = SigningKey::generate(&mut rng);
    assert!(keystore.store_keypair(&foreign_handle, &key).await.is_err());
}
