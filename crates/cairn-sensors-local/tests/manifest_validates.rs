#![allow(missing_docs)]
//! Integration: the bundled plugin.toml parses through the manifest
//! parser, matches the host contract version + name, and registration
//! populates both per-contract and global manifest maps.

use cairn_core::contract::manifest::{ContractKind, PluginManifest};
use cairn_core::contract::registry::PluginName;
use cairn_core::contract::sensor_ingress::CONTRACT_VERSION;

#[test]
fn manifest_parses_and_matches_host() {
    let manifest =
        PluginManifest::parse_toml(cairn_sensors_local::MANIFEST_TOML).expect("manifest parses");
    assert_eq!(manifest.name().as_str(), "cairn-sensors-local");
    assert_eq!(manifest.contract(), ContractKind::SensorIngress);
    let expected = PluginName::new("cairn-sensors-local").expect("valid");
    manifest
        .verify_compatible_with(&expected, ContractKind::SensorIngress, CONTRACT_VERSION)
        .expect("manifest matches host");
}

#[test]
fn register_populates_registry() {
    let mut reg = cairn_core::contract::registry::PluginRegistry::new();
    cairn_sensors_local::register(&mut reg).expect("registers");
    let name = PluginName::new("cairn-sensors-local").expect("valid");
    assert!(reg.sensor_ingress_plugin(&name).is_some());
    assert!(reg.parsed_manifest(&name).is_some());
}
