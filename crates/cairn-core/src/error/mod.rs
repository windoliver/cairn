//! Top-level error types for `cairn-core`.
//!
//! Each sub-module holds one error enum whose scope matches a contract or
//! service boundary. Contract-level errors (e.g., [`KeystoreError`],
//! [`RegistryError`]) live alongside their traits in [`crate::contract`];
//! this module holds errors whose scope spans multiple contracts.
//!
//! [`KeystoreError`]: crate::contract::keystore::KeystoreError
//! [`RegistryError`]: crate::contract::identity_registry::RegistryError

pub mod identity;

pub use identity::IdentityServiceError;
