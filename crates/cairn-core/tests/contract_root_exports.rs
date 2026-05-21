//! Compile-path test: every #174 acceptance trait is reachable directly from
//! `cairn_core::contract::*` without dipping into submodules.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_core::contract::{
    AgentBudgetConsumed, AgentCostBudget, AgentIdentity, AgentOutput, AgentOutputSchema,
    AgentProvider, AgentProviderCapabilities, AgentProviderError, AgentRun, AgentRunMeter,
    AgentRunStatus, AgentScope, AgentSpawnRequest, AgentToolAllowlist, AgentToolAttempt,
    AgentToolCall, AgentToolPolicyOutcome, AgentWallClockBudget, CairnVerb, ContractKind,
    ContractVersion, FrontendAdapter, FrontendAdapterCapabilities, FrontendFieldClass,
    FrontendFieldPolicy, FrontendReconcileError, LLMProvider, LLMProviderCapabilities, MCPServer,
    MCPServerCapabilities, MemoryStore, MemoryStoreCapabilities, PluginError, PluginManifest,
    PluginName, PluginRegistry, SensorIngress, SensorIngressCapabilities, VersionRange,
    WorkflowOrchestrator, WorkflowOrchestratorCapabilities, evaluate_tool_policy, validate_output,
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

#[test]
fn agent_provider_contract_types_constructible_from_root() {
    let identity = AgentIdentity::new("agt:root-export:v1").expect("valid identity");
    let request = AgentSpawnRequest {
        identity,
        scope: AgentScope::read_only(),
        tool_allowlist: AgentToolAllowlist::read_only_cairn(),
        cost_budget: AgentCostBudget {
            max_turns: 1,
            max_tool_calls: 1,
            max_cost_units: 1,
        },
        wall_clock_budget: AgentWallClockBudget { max_millis: 1 },
        output_schema: AgentOutputSchema::Text,
        prompt: "root export".to_string(),
    };
    let mut meter = AgentRunMeter::new(&request);
    meter.charge_turn(1).expect("turn budget available");
    let attempt = AgentToolAttempt {
        call: AgentToolCall::new(CairnVerb::Search),
        outcome: AgentToolPolicyOutcome::AllowedReadOnly,
        reason: "allowed".to_string(),
    };
    let run = AgentRun {
        status: AgentRunStatus::Completed,
        abort_error: None,
        output: AgentOutput::Text("ok".to_string()),
        budget_consumed: AgentBudgetConsumed {
            turns: 1,
            tool_calls: 0,
            cost_units: 0,
        },
        tool_calls: vec![attempt],
        policy_trace: Vec::new(),
    };
    assert_eq!(run.status, AgentRunStatus::Completed);
    assert!(matches!(
        evaluate_tool_policy(&request, &AgentToolCall::new(CairnVerb::Search)),
        Ok(AgentToolPolicyOutcome::AllowedReadOnly)
    ));
    validate_output(&AgentOutputSchema::Text, &run.output).expect("text output validates");
    let err = AgentProviderError::ToolNotAllowed {
        verb: CairnVerb::Forget,
    };
    assert!(err.to_string().contains("tool not allowed"));
    let budget_err = AgentProviderError::BudgetExceeded {
        limit: "turns".to_string(),
    };
    assert!(budget_err.to_string().contains("turns"));
}

#[test]
fn frontend_contract_types_are_reachable() {
    let _: FrontendAdapterCapabilities = FrontendAdapterCapabilities::default();
    let _: FrontendFieldClass = FrontendFieldClass::UserContent;
    let _ = FrontendFieldPolicy::is_mutable_from_frontend("body");
    let _: FrontendReconcileError = FrontendReconcileError::UnsignedIntent;
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
