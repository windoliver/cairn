//! T20g — `ConnectorManifest` snapshot test.
//!
//! Parses the canonical fixture manifest and takes an insta YAML snapshot.
//! Run `cargo insta test --accept -p cairn-connectors-core --test manifest_validates`
//! once to materialise the `.snap` file; subsequent runs must match exactly.
//!
//! Issue #130, brief §9.1 source sensors.

#![allow(missing_docs)]

use cairn_connectors_core::manifest::ConnectorManifest;

const MANIFEST: &str = include_str!("fixtures/manifest.toml");

#[test]
fn snapshot_parsed_manifest() {
    let m = ConnectorManifest::parse_toml(MANIFEST).expect("manifest.toml must parse");
    insta::assert_yaml_snapshot!(m);
}
