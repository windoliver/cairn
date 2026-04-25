//! Compile-path test: every #174 acceptance trait is reachable directly from
//! `cairn_core::contract::*` without dipping into submodules.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_core::contract::{
    AgentProvider, AgentProviderCapabilities, ContractKind, ContractVersion, FrontendAdapter,
    FrontendAdapterCapabilities, LLMProvider, LLMProviderCapabilities, MCPServer,
    MCPServerCapabilities, MemoryStore, MemoryStoreCapabilities, PluginError, PluginManifest,
    PluginName, PluginRegistry, SensorIngress, SensorIngressCapabilities, VersionRange,
    WorkflowOrchestrator, WorkflowOrchestratorCapabilities,
};

#[test]
fn registry_constructible() {
    let _: PluginRegistry = PluginRegistry::new();
}

#[test]
fn version_constructible() {
    let _: ContractVersion = ContractVersion::new(0, 1, 0);
    let _: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

#[test]
fn capability_structs_default() {
    let _: MemoryStoreCapabilities = MemoryStoreCapabilities::default();
    let _: LLMProviderCapabilities = LLMProviderCapabilities::default();
    let _: WorkflowOrchestratorCapabilities = WorkflowOrchestratorCapabilities::default();
    let _: SensorIngressCapabilities = SensorIngressCapabilities::default();
    let _: MCPServerCapabilities = MCPServerCapabilities::default();
    let _: FrontendAdapterCapabilities = FrontendAdapterCapabilities::default();
    let _: AgentProviderCapabilities = AgentProviderCapabilities::default();
}

mod compile_only {
    use super::*;

    pub fn accepts_dyn_memory_store(_: &dyn MemoryStore) {}
    pub fn accepts_dyn_llm_provider(_: &dyn LLMProvider) {}
    pub fn accepts_dyn_workflow_orchestrator(_: &dyn WorkflowOrchestrator) {}
    pub fn accepts_dyn_sensor_ingress(_: &dyn SensorIngress) {}
    pub fn accepts_dyn_mcp_server(_: &dyn MCPServer) {}
    pub fn accepts_dyn_frontend_adapter(_: &dyn FrontendAdapter) {}
    pub fn accepts_dyn_agent_provider(_: &dyn AgentProvider) {}
}

// Type-level proof the traits resolve via the contract root: these functions
// compile only if the traits are re-exported from `cairn_core::contract`.
#[test]
fn dyn_dispatch_compiles() {
    // Keep symbols live for clippy by taking their addresses.
    let _ = compile_only::accepts_dyn_memory_store as fn(_);
    let _ = compile_only::accepts_dyn_llm_provider as fn(_);
    let _ = compile_only::accepts_dyn_workflow_orchestrator as fn(_);
    let _ = compile_only::accepts_dyn_sensor_ingress as fn(_);
    let _ = compile_only::accepts_dyn_mcp_server as fn(_);
    let _ = compile_only::accepts_dyn_frontend_adapter as fn(_);
    let _ = compile_only::accepts_dyn_agent_provider as fn(_);
}

#[test]
fn manifest_kinds_are_complete() {
    let kinds = [
        ContractKind::MemoryStore,
        ContractKind::LLMProvider,
        ContractKind::WorkflowOrchestrator,
        ContractKind::SensorIngress,
        ContractKind::MCPServer,
        ContractKind::FrontendAdapter,
        ContractKind::AgentProvider,
    ];
    assert_eq!(kinds.len(), 7);
}

#[test]
fn plugin_error_constructible() {
    // Force compile-time check: PluginError and PluginManifest are reachable.
    let _: Result<PluginName, PluginError> = PluginName::new("good-name");
    let _: Result<PluginManifest, PluginError> = PluginManifest::parse_toml("");
}
