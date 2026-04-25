//! Cairn contract surface.
//!
//! Brief §4.1 — every contract is a trait, every trait declares
//! `CONTRACT_VERSION`, plugins register through `register_plugin!` and end
//! up in a [`registry::PluginRegistry`] the host assembles at startup.
//!
//! Public surface (re-exported from this module):
//! - [`ContractVersion`], [`VersionRange`] — versioning primitives (full path: [`version`]).
//! - [`PluginRegistry`], [`PluginName`], [`PluginError`] — runtime registry (full path: [`registry`]).
//! - [`PluginManifest`], [`ContractKind`] — capability manifest (full path: [`manifest`]).
//! - Five P0 contract traits + their capability structs:
//!   [`MemoryStore`] / [`MemoryStoreCapabilities`],
//!   [`LLMProvider`] / [`LLMProviderCapabilities`],
//!   [`WorkflowOrchestrator`] / [`WorkflowOrchestratorCapabilities`],
//!   [`SensorIngress`] / [`SensorIngressCapabilities`],
//!   [`McpServer`] / [`McpServerCapabilities`].
//! - Forward stubs (P1/P2, hidden until #113 / #124): `FrontendAdapter`, `AgentProvider`.

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

// Flat re-exports so users can write `cairn_core::contract::MemoryStore`
// instead of `cairn_core::contract::memory_store::MemoryStore`.
pub use manifest::{ContractKind, PluginManifest};
pub use registry::{PluginError, PluginName, PluginRegistry};
pub use version::{ContractVersion, VersionRange};

pub use agent_provider::{AgentProvider, AgentProviderCapabilities};
pub use frontend_adapter::{FrontendAdapter, FrontendAdapterCapabilities};
pub use llm_provider::{LLMProvider, LLMProviderCapabilities};
pub use mcp_server::{McpServer, McpServerCapabilities};
pub use memory_store::{MemoryStore, MemoryStoreCapabilities};
pub use sensor_ingress::{SensorIngress, SensorIngressCapabilities};
pub use workflow_orchestrator::{WorkflowOrchestrator, WorkflowOrchestratorCapabilities};
