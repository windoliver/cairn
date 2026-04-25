//! Parity check: the patterns in `plugin/manifest.json` must match the
//! validation rules enforced by `cairn_core::contract::manifest::PluginManifest`.
//!
//! We don't run a full JSON Schema validator here (that would add a heavy dep);
//! instead we inspect the schema document and confirm the regex patterns used
//! for `name`, `features.propertyNames`, and the version object's bounds match
//! what the Rust parser enforces. This catches schema/Rust drift in CI.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use serde_json::Value;

const SCHEMA_SRC: &str = include_str!("../schema/plugin/manifest.json");

#[test]
fn name_pattern_matches_plugin_name_rules() {
    let schema: Value = serde_json::from_str(SCHEMA_SRC).expect("schema is valid JSON");
    let pattern = schema
        .pointer("/properties/name/pattern")
        .and_then(Value::as_str)
        .expect("name pattern present");
    // PluginName accepts: 3..=64 chars, ascii lowercase + digit + hyphen, no
    // leading/trailing hyphen. The schema's regex must align.
    assert_eq!(pattern, "^[a-z0-9](?:[a-z0-9-]{1,62}[a-z0-9])$");
}

#[test]
fn feature_key_pattern_matches_rust_validator() {
    let schema: Value = serde_json::from_str(SCHEMA_SRC).expect("schema is valid JSON");
    let pattern = schema
        .pointer("/properties/features/propertyNames/pattern")
        .and_then(Value::as_str)
        .expect("feature key pattern present");
    // Rust validator: ^[A-Za-z0-9_]{1,64}$. Schema must agree.
    assert_eq!(pattern, "^[A-Za-z0-9_]{1,64}$");
}

#[test]
fn version_bounds_are_u16_compatible() {
    let schema: Value = serde_json::from_str(SCHEMA_SRC).expect("schema is valid JSON");
    let major = schema
        .pointer("/$defs/version/properties/major")
        .expect("version.major present");
    assert_eq!(major.pointer("/minimum").and_then(Value::as_u64), Some(0));
    assert_eq!(
        major.pointer("/maximum").and_then(Value::as_u64),
        Some(65535)
    );
}

#[test]
fn contract_enum_lists_all_seven_kinds() {
    let schema: Value = serde_json::from_str(SCHEMA_SRC).expect("schema is valid JSON");
    let enum_arr = schema
        .pointer("/properties/contract/enum")
        .and_then(Value::as_array)
        .expect("contract enum present");
    let mut names: Vec<&str> = enum_arr.iter().filter_map(Value::as_str).collect();
    names.sort_unstable();
    let mut expected = vec![
        "AgentProvider",
        "FrontendAdapter",
        "LLMProvider",
        "MCPServer",
        "MemoryStore",
        "SensorIngress",
        "WorkflowOrchestrator",
    ];
    expected.sort_unstable();
    assert_eq!(names, expected);
}
