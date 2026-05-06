//! Top-level error types for `cairn-core`.
//!
//! Each sub-module holds one error enum or surface whose scope matches a
//! contract or service boundary. Contract-level errors (e.g.,
//! [`KeystoreError`], [`RegistryError`]) live alongside their traits in
//! [`crate::contract`]; this module holds errors and translation layers
//! whose scope spans multiple contracts.
//!
//! [`KeystoreError`]: crate::contract::keystore::KeystoreError
//! [`RegistryError`]: crate::contract::identity_registry::RegistryError

pub mod flush_plan;
pub mod identity;
pub mod wire;

pub use flush_plan::FlushPlanError;
pub use identity::IdentityServiceError;
pub use wire::{ErrorBody, envelope_error_for};
