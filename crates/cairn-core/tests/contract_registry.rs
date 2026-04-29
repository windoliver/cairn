//! Integration: a fake plugin registers via `register_plugin!`, the host
//! looks it up, and version mismatch fails closed.

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::contract::agent_provider::{
    AgentProvider, AgentProviderCapabilities, AgentProviderPlugin,
};
use cairn_core::contract::frontend_adapter::{
    FrontendAdapter, FrontendAdapterCapabilities, FrontendAdapterPlugin,
};
use cairn_core::contract::llm_provider::{LLMProvider, LLMProviderCapabilities, LLMProviderPlugin};
use cairn_core::contract::mcp_server::{MCPServer, MCPServerCapabilities, MCPServerPlugin};
use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, Edge, EdgeDir, EdgeKey, KeywordSearchArgs, KeywordSearchPage, ListArgs,
    ListPage, MemoryStore, MemoryStoreCapabilities, MemoryStorePlugin, RecordVersion, StoreError,
    TombstoneReason, UpsertOutcome,
};
use cairn_core::contract::registry::{PluginError, PluginName, PluginRegistry};
use cairn_core::contract::sensor_ingress::{
    SensorIngress, SensorIngressCapabilities, SensorIngressPlugin,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::contract::workflow_orchestrator::{
    WorkflowOrchestrator, WorkflowOrchestratorCapabilities, WorkflowOrchestratorPlugin,
};
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use cairn_core::register_plugin;
use cairn_core::register_plugin_with;

mod compatible_plugin {
    use super::*;

    #[derive(Default)]
    pub struct FakeStore;

    #[async_trait::async_trait]
    impl MemoryStore for FakeStore {
        fn name(&self) -> &'static str {
            "fake-compat"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: true,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 3, 0))
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            Err("stub: upsert not implemented".into())
        }
        async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
            Ok(None)
        }
        async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
            Ok(ListPage {
                records: vec![],
                next_cursor: None,
            })
        }
        async fn tombstone(&self, _id: &RecordId, _r: TombstoneReason) -> Result<(), StoreError> {
            Ok(())
        }
        async fn versions(&self, _t: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
            Ok(vec![])
        }
        async fn put_edge(&self, _e: &Edge) -> Result<(), StoreError> {
            Ok(())
        }
        async fn remove_edge(&self, _k: &EdgeKey) -> Result<bool, StoreError> {
            Ok(false)
        }
        async fn neighbours(&self, _id: &RecordId, _d: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            Ok(vec![])
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            Err("stub: search_keyword not implemented".into())
        }
    }

    impl MemoryStorePlugin for FakeStore {
        const NAME: &'static str = "fake-compat";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 3, 0));
    }

    register_plugin!(MemoryStore, FakeStore, "fake-compat");
}

mod future_plugin {
    use super::*;

    #[derive(Default)]
    pub struct FutureStore;

    #[async_trait::async_trait]
    impl MemoryStore for FutureStore {
        fn name(&self) -> &'static str {
            "fake-future"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            // Demands a future host version — must be rejected today.
            VersionRange::new(
                ContractVersion::new(9, 9, 0),
                ContractVersion::new(10, 0, 0),
            )
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            Err("stub: upsert not implemented".into())
        }
        async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
            Ok(None)
        }
        async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
            Ok(ListPage {
                records: vec![],
                next_cursor: None,
            })
        }
        async fn tombstone(&self, _id: &RecordId, _r: TombstoneReason) -> Result<(), StoreError> {
            Ok(())
        }
        async fn versions(&self, _t: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
            Ok(vec![])
        }
        async fn put_edge(&self, _e: &Edge) -> Result<(), StoreError> {
            Ok(())
        }
        async fn remove_edge(&self, _k: &EdgeKey) -> Result<bool, StoreError> {
            Ok(false)
        }
        async fn neighbours(&self, _id: &RecordId, _d: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            Ok(vec![])
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            Err("stub: search_keyword not implemented".into())
        }
    }

    impl MemoryStorePlugin for FutureStore {
        const NAME: &'static str = "fake-future";
        const SUPPORTED_VERSIONS: VersionRange = VersionRange::new(
            ContractVersion::new(9, 9, 0),
            ContractVersion::new(10, 0, 0),
        );
    }

    register_plugin!(MemoryStore, FutureStore, "fake-future");
}

#[test]
fn compatible_plugin_registers_via_macro() {
    let mut reg = PluginRegistry::new();
    compatible_plugin::register(&mut reg).expect("compatible plugin registers");

    let name = PluginName::new("fake-compat").expect("valid name");
    let resolved = reg.memory_store(&name).expect("registered");
    assert_eq!(resolved.name(), "fake-compat");
    assert!(
        resolved
            .supported_contract_versions()
            .accepts(CONTRACT_VERSION)
    );
}

#[test]
fn incompatible_plugin_fails_closed() {
    let mut reg = PluginRegistry::new();
    let err =
        future_plugin::register(&mut reg).expect_err("plugin demanding future host must fail");
    match err {
        PluginError::UnsupportedContractVersion { contract, host, .. } => {
            assert_eq!(contract, "MemoryStore");
            assert_eq!(host, CONTRACT_VERSION);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn keeps_arc_pointer_stable() {
    // Two lookups return the same underlying Arc.
    let mut reg = PluginRegistry::new();
    compatible_plugin::register(&mut reg).expect("registers");
    let name = PluginName::new("fake-compat").expect("valid");
    let a = reg.memory_store(&name).expect("registered");
    let b = reg.memory_store(&name).expect("registered");
    assert!(Arc::ptr_eq(&a, &b));
}

mod manifest_aware_plugin {
    use super::*;

    pub const MANIFEST_TOML: &str = r#"
name = "fake-with-manifest"
contract = "MemoryStore"

[contract_version_range.min]
major = 0
minor = 2
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 3
patch = 0
"#;

    #[derive(Default)]
    pub struct FakeStore;

    #[async_trait::async_trait]
    impl MemoryStore for FakeStore {
        fn name(&self) -> &'static str {
            "fake-with-manifest"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 3, 0))
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            Err("stub: upsert not implemented".into())
        }
        async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
            Ok(None)
        }
        async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
            Ok(ListPage {
                records: vec![],
                next_cursor: None,
            })
        }
        async fn tombstone(&self, _id: &RecordId, _r: TombstoneReason) -> Result<(), StoreError> {
            Ok(())
        }
        async fn versions(&self, _t: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
            Ok(vec![])
        }
        async fn put_edge(&self, _e: &Edge) -> Result<(), StoreError> {
            Ok(())
        }
        async fn remove_edge(&self, _k: &EdgeKey) -> Result<bool, StoreError> {
            Ok(false)
        }
        async fn neighbours(&self, _id: &RecordId, _d: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            Ok(vec![])
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            Err("stub: search_keyword not implemented".into())
        }
    }

    impl MemoryStorePlugin for FakeStore {
        const NAME: &'static str = "fake-with-manifest";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 3, 0));
    }

    register_plugin!(MemoryStore, FakeStore, "fake-with-manifest", MANIFEST_TOML);
}

#[test]
fn manifest_aware_macro_registers_with_manifest() {
    let mut reg = PluginRegistry::new();
    manifest_aware_plugin::register(&mut reg).expect("manifest-aware register succeeds");

    let name = PluginName::new("fake-with-manifest").expect("valid");
    assert!(reg.memory_store(&name).is_some(), "trait registered");
    assert!(reg.parsed_manifest(&name).is_some(), "manifest registered");
    assert_eq!(
        reg.parsed_manifest(&name).unwrap().contract(),
        cairn_core::contract::manifest::ContractKind::MemoryStore
    );
}

mod incompatible_factory_plugin {
    use super::*;

    pub struct NeverBuilt;

    #[async_trait::async_trait]
    impl MemoryStore for NeverBuilt {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            Err("stub: upsert not implemented".into())
        }
        async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
            Ok(None)
        }
        async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
            Ok(ListPage {
                records: vec![],
                next_cursor: None,
            })
        }
        async fn tombstone(&self, _id: &RecordId, _r: TombstoneReason) -> Result<(), StoreError> {
            Ok(())
        }
        async fn versions(&self, _t: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
            Ok(vec![])
        }
        async fn put_edge(&self, _e: &Edge) -> Result<(), StoreError> {
            Ok(())
        }
        async fn remove_edge(&self, _k: &EdgeKey) -> Result<bool, StoreError> {
            Ok(false)
        }
        async fn neighbours(&self, _id: &RecordId, _d: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            Ok(vec![])
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            Err("stub: search_keyword not implemented".into())
        }
    }

    impl MemoryStorePlugin for NeverBuilt {
        const NAME: &'static str = "never-built";
        const SUPPORTED_VERSIONS: VersionRange = VersionRange::new(
            ContractVersion::new(9, 9, 0),
            ContractVersion::new(10, 0, 0),
        );
    }

    register_plugin_with!(
        MemoryStore,
        NeverBuilt,
        "never-built",
        |_cfg: &cairn_core::config::CairnConfig| -> Result<NeverBuilt, std::convert::Infallible> {
            panic!("factory must NOT be called for incompatible plugin versions")
        }
    );
}

mod config_driven_plugin {
    use super::*;

    pub struct PathStore;

    #[async_trait::async_trait]
    impl MemoryStore for PathStore {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            Err("stub: upsert not implemented".into())
        }
        async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
            Ok(None)
        }
        async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
            Ok(ListPage {
                records: vec![],
                next_cursor: None,
            })
        }
        async fn tombstone(&self, _id: &RecordId, _r: TombstoneReason) -> Result<(), StoreError> {
            Ok(())
        }
        async fn versions(&self, _t: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
            Ok(vec![])
        }
        async fn put_edge(&self, _e: &Edge) -> Result<(), StoreError> {
            Ok(())
        }
        async fn remove_edge(&self, _k: &EdgeKey) -> Result<bool, StoreError> {
            Ok(false)
        }
        async fn neighbours(&self, _id: &RecordId, _d: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            Ok(vec![])
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            Err("stub: search_keyword not implemented".into())
        }
    }

    impl MemoryStorePlugin for PathStore {
        const NAME: &'static str = "path-store";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 3, 0));
    }

    register_plugin_with!(
        MemoryStore,
        PathStore,
        "path-store",
        |_cfg: &cairn_core::config::CairnConfig| { Ok::<_, std::convert::Infallible>(PathStore) }
    );
}

mod name_mismatch_plugin {
    use super::*;

    #[derive(Default)]
    pub struct BadNameStore;

    #[async_trait::async_trait]
    impl MemoryStore for BadNameStore {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            Err("stub: upsert not implemented".into())
        }
        async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
            Ok(None)
        }
        async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
            Ok(ListPage {
                records: vec![],
                next_cursor: None,
            })
        }
        async fn tombstone(&self, _id: &RecordId, _r: TombstoneReason) -> Result<(), StoreError> {
            Ok(())
        }
        async fn versions(&self, _t: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
            Ok(vec![])
        }
        async fn put_edge(&self, _e: &Edge) -> Result<(), StoreError> {
            Ok(())
        }
        async fn remove_edge(&self, _k: &EdgeKey) -> Result<bool, StoreError> {
            Ok(false)
        }
        async fn neighbours(&self, _id: &RecordId, _d: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            Ok(vec![])
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            Err("stub: search_keyword not implemented".into())
        }
    }

    impl MemoryStorePlugin for BadNameStore {
        const NAME: &'static str = "actual-name";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 3, 0));
    }

    // NAME const = "actual-name" but macro literal = "wrong-name"
    register_plugin!(MemoryStore, BadNameStore, "wrong-name");
}

#[test]
fn factory_not_called_on_version_mismatch() {
    let mut reg = PluginRegistry::new();
    let cfg = CairnConfig::default();
    let err = incompatible_factory_plugin::register(&mut reg, &cfg)
        .expect_err("incompatible plugin must fail closed");
    match err {
        PluginError::UnsupportedContractVersion { contract, host, .. } => {
            assert_eq!(contract, "MemoryStore");
            assert_eq!(host, CONTRACT_VERSION);
        }
        other => panic!("expected UnsupportedContractVersion, got {other:?}"),
    }
    // If we reach here the panicking factory was never called — test passes.
}

#[test]
fn config_driven_plugin_registers_and_resolves() {
    let mut reg = PluginRegistry::new();
    let cfg = CairnConfig::default();
    config_driven_plugin::register(&mut reg, &cfg).expect("config-driven plugin registers");
    let name = PluginName::new("path-store").expect("valid");
    let plugin = reg.memory_store(&name).expect("registered");
    assert_eq!(plugin.name(), "path-store");
}

#[test]
fn register_plugin_macro_rejects_name_const_mismatch() {
    // Verifies the new static identity pre-check in register_plugin! fires
    // before Default::default() is called.
    let mut reg = PluginRegistry::new();
    let err =
        name_mismatch_plugin::register(&mut reg).expect_err("NAME/literal mismatch must fail");
    assert!(
        matches!(err, PluginError::IdentityMismatch { .. }),
        "expected IdentityMismatch, got {err:?}"
    );
}

// ── register_plugin_with! coverage for the remaining 6 contract arms ─────────

mod llm_provider_factory_plugin {
    use super::*;

    pub struct StubLlm;

    #[async_trait::async_trait]
    impl LLMProvider for StubLlm {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &LLMProviderCapabilities {
            static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
                json_mode: false,
                streaming: false,
                tool_calls: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
    }

    impl LLMProviderPlugin for StubLlm {
        const NAME: &'static str = "stub-llm-factory";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    register_plugin_with!(
        LLMProvider,
        StubLlm,
        "stub-llm-factory",
        |_cfg: &cairn_core::config::CairnConfig| { Ok::<_, std::convert::Infallible>(StubLlm) }
    );
}

mod workflow_orchestrator_factory_plugin {
    use super::*;

    pub struct StubOrch;

    #[async_trait::async_trait]
    impl WorkflowOrchestrator for StubOrch {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &WorkflowOrchestratorCapabilities {
            static CAPS: WorkflowOrchestratorCapabilities = WorkflowOrchestratorCapabilities {
                durable: false,
                crash_safe: false,
                cron_schedules: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
    }

    impl WorkflowOrchestratorPlugin for StubOrch {
        const NAME: &'static str = "stub-orch-factory";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    register_plugin_with!(
        WorkflowOrchestrator,
        StubOrch,
        "stub-orch-factory",
        |_cfg: &cairn_core::config::CairnConfig| { Ok::<_, std::convert::Infallible>(StubOrch) }
    );
}

mod sensor_ingress_factory_plugin {
    use super::*;

    pub struct StubSensor;

    #[async_trait::async_trait]
    impl SensorIngress for StubSensor {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &SensorIngressCapabilities {
            static CAPS: SensorIngressCapabilities = SensorIngressCapabilities {
                batches: false,
                streaming: false,
                consent_aware: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
    }

    impl SensorIngressPlugin for StubSensor {
        const NAME: &'static str = "stub-sensor-factory";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    register_plugin_with!(
        SensorIngress,
        StubSensor,
        "stub-sensor-factory",
        |_cfg: &cairn_core::config::CairnConfig| { Ok::<_, std::convert::Infallible>(StubSensor) }
    );
}

mod mcp_server_factory_plugin {
    use super::*;

    pub struct StubMcp;

    #[async_trait::async_trait]
    impl MCPServer for StubMcp {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &MCPServerCapabilities {
            static CAPS: MCPServerCapabilities = MCPServerCapabilities {
                stdio: false,
                sse: false,
                http_streamable: false,
                extensions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
    }

    impl MCPServerPlugin for StubMcp {
        const NAME: &'static str = "stub-mcp-factory";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    register_plugin_with!(
        MCPServer,
        StubMcp,
        "stub-mcp-factory",
        |_cfg: &cairn_core::config::CairnConfig| { Ok::<_, std::convert::Infallible>(StubMcp) }
    );
}

mod frontend_adapter_factory_plugin {
    use super::*;

    pub struct StubFrontend;

    #[async_trait::async_trait]
    impl FrontendAdapter for StubFrontend {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &FrontendAdapterCapabilities {
            static CAPS: FrontendAdapterCapabilities = FrontendAdapterCapabilities {
                markdown_projection: false,
                live_events: false,
                reverse_edits: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
    }

    impl FrontendAdapterPlugin for StubFrontend {
        const NAME: &'static str = "stub-frontend-factory";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 0, 1), ContractVersion::new(0, 1, 0));
    }

    register_plugin_with!(
        FrontendAdapter,
        StubFrontend,
        "stub-frontend-factory",
        |_cfg: &cairn_core::config::CairnConfig| {
            Ok::<_, std::convert::Infallible>(StubFrontend)
        }
    );
}

mod agent_provider_factory_plugin {
    use super::*;

    pub struct StubAgent;

    #[async_trait::async_trait]
    impl AgentProvider for StubAgent {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &AgentProviderCapabilities {
            static CAPS: AgentProviderCapabilities = AgentProviderCapabilities {
                honors_cost_budget: false,
                scope_enforced: false,
                mcp_tools: false,
                cli_subprocess_tools: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
    }

    impl AgentProviderPlugin for StubAgent {
        const NAME: &'static str = "stub-agent-factory";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 0, 1), ContractVersion::new(0, 1, 0));
    }

    register_plugin_with!(
        AgentProvider,
        StubAgent,
        "stub-agent-factory",
        |_cfg: &cairn_core::config::CairnConfig| { Ok::<_, std::convert::Infallible>(StubAgent) }
    );
}

// ── factory-error path ────────────────────────────────────────────────────────

mod factory_error_plugin {
    use super::*;

    pub struct FailingStore;

    #[async_trait::async_trait]
    impl MemoryStore for FailingStore {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            Err("stub: upsert not implemented".into())
        }
        async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
            Ok(None)
        }
        async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
            Ok(ListPage {
                records: vec![],
                next_cursor: None,
            })
        }
        async fn tombstone(&self, _id: &RecordId, _r: TombstoneReason) -> Result<(), StoreError> {
            Ok(())
        }
        async fn versions(&self, _t: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
            Ok(vec![])
        }
        async fn put_edge(&self, _e: &Edge) -> Result<(), StoreError> {
            Ok(())
        }
        async fn remove_edge(&self, _k: &EdgeKey) -> Result<bool, StoreError> {
            Ok(false)
        }
        async fn neighbours(&self, _id: &RecordId, _d: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            Ok(vec![])
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            Err("stub: search_keyword not implemented".into())
        }
    }

    impl MemoryStorePlugin for FailingStore {
        const NAME: &'static str = "failing-store";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 3, 0));
    }

    register_plugin_with!(
        MemoryStore,
        FailingStore,
        "failing-store",
        |_cfg: &cairn_core::config::CairnConfig| {
            Err::<FailingStore, _>(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "db file missing",
            ))
        }
    );
}

// ── new tests ─────────────────────────────────────────────────────────────────

#[test]
fn register_plugin_with_llm_provider() {
    let mut reg = PluginRegistry::new();
    let cfg = CairnConfig::default();
    llm_provider_factory_plugin::register(&mut reg, &cfg).expect("LLMProvider factory registers");
    let name = PluginName::new("stub-llm-factory").expect("valid");
    assert!(reg.llm_provider(&name).is_some());
}

#[test]
fn register_plugin_with_workflow_orchestrator() {
    let mut reg = PluginRegistry::new();
    let cfg = CairnConfig::default();
    workflow_orchestrator_factory_plugin::register(&mut reg, &cfg)
        .expect("WorkflowOrchestrator factory registers");
    let name = PluginName::new("stub-orch-factory").expect("valid");
    assert!(reg.workflow_orchestrator(&name).is_some());
}

#[test]
fn register_plugin_with_sensor_ingress() {
    let mut reg = PluginRegistry::new();
    let cfg = CairnConfig::default();
    sensor_ingress_factory_plugin::register(&mut reg, &cfg)
        .expect("SensorIngress factory registers");
    let name = PluginName::new("stub-sensor-factory").expect("valid");
    assert!(reg.sensor_ingress_plugin(&name).is_some());
}

#[test]
fn register_plugin_with_mcp_server() {
    let mut reg = PluginRegistry::new();
    let cfg = CairnConfig::default();
    mcp_server_factory_plugin::register(&mut reg, &cfg).expect("MCPServer factory registers");
    let name = PluginName::new("stub-mcp-factory").expect("valid");
    assert!(reg.mcp_server(&name).is_some());
}

#[test]
fn register_plugin_with_frontend_adapter() {
    let mut reg = PluginRegistry::new();
    let cfg = CairnConfig::default();
    frontend_adapter_factory_plugin::register(&mut reg, &cfg)
        .expect("FrontendAdapter factory registers");
    let name = PluginName::new("stub-frontend-factory").expect("valid");
    assert!(reg.frontend_adapter(&name).is_some());
}

#[test]
fn register_plugin_with_agent_provider() {
    let mut reg = PluginRegistry::new();
    let cfg = CairnConfig::default();
    agent_provider_factory_plugin::register(&mut reg, &cfg)
        .expect("AgentProvider factory registers");
    let name = PluginName::new("stub-agent-factory").expect("valid");
    assert!(reg.agent_provider(&name).is_some());
}

#[test]
fn factory_error_propagates_as_plugin_error() {
    let mut reg = PluginRegistry::new();
    let cfg = CairnConfig::default();
    let err = factory_error_plugin::register(&mut reg, &cfg)
        .expect_err("failing factory must produce an error");
    match err {
        PluginError::FactoryError {
            contract,
            ref plugin,
            ref source,
        } => {
            assert_eq!(contract, "MemoryStore");
            assert_eq!(plugin.as_str(), "failing-store");
            assert!(
                source.to_string().contains("db file missing"),
                "source: {source}"
            );
        }
        other => panic!("expected FactoryError, got {other:?}"),
    }
}
