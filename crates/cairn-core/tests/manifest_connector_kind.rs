//! Integration test: `ContractKind::Connector` parses from a TOML manifest
//! and `as_static_str` returns the correct discriminator. Issue #130, brief §9.1, §19.

use cairn_core::contract::manifest::{ContractKind, PluginManifest};

const FIXTURE: &str = r#"
name = "fixture"
contract = "Connector"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0
"#;

#[test]
fn connector_kind_parses_from_manifest() {
    let manifest = PluginManifest::parse_toml(FIXTURE).expect("parse");
    assert_eq!(manifest.contract(), ContractKind::Connector);
}

#[test]
fn connector_kind_has_static_str() {
    assert_eq!(ContractKind::Connector.as_static_str(), "Connector");
}
