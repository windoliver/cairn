#![allow(missing_docs)]
//! Integration: the bundled plugin.toml parses through the manifest
//! parser, matches the host contract version + name, and registration
//! populates both per-contract and global manifest maps.

use cairn_core::contract::manifest::{ContractKind, PluginManifest};
use cairn_core::contract::registry::PluginName;
use cairn_core::contract::workflow_orchestrator::CONTRACT_VERSION;

#[test]
fn manifest_parses_and_matches_host() {
    let manifest =
        PluginManifest::parse_toml(cairn_workflows::MANIFEST_TOML).expect("manifest parses");
    assert_eq!(manifest.name().as_str(), "cairn-workflows");
    assert_eq!(manifest.contract(), ContractKind::WorkflowOrchestrator);
    let expected = PluginName::new("cairn-workflows").expect("valid");
    manifest
        .verify_compatible_with(
            &expected,
            ContractKind::WorkflowOrchestrator,
            CONTRACT_VERSION,
        )
        .expect("manifest matches host");
}

#[test]
fn register_populates_registry() {
    let mut reg = cairn_core::contract::registry::PluginRegistry::new();
    cairn_workflows::register(&mut reg).expect("registers");
    let name = PluginName::new("cairn-workflows").expect("valid");
    assert!(reg.workflow_orchestrator(&name).is_some());
    assert!(reg.parsed_manifest(&name).is_some());
}
