//! Integration: the bundled plugin.toml parses through the manifest
//! parser and matches the host contract version + name.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_core::contract::manifest::{ContractKind, PluginManifest};
use cairn_core::contract::mcp_server::CONTRACT_VERSION;
use cairn_core::contract::registry::PluginName;

#[test]
fn manifest_parses_and_matches_host() {
    let manifest = PluginManifest::parse_toml(cairn_mcp::MANIFEST_TOML).expect("manifest parses");
    assert_eq!(manifest.name().as_str(), "cairn-mcp");
    assert_eq!(manifest.contract(), ContractKind::MCPServer);
    let expected = PluginName::new("cairn-mcp").expect("valid");
    manifest
        .verify_compatible_with(&expected, ContractKind::MCPServer, CONTRACT_VERSION)
        .expect("manifest matches host");
}

#[test]
fn register_populates_registry() {
    let mut reg = cairn_core::contract::registry::PluginRegistry::new();
    cairn_mcp::register(&mut reg).expect("registers");
    let name = PluginName::new("cairn-mcp").expect("valid");
    assert!(reg.mcp_server(&name).is_some());
    assert!(reg.parsed_manifest(&name).is_some());
}

#[test]
fn stdio_capability_advertised() {
    use cairn_core::contract::registry::PluginRegistry;

    let mut reg = PluginRegistry::new();
    cairn_mcp::register(&mut reg).expect("registers");
    let name = PluginName::new("cairn-mcp").expect("valid");
    let plugin = reg.mcp_server(&name).expect("registered");
    assert!(
        plugin.capabilities().stdio,
        "CairnMcpServer must advertise stdio=true"
    );
}
