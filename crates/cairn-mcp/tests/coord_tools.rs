//! Coordination MCP tool registration gates.

use cairn_core::generated::common::Capabilities;

#[test]
fn coord_tools_follow_coord_runtime_readiness() {
    let none = cairn_mcp::coord_tools::enabled_tools(&[]);
    assert!(
        none.is_empty(),
        "coord tools must stay hidden without the coord extension capability"
    );

    let coord_capabilities = [Capabilities::CairnMcpV1ExtensionCoord];
    let enabled = cairn_mcp::coord_tools::enabled_tools(&coord_capabilities);
    if !cairn_core::status::wiring::coord_extension_ready() {
        assert!(
            enabled.is_empty(),
            "coord tools must stay hidden until the coord runtime is wired"
        );
        return;
    }

    let names: Vec<_> = enabled.iter().map(|tool| tool.name).collect();
    assert_eq!(
        names,
        [
            "cairn.coord.lease_acquire",
            "cairn.coord.lease_release",
            "cairn.coord.lease_list",
            "cairn.coord.signal_send",
            "cairn.coord.signal_recv",
            "cairn.coord.action_create",
            "cairn.coord.action_update",
            "cairn.coord.action_graph",
            "cairn.coord.routine_instantiate",
            "cairn.coord.frontier",
            "cairn.coord.next",
        ]
    );
}

#[test]
fn coord_dispatch_stub_cannot_be_marked_ready() {
    assert!(
        !cairn_mcp::coord_tools::dispatch_ready(),
        "remove the coord_tools::dispatch stub and add real non-error dispatch coverage before enabling coord readiness"
    );
}

#[test]
fn coord_tool_schemas_describe_required_arguments_and_closed_enums() {
    for tool in cairn_mcp::coord_tools::COORD_TOOLS {
        let schema = schema(tool);
        assert_eq!(schema["type"], "object", "schema for {}", tool.name);
        assert_eq!(
            schema["additionalProperties"], false,
            "schema for {} must reject unknown arguments",
            tool.name
        );
        assert!(schema["properties"].is_object(), "schema for {}", tool.name);
    }

    let lease_acquire = schema(tool("cairn.coord.lease_acquire"));
    assert_eq!(
        lease_acquire["required"],
        serde_json::json!(["action_id", "ttl"])
    );
    assert_eq!(lease_acquire["properties"]["ttl"]["type"], "string");
    assert_eq!(lease_acquire["properties"]["steal_after"]["type"], "string");
    assert_eq!(
        lease_acquire["properties"]["action_id"]["pattern"],
        "^[0-7][0-9A-HJKMNP-TV-Z]{25}$"
    );
    assert_eq!(lease_acquire["properties"]["ttl"]["pattern"], "^P.+$");
    assert_eq!(
        lease_acquire["properties"]["steal_after"]["pattern"],
        "^P.+$"
    );
    assert!(
        lease_acquire["properties"]["actor"].is_null(),
        "acting identity must come from authenticated session state"
    );

    let lease_release = schema(tool("cairn.coord.lease_release"));
    assert!(lease_release["properties"]["actor"].is_null());

    let signal_send = schema(tool("cairn.coord.signal_send"));
    assert_eq!(
        signal_send["properties"]["to_actor"]["pattern"],
        "^(agt|hmn|snr):[A-Za-z0-9._:-]+$"
    );
    assert_eq!(
        signal_send["properties"]["payload_id"]["pattern"],
        "^[0-7][0-9A-HJKMNP-TV-Z]{25}$"
    );
    assert_eq!(
        signal_send["properties"]["kind"]["enum"],
        serde_json::json!([
            "task_completed",
            "lease_released",
            "request_review",
            "user_input_needed",
            "error",
            "info"
        ])
    );

    let signal_recv = schema(tool("cairn.coord.signal_recv"));
    assert_eq!(signal_recv["properties"]["cursor"]["type"], "string");
    assert_eq!(signal_recv["properties"]["cursor"]["minLength"], 1);
    assert!(signal_recv["properties"]["actor"].is_null());

    let frontier = schema(tool("cairn.coord.frontier"));
    assert!(frontier["properties"]["actor"].is_null());

    let action_update = schema(tool("cairn.coord.action_update"));
    assert_eq!(
        action_update["properties"]["status"]["enum"],
        serde_json::json!([
            "pending",
            "in_progress",
            "completed",
            "blocked",
            "cancelled"
        ])
    );
}

fn tool(name: &str) -> &'static cairn_mcp::coord_tools::CoordToolDecl {
    cairn_mcp::coord_tools::COORD_TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("missing coord tool {name}"))
}

fn schema(tool: &cairn_mcp::coord_tools::CoordToolDecl) -> serde_json::Value {
    serde_json::from_slice(tool.input_schema)
        .unwrap_or_else(|err| panic!("schema for {} must be JSON: {err}", tool.name))
}
