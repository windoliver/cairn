//! Contract surface — traits, plugin registry, capability manifest.
//!
//! Brief §4.1: every contract is a trait, every trait declares
//! `CONTRACT_VERSION`, plugins register through `register_plugin!`.

pub mod agent_provider;
pub mod frontend_adapter;
pub mod llm_provider;
pub mod macros;
pub mod manifest;
pub mod mcp_server;
pub mod memory_store;
pub mod registry;
pub mod sensor_ingress;
pub mod version;
pub mod workflow_orchestrator;
