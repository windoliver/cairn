//! Contract surface — traits, plugin registry, capability manifest.
//!
//! Brief §4.1: every contract is a trait, every trait declares
//! `CONTRACT_VERSION`, plugins register through `register_plugin!`.

pub mod llm_provider;
pub mod memory_store;
pub mod registry;
pub mod version;
