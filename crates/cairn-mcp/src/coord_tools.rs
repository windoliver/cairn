//! MCP tool declarations for the `cairn.coord.v1` extension.
//!
//! These tools are advertised only when `cairn.coord.v1` is wired end to end.
//! Actor identity is intentionally absent from public schemas: future dispatch
//! must derive the acting principal from authenticated MCP session state.

use cairn_core::generated::common::Capabilities;
use rmcp::model::{CallToolResult, Content};
use serde_json::Map;

/// Capability string that gates every coordination MCP tool.
pub const COORD_CAPABILITY: &str = "cairn.mcp.v1.extension.coord";

const LEASE_ACQUIRE_SCHEMA: &[u8] = br#"{"type":"object","properties":{"action_id":{"type":"string","pattern":"^[0-7][0-9A-HJKMNP-TV-Z]{25}$"},"ttl":{"type":"string","pattern":"^P.+$"},"steal_after":{"type":"string","pattern":"^P.+$"}},"required":["action_id","ttl"],"additionalProperties":false}"#;
const LEASE_RELEASE_SCHEMA: &[u8] = br#"{"type":"object","properties":{"action_id":{"type":"string","pattern":"^[0-7][0-9A-HJKMNP-TV-Z]{25}$"},"reason":{"type":"string"}},"required":["action_id"],"additionalProperties":false}"#;
const LEASE_LIST_SCHEMA: &[u8] =
    br#"{"type":"object","properties":{},"additionalProperties":false}"#;
const SIGNAL_SEND_SCHEMA: &[u8] = br#"{"type":"object","properties":{"to_actor":{"type":"string","pattern":"^(agt|hmn|snr):[A-Za-z0-9._:-]+$"},"kind":{"type":"string","enum":["task_completed","lease_released","request_review","user_input_needed","error","info"]},"payload_id":{"type":"string","pattern":"^[0-7][0-9A-HJKMNP-TV-Z]{25}$"}},"required":["to_actor","kind"],"additionalProperties":false}"#;
const SIGNAL_RECV_SCHEMA: &[u8] = br#"{"type":"object","properties":{"cursor":{"type":"string","minLength":1,"description":"Opaque signal resume token returned by the previous receive."},"kind":{"type":"string","enum":["task_completed","lease_released","request_review","user_input_needed","error","info"]}},"additionalProperties":false}"#;
const ACTION_CREATE_SCHEMA: &[u8] = br#"{"type":"object","properties":{"title":{"type":"string","minLength":1},"depends_on":{"type":"array","items":{"type":"string","pattern":"^[0-7][0-9A-HJKMNP-TV-Z]{25}$"}},"priority":{"type":"integer"}},"required":["title"],"additionalProperties":false}"#;
const ACTION_UPDATE_SCHEMA: &[u8] = br#"{"type":"object","properties":{"action_id":{"type":"string","pattern":"^[0-7][0-9A-HJKMNP-TV-Z]{25}$"},"status":{"type":"string","enum":["pending","in_progress","completed","blocked","cancelled"]},"reason":{"type":"string"}},"required":["action_id","status"],"additionalProperties":false}"#;
const ACTION_GRAPH_SCHEMA: &[u8] =
    br#"{"type":"object","properties":{"root":{"type":"string","pattern":"^[0-7][0-9A-HJKMNP-TV-Z]{25}$"}},"additionalProperties":false}"#;
const ROUTINE_INSTANTIATE_SCHEMA: &[u8] = br#"{"type":"object","properties":{"routine_name":{"type":"string","minLength":1},"vars":{"type":"object","additionalProperties":{"type":"string"}}},"required":["routine_name"],"additionalProperties":false}"#;
const FRONTIER_SCHEMA: &[u8] = br#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1}},"additionalProperties":false}"#;
const NEXT_SCHEMA: &[u8] = br#"{"type":"object","properties":{},"additionalProperties":false}"#;

/// One coordination MCP tool declaration.
#[derive(Debug, Clone, Copy)]
pub struct CoordToolDecl {
    /// MCP tool name.
    pub name: &'static str,
    /// Human-readable description for `tools/list`.
    pub description: &'static str,
    /// JSON-schema bytes for the tool arguments.
    pub input_schema: &'static [u8],
}

/// Static coordination tool registry.
pub const COORD_TOOLS: &[CoordToolDecl] = &[
    CoordToolDecl {
        name: "cairn.coord.lease_acquire",
        description: "Acquire an exclusive coordination lease for an action.",
        input_schema: LEASE_ACQUIRE_SCHEMA,
    },
    CoordToolDecl {
        name: "cairn.coord.lease_release",
        description: "Release an action coordination lease.",
        input_schema: LEASE_RELEASE_SCHEMA,
    },
    CoordToolDecl {
        name: "cairn.coord.lease_list",
        description: "List active coordination leases.",
        input_schema: LEASE_LIST_SCHEMA,
    },
    CoordToolDecl {
        name: "cairn.coord.signal_send",
        description: "Append an inter-agent coordination signal.",
        input_schema: SIGNAL_SEND_SCHEMA,
    },
    CoordToolDecl {
        name: "cairn.coord.signal_recv",
        description: "Read inter-agent coordination signals.",
        input_schema: SIGNAL_RECV_SCHEMA,
    },
    CoordToolDecl {
        name: "cairn.coord.action_create",
        description: "Create a coordination action node.",
        input_schema: ACTION_CREATE_SCHEMA,
    },
    CoordToolDecl {
        name: "cairn.coord.action_update",
        description: "Update a coordination action status.",
        input_schema: ACTION_UPDATE_SCHEMA,
    },
    CoordToolDecl {
        name: "cairn.coord.action_graph",
        description: "Read the coordination action dependency graph.",
        input_schema: ACTION_GRAPH_SCHEMA,
    },
    CoordToolDecl {
        name: "cairn.coord.routine_instantiate",
        description: "Instantiate a declarative coordination routine.",
        input_schema: ROUTINE_INSTANTIATE_SCHEMA,
    },
    CoordToolDecl {
        name: "cairn.coord.frontier",
        description: "List recommended unblocked coordination actions.",
        input_schema: FRONTIER_SCHEMA,
    },
    CoordToolDecl {
        name: "cairn.coord.next",
        description: "Return the highest-priority unblocked coordination action.",
        input_schema: NEXT_SCHEMA,
    },
];

/// True when `name` belongs to the coordination extension namespace.
#[must_use]
pub fn is_coord_tool(name: &str) -> bool {
    COORD_TOOLS.iter().any(|tool| tool.name == name)
}

/// Coordination tools enabled by the advertised status capabilities.
#[must_use]
pub fn enabled_tools(capabilities: &[Capabilities]) -> Vec<CoordToolDecl> {
    if runtime_ready(capabilities) {
        COORD_TOOLS.to_vec()
    } else {
        Vec::new()
    }
}

/// True only when status negotiation and MCP dispatch are both ready.
#[must_use]
pub fn runtime_ready(capabilities: &[Capabilities]) -> bool {
    dispatch_ready()
        && capabilities
            .iter()
            .any(|cap| matches!(cap, Capabilities::CairnMcpV1ExtensionCoord))
}

/// True when coord MCP calls have a real dispatcher available.
#[must_use]
pub fn dispatch_ready() -> bool {
    cairn_core::status::wiring::COORD_MCP_DISPATCH_WIRED
}

/// Dispatch a coord MCP tool.
///
/// This function is intentionally unreachable while [`dispatch_ready`] is
/// false. The branch exists so tool advertisement and tool calls can share a
/// single dispatch-readiness gate.
#[must_use]
pub fn dispatch(name: &str, _arguments: Option<Map<String, serde_json::Value>>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(format!(
        "coord tool dispatch is not implemented: {name}"
    ))])
}
