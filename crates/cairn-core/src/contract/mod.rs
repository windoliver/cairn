//! Cairn contract surface.
//!
//! Brief §4.1 — every contract is a trait, every trait declares
//! `CONTRACT_VERSION`, plugins register through `register_plugin!` and end
//! up in a [`registry::PluginRegistry`] the host assembles at startup.
//!
//! Public surface:
//! - [`version::ContractVersion`], [`version::VersionRange`]
//! - [`registry::PluginRegistry`], [`registry::PluginName`],
//!   [`registry::PluginError`]
//! - [`manifest::PluginManifest`], [`manifest::ContractKind`]
//! - One module per contract: [`memory_store`], [`llm_provider`],
//!   [`workflow_orchestrator`], [`sensor_ingress`], [`mcp_server`],
//!   [`frontend_adapter`] (P1), [`agent_provider`] (P2)

pub mod agent_provider;
pub mod frontend_adapter;
pub mod llm_provider;
pub mod manifest;
pub mod mcp_server;
pub mod memory_store;
pub mod registry;
pub mod sensor_ingress;
pub mod version;
pub mod workflow_orchestrator;

#[doc(hidden)]
pub mod macros;
