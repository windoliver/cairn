// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_mcp::generated::TOOLS;

#[test]
fn tool_count_is_eight() {
    assert_eq!(TOOLS.len(), 8, "eight verbs must be registered");
}

#[test]
fn tool_names_are_the_eight_verbs_in_order() {
    let names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
    assert_eq!(
        names,
        &[
            "ingest",
            "search",
            "retrieve",
            "summarize",
            "assemble_hot",
            "capture_trace",
            "lint",
            "forget",
        ],
        "tool names must match brief §8 verb list in order"
    );
}

#[test]
fn tool_input_schemas_parse_as_json_objects() {
    for tool in TOOLS {
        let v: serde_json::Value = serde_json::from_slice(tool.input_schema)
            .unwrap_or_else(|e| panic!("schema for '{}' is invalid JSON: {e}", tool.name));
        assert!(
            v.is_object(),
            "input schema for '{}' must be a JSON object",
            tool.name
        );
    }
}

/// Snapshot the name + auth + capability metadata for wire-compat tracking (§8.0.a).
#[test]
fn tool_auth_metadata_snapshot() {
    let metadata: Vec<serde_json::Value> = TOOLS
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "auth": t.auth,
                "capability": t.capability,
                "auth_overrides_count": t.auth_overrides.len(),
                "capability_overrides_count": t.capability_overrides.len(),
            })
        })
        .collect();
    insta::assert_json_snapshot!("tool_auth_metadata", metadata);
}
