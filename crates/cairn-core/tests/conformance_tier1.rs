//! Tier-1 conformance: ensure the three core cases pass against a
//! well-formed stub plugin registered with a matching manifest.

use std::sync::Arc;

use cairn_core::contract::conformance::{CaseStatus, Tier, run_conformance_for_plugin};
use cairn_core::contract::manifest::PluginManifest;
use cairn_core::contract::memory_store::{MemoryStore, MemoryStoreCapabilities};
use cairn_core::contract::registry::{PluginName, PluginRegistry};
use cairn_core::contract::version::{ContractVersion, VersionRange};

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
    assert_eq!(tier1.len(), 3, "expect 3 tier-1 cases");
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
    assert!(ids.contains(&"register_round_trip"));
    assert!(ids.contains(&"capability_self_consistency_floor"));
}
