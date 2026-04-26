//! Tier-1 conformance: ensure the four core cases pass against a
//! well-formed stub plugin registered with a matching manifest. The
//! cases are: `manifest_matches_host`, `arc_pointer_stable`,
//! `capability_self_consistency_floor`,
//! `manifest_features_match_capabilities`.

use std::sync::Arc;

use cairn_core::contract::conformance::{CaseStatus, Tier, run_conformance_for_plugin};
use cairn_core::contract::manifest::PluginManifest;
use cairn_core::contract::mcp_server::{MCPServer, MCPServerCapabilities};
use cairn_core::contract::memory_store::{MemoryStore, MemoryStoreCapabilities};
use cairn_core::contract::registry::{PluginName, PluginRegistry};
use cairn_core::contract::sensor_ingress::{SensorIngress, SensorIngressCapabilities};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::contract::workflow_orchestrator::{
    WorkflowOrchestrator, WorkflowOrchestratorCapabilities,
};

const STORE_MANIFEST: &str = r#"
name = "stub-store"
contract = "MemoryStore"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
fts = false
vector = false
graph_edges = false
transactions = false
"#;

#[derive(Default)]
struct StubStore;

#[async_trait::async_trait]
impl MemoryStore for StubStore {
    fn name(&self) -> &'static str {
        "stub-store"
    }
    fn capabilities(&self) -> &MemoryStoreCapabilities {
        // `Default::default()` is not const, so use an explicit literal.
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: false,
            vector: false,
            graph_edges: false,
            transactions: false,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
}

#[test]
fn tier1_cases_pass_for_well_formed_memory_store() {
    let mut reg = PluginRegistry::new();
    let name = PluginName::new("stub-store").expect("valid");
    let manifest = PluginManifest::parse_toml(STORE_MANIFEST).expect("manifest parses");
    reg.register_memory_store_with_manifest(name.clone(), manifest, Arc::new(StubStore))
        .expect("registers");

    let outcomes = run_conformance_for_plugin(&reg, &name);

    let tier1: Vec<_> = outcomes.iter().filter(|o| o.tier == Tier::One).collect();
    assert_eq!(tier1.len(), 4, "expect 4 tier-1 cases");
    for outcome in &tier1 {
        assert!(
            matches!(outcome.status, CaseStatus::Ok),
            "tier-1 case {} must pass, got {:?}",
            outcome.id,
            outcome.status
        );
    }

    let ids: Vec<_> = tier1.iter().map(|o| o.id).collect();
    assert!(ids.contains(&"manifest_matches_host"));
    assert!(ids.contains(&"arc_pointer_stable"));
    assert!(ids.contains(&"capability_self_consistency_floor"));
    assert!(ids.contains(&"manifest_features_match_capabilities"));
}

const MCP_MANIFEST: &str = r#"
name = "stub-mcp"
contract = "MCPServer"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
stdio = true
sse = false
http_streamable = false
extensions = false
"#;

#[derive(Default)]
struct StubMcpServer;

#[async_trait::async_trait]
impl MCPServer for StubMcpServer {
    fn name(&self) -> &'static str {
        "stub-mcp"
    }
    fn capabilities(&self) -> &MCPServerCapabilities {
        static CAPS: MCPServerCapabilities = MCPServerCapabilities {
            stdio: true,
            sse: false,
            http_streamable: false,
            extensions: false,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
}

#[test]
fn tier1_cases_pass_for_well_formed_mcp_server() {
    let mut reg = PluginRegistry::new();
    let name = PluginName::new("stub-mcp").expect("valid");
    let manifest = PluginManifest::parse_toml(MCP_MANIFEST).expect("manifest parses");
    reg.register_mcp_server_with_manifest(name.clone(), manifest, Arc::new(StubMcpServer))
        .expect("registers");

    let outcomes = run_conformance_for_plugin(&reg, &name);

    let tier1: Vec<_> = outcomes.iter().filter(|o| o.tier == Tier::One).collect();
    assert_eq!(tier1.len(), 4, "expect 4 tier-1 cases");
    for outcome in &tier1 {
        assert!(
            matches!(outcome.status, CaseStatus::Ok),
            "tier-1 case {} must pass, got {:?}",
            outcome.id,
            outcome.status
        );
    }

    let ids: Vec<_> = tier1.iter().map(|o| o.id).collect();
    assert!(ids.contains(&"manifest_matches_host"));
    assert!(ids.contains(&"arc_pointer_stable"));
    assert!(ids.contains(&"capability_self_consistency_floor"));
    assert!(ids.contains(&"manifest_features_match_capabilities"));
}

const SENSOR_MANIFEST: &str = r#"
name = "stub-sensor"
contract = "SensorIngress"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
batches = true
streaming = false
consent_aware = true
"#;

#[derive(Default)]
struct StubSensor;

#[async_trait::async_trait]
impl SensorIngress for StubSensor {
    fn name(&self) -> &'static str {
        "stub-sensor"
    }
    fn capabilities(&self) -> &SensorIngressCapabilities {
        static CAPS: SensorIngressCapabilities = SensorIngressCapabilities {
            batches: true,
            streaming: false,
            consent_aware: true,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
}

#[test]
fn tier1_cases_pass_for_well_formed_sensor_ingress() {
    let mut reg = PluginRegistry::new();
    let name = PluginName::new("stub-sensor").expect("valid");
    let manifest = PluginManifest::parse_toml(SENSOR_MANIFEST).expect("manifest parses");
    reg.register_sensor_ingress_with_manifest(name.clone(), manifest, Arc::new(StubSensor))
        .expect("registers");

    let outcomes = run_conformance_for_plugin(&reg, &name);

    let tier1: Vec<_> = outcomes.iter().filter(|o| o.tier == Tier::One).collect();
    assert_eq!(tier1.len(), 4, "expect 4 tier-1 cases");
    for outcome in &tier1 {
        assert!(
            matches!(outcome.status, CaseStatus::Ok),
            "tier-1 case {} must pass, got {:?}",
            outcome.id,
            outcome.status
        );
    }

    let ids: Vec<_> = tier1.iter().map(|o| o.id).collect();
    assert!(ids.contains(&"manifest_matches_host"));
    assert!(ids.contains(&"arc_pointer_stable"));
    assert!(ids.contains(&"capability_self_consistency_floor"));
    assert!(ids.contains(&"manifest_features_match_capabilities"));
}

const WORKFLOW_MANIFEST: &str = r#"
name = "stub-workflow"
contract = "WorkflowOrchestrator"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
durable = true
crash_safe = true
cron_schedules = false
"#;

#[derive(Default)]
struct StubWorkflow;

#[async_trait::async_trait]
impl WorkflowOrchestrator for StubWorkflow {
    fn name(&self) -> &'static str {
        "stub-workflow"
    }
    fn capabilities(&self) -> &WorkflowOrchestratorCapabilities {
        static CAPS: WorkflowOrchestratorCapabilities = WorkflowOrchestratorCapabilities {
            durable: true,
            crash_safe: true,
            cron_schedules: false,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
}

#[test]
fn tier1_cases_pass_for_well_formed_workflow_orchestrator() {
    let mut reg = PluginRegistry::new();
    let name = PluginName::new("stub-workflow").expect("valid");
    let manifest = PluginManifest::parse_toml(WORKFLOW_MANIFEST).expect("manifest parses");
    reg.register_workflow_orchestrator_with_manifest(
        name.clone(),
        manifest,
        Arc::new(StubWorkflow),
    )
    .expect("registers");

    let outcomes = run_conformance_for_plugin(&reg, &name);

    let tier1: Vec<_> = outcomes.iter().filter(|o| o.tier == Tier::One).collect();
    assert_eq!(tier1.len(), 4, "expect 4 tier-1 cases");
    for outcome in &tier1 {
        assert!(
            matches!(outcome.status, CaseStatus::Ok),
            "tier-1 case {} must pass, got {:?}",
            outcome.id,
            outcome.status
        );
    }

    let ids: Vec<_> = tier1.iter().map(|o| o.id).collect();
    assert!(ids.contains(&"manifest_matches_host"));
    assert!(ids.contains(&"arc_pointer_stable"));
    assert!(ids.contains(&"capability_self_consistency_floor"));
    assert!(ids.contains(&"manifest_features_match_capabilities"));
}
