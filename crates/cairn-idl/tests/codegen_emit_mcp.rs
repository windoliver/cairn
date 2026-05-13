#![allow(missing_docs)]

use cairn_idl::codegen::{emit_mcp, ir, loader};

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

#[test]
fn emits_tools_array_and_schemas_subtree() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    assert!(
        names
            .iter()
            .any(|n| n.ends_with("crates/cairn-mcp/src/generated/mod.rs"))
    );
    for verb in [
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
    ] {
        assert!(
            names.iter().any(|n| n.ends_with(&format!(
                "crates/cairn-mcp/src/generated/schemas/verbs/{verb}.json"
            ))),
            "missing schema for {verb}"
        );
    }
    assert!(
        names
            .iter()
            .any(|n| n.ends_with("crates/cairn-mcp/src/generated/schemas/prelude/status.json"))
    );
    assert!(
        names
            .iter()
            .any(|n| n.ends_with("crates/cairn-mcp/src/generated/schemas/prelude/handshake.json"))
    );
}

#[test]
fn schemas_use_canonical_json() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let ingest = files
        .iter()
        .find(|f| f.path.ends_with("schemas/verbs/ingest.json"))
        .unwrap();
    let body = std::str::from_utf8(&ingest.bytes).unwrap();
    assert!(
        body.ends_with('\n'),
        "canonical JSON must end with a newline"
    );
    // Parsing must succeed and round-trip via canonical writer == identity.
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
    let again = cairn_idl::codegen::fmt::write_json_canonical(&parsed);
    assert_eq!(body, again, "ingest schema is not canonical");
}

#[test]
fn tool_decl_description_includes_skill_triggers() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let mod_rs = files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-mcp/src/generated/mod.rs"))
        .unwrap();
    let body = std::str::from_utf8(&mod_rs.bytes).unwrap();
    // Any description should include at least one positive trigger phrase.
    assert!(
        body.contains("remember that"),
        "ingest's positive trigger missing"
    );
}

#[test]
fn verb_tool_descriptions_use_issue_159_metadata_names() {
    for verb in [
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
    ] {
        let path = std::path::Path::new(cairn_idl::SCHEMA_DIR)
            .join("verbs")
            .join(format!("{verb}.json"));
        let raw = std::fs::read_to_string(&path).unwrap();
        let schema: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let triggers = schema
            .get("x-cairn-skill-triggers")
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| panic!("{verb}: missing x-cairn-skill-triggers"));

        assert!(
            triggers
                .get("positive_triggers")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| !items.is_empty()),
            "{verb}: missing non-empty positive_triggers"
        );
        assert!(
            triggers
                .get("negative_triggers")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| !items.is_empty()),
            "{verb}: missing non-empty negative_triggers"
        );
        assert!(
            triggers
                .get("exclusivity_hints")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|hint| !hint.is_empty()),
            "{verb}: missing non-empty exclusivity_hints"
        );
        assert!(
            !triggers.contains_key("positive")
                && !triggers.contains_key("negative")
                && !triggers.contains_key("exclusivity"),
            "{verb}: legacy trigger keys should not remain in canonical IDL"
        );
    }
}

#[test]
fn tool_decl_descriptions_are_single_paragraph_prompts() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let mod_rs = files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-mcp/src/generated/mod.rs"))
        .unwrap();
    let body = std::str::from_utf8(&mod_rs.bytes).unwrap();

    assert!(
        !body.contains("POSITIVE — use when") && !body.contains("NEGATIVE — do not use when"),
        "MCP descriptions should be rendered as coherent prompt paragraphs, not sectioned lists"
    );
    assert!(
        body.contains("Use when the user says 'remember that...'")
            && body.contains("Do not use for raw chat transcripts")
            && body.contains("Exclusivity: prefer this over other remember_* / save_* tools"),
        "rendered description should still include positive, negative, and exclusivity guidance"
    );
}

#[test]
fn supporting_schema_groups_are_emitted_alongside_verbs() {
    // Cross-file `$ref` paths inside verb schemas (e.g.
    // `../common/scope_filter.json`) need the sibling schema groups to ship
    // under the same on-disk root, otherwise the references dangle when the
    // MCP server validates incoming requests.
    let files = emit_mcp::emit(&doc()).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    for required in [
        "crates/cairn-mcp/src/generated/schemas/common/primitives.json",
        "crates/cairn-mcp/src/generated/schemas/common/scope_filter.json",
        "crates/cairn-mcp/src/generated/schemas/errors/error.json",
        "crates/cairn-mcp/src/generated/schemas/capabilities/capabilities.json",
        "crates/cairn-mcp/src/generated/schemas/extensions/registry.json",
        "crates/cairn-mcp/src/generated/schemas/envelope/request.json",
        "crates/cairn-mcp/src/generated/schemas/envelope/response.json",
        "crates/cairn-mcp/src/generated/schemas/envelope/signed_intent.json",
    ] {
        assert!(
            names.iter().any(|n| n.ends_with(required)),
            "missing supporting schema {required}"
        );
    }
}

#[test]
fn input_schema_files_are_emitted_per_verb() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    for verb in [
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
    ] {
        assert!(
            names.iter().any(|n| n.ends_with(&format!(
                "crates/cairn-mcp/src/generated/schemas/verbs/{verb}.input.json"
            ))),
            "missing input schema for {verb}"
        );
    }
}

#[test]
fn input_schema_root_delegates_to_args() {
    let files = emit_mcp::emit(&doc()).unwrap();
    for verb in [
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
    ] {
        let suffix = format!("crates/cairn-mcp/src/generated/schemas/verbs/{verb}.input.json");
        let f = files
            .iter()
            .find(|f| f.path.ends_with(&suffix))
            .unwrap_or_else(|| panic!("missing {verb}.input.json"));
        let parsed: serde_json::Value = serde_json::from_slice(&f.bytes).unwrap();
        assert_eq!(
            parsed.get("$ref").and_then(serde_json::Value::as_str),
            Some("#/$defs/Args"),
            "{verb}.input.json must root at $ref #/$defs/Args"
        );
        let defs = parsed
            .get("$defs")
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| panic!("{verb}.input.json missing $defs"));
        assert!(
            defs.contains_key("Args"),
            "{verb}.input.json $defs must include Args"
        );
    }
}

#[test]
fn input_schema_args_for_required_verbs_advertises_required() {
    // ingest, summarize: Args has top-level `required` (or oneOf gating).
    // The Args sub-schema MUST advertise the requirement so MCP clients
    // reject `{}` rather than passing it through silently.
    let files = emit_mcp::emit(&doc()).unwrap();
    for (verb, expected_required_or_one_of) in [("ingest", "kind"), ("summarize", "record_ids")] {
        let suffix = format!("crates/cairn-mcp/src/generated/schemas/verbs/{verb}.input.json");
        let f = files.iter().find(|f| f.path.ends_with(&suffix)).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&f.bytes).unwrap();
        let args = parsed
            .pointer("/$defs/Args")
            .unwrap_or_else(|| panic!("{verb}: $defs.Args missing"));
        let required = args
            .get("required")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|a| {
                a.iter()
                    .any(|v| v.as_str() == Some(expected_required_or_one_of))
            });
        assert!(
            required,
            "{verb}: Args must require {expected_required_or_one_of}"
        );
    }
}

#[test]
fn input_schema_for_oneof_verbs_keeps_dispatch() {
    // For verbs whose Args is an XOR oneOf (ingest body/file/url) or
    // tagged-union dispatch (forget mode, retrieve target), the Args schema
    // must keep the oneOf or rely on $defs/Args*-style sub-types so callers
    // cannot send `{}` and get past validation.
    let files = emit_mcp::emit(&doc()).unwrap();

    // ingest, capture_trace: Args has its own oneOf at the Args level.
    for verb in ["ingest", "capture_trace"] {
        let f = files
            .iter()
            .find(|f| f.path.ends_with(format!("schemas/verbs/{verb}.input.json")))
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&f.bytes).unwrap();
        let args = parsed.pointer("/$defs/Args").unwrap();
        assert!(
            args.get("oneOf").is_some(),
            "{verb} Args must keep its oneOf XOR dispatch"
        );
    }

    // forget / retrieve: Args itself is a oneOf over $defs/Args* subtypes.
    for verb in ["forget", "retrieve"] {
        let f = files
            .iter()
            .find(|f| f.path.ends_with(format!("schemas/verbs/{verb}.input.json")))
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&f.bytes).unwrap();
        let args = parsed.pointer("/$defs/Args").unwrap();
        assert!(
            args.get("oneOf").is_some(),
            "{verb} Args must keep its oneOf dispatch over Args* sub-types"
        );
    }
}

#[test]
fn tool_decl_input_schema_points_at_input_file() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let mod_rs = files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-mcp/src/generated/mod.rs"))
        .unwrap();
    let body = std::str::from_utf8(&mod_rs.bytes).unwrap();
    for verb in [
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
    ] {
        let needle = format!("schemas/verbs/{verb}.input.json");
        assert!(
            body.contains(&needle),
            "ToolDecl for {verb} must include_bytes! the .input.json companion"
        );
    }
}

#[test]
fn search_capability_overrides_include_explain_true() {
    // `x-cairn-capability-when-true` on `search.explain` must propagate into
    // the generated ToolDecl so the MCP transport can gate `explain: true`
    // without special-casing it in transport code.
    let files = emit_mcp::emit(&doc()).unwrap();
    let mod_rs = files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-mcp/src/generated/mod.rs"))
        .unwrap();
    let body = std::str::from_utf8(&mod_rs.bytes).unwrap();
    assert!(
        body.contains("\"explain=true\""),
        "search ToolDecl must include capability_override for explain=true; mod.rs snippet:\n{}",
        body.lines()
            .filter(|l| l.contains("explain") || l.contains("capability_override"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        body.contains("cairn.mcp.v1.policy_trace"),
        "explain=true capability must be cairn.mcp.v1.policy_trace"
    );
}

#[test]
fn verb_schema_carries_full_idl_file_with_local_defs() {
    // Per-verb schema should be the full source file so `#/$defs/...` refs
    // inside Args/Data resolve against the same JSON document.
    let files = emit_mcp::emit(&doc()).unwrap();
    let retrieve = files
        .iter()
        .find(|f| f.path.ends_with("schemas/verbs/retrieve.json"))
        .unwrap();
    let body = std::str::from_utf8(&retrieve.bytes).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
    let defs = parsed
        .get("$defs")
        .and_then(serde_json::Value::as_object)
        .expect("retrieve schema must keep its $defs envelope");
    for required_def in [
        "Args",
        "ArgsRecord",
        "ArgsSession",
        "ArgsTurn",
        "ArgsFolder",
        "ArgsScope",
        "ArgsProfile",
        "Data",
    ] {
        assert!(
            defs.contains_key(required_def),
            "retrieve schema missing $defs.{required_def} — local refs would dangle"
        );
    }
}
