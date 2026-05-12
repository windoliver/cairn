//! OS-keychain backed [`Keystore`] for Cairn.
//!
//! Wraps the `keyring` crate to provide per-vault namespaced
//! identity-key + opaque-witness storage.
//!
//! [`Keystore`]: cairn_core::contract::keystore::Keystore

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod os;
pub use os::OsKeystore;
