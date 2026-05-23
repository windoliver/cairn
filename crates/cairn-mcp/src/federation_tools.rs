//! MCP tool registration for the `cairn.federation.v1` extension
//! (brief §12.a, issue #123 T17).
//!
//! The three federation verbs (`propose_share`, `accept_share`,
//! `revoke_share`) ship as IDL-generated [`crate::generated::ToolDecl`]
//! entries in [`crate::generated::TOOLS`]. Unlike the eight core verbs
//! they are gated by the `cairn.mcp.v1.extension.federation` capability
//! string — when the runtime is not wired end-to-end, the tools must
//! NOT be advertised through `tools/list`, and a `tools/call` request
//! must reject with `CapabilityUnavailable` (brief §15 fail-closed).
//!
//! This module mirrors the [`crate::coord_tools`] surface:
//!
//! * [`FEDERATION_TOOL_NAMES`] — the three verb names the federation
//!   extension owns.
//! * [`is_federation_tool`] — name-membership test for routing.
//! * [`runtime_ready`] — gate that ANDs the
//!   [`cairn_core::status::wiring::federation_extension_ready`]
//!   constant with the live status capabilities slice. Returns `true`
//!   only when the runtime advertises the federation capability AND the
//!   build-time wiring constants agree.
//! * [`dispatch`] — stub dispatcher used while
//!   [`cairn_core::status::wiring::FEDERATION_MCP_TOOLS_WIRED`] is
//!   `false`. T19 will replace the stub with real verb dispatch once
//!   every dependency (keystore, outbox, `consent_lookup`, rebac) is
//!   plumbed through the MCP runtime.
//!
//! The hand-written [`From`] conversions between codegenerated args /
//! data types and the verb-layer request / response types live in
//! [`crate::federation_conv`] — keeping them out of this file lets the
//! tool-registration surface stay focused on gating.

use cairn_core::generated::common::Capabilities;
use rmcp::model::{CallToolResult, Content};

/// IDL-generated MCP tool names that belong to the
/// `cairn.mcp.v1.extension.federation` capability.
///
/// Kept in the order they appear in [`crate::generated::TOOLS`] so any
/// snapshot or diff between the two surfaces stays stable.
pub const FEDERATION_TOOL_NAMES: &[&str] = &["propose_share", "accept_share", "revoke_share"];

/// Capability string that gates every federation MCP tool.
pub const FEDERATION_CAPABILITY: &str = "cairn.mcp.v1.extension.federation";

/// `true` when `name` is one of the federation verb tools.
#[must_use]
pub fn is_federation_tool(name: &str) -> bool {
    FEDERATION_TOOL_NAMES.contains(&name)
}

/// `true` only when status negotiation AND MCP dispatch are both ready.
///
/// Status negotiation is the runtime-advertised capability slice
/// (produced by `cairn_core::status::advertise`). The dispatch-readiness
/// half is gated by [`cairn_core::status::wiring::federation_extension_ready`]
/// — flipping that const without the matching capability advertise is a
/// bug; both halves must agree.
#[must_use]
pub fn runtime_ready(capabilities: &[Capabilities]) -> bool {
    dispatch_ready()
        && capabilities
            .iter()
            .any(|cap| matches!(cap, Capabilities::CairnMcpV1ExtensionFederation))
}

/// `true` when federation MCP calls have a real dispatcher available.
///
/// Mirrors [`crate::coord_tools::dispatch_ready`]. T19 lands the real
/// dispatcher in [`dispatch`]; this constant flips when every
/// federation wiring constant is on.
#[must_use]
pub const fn dispatch_ready() -> bool {
    cairn_core::status::wiring::federation_extension_ready()
}

/// Build a `CapabilityUnavailable` `CallToolResult` for a federation
/// tool that was called while the capability is unwired.
///
/// Mirrors `crate::handler::capability_unavailable_result` so the
/// federation surface stays consistent with the rest of the MCP handler.
#[must_use]
pub fn capability_unavailable(name: &str) -> CallToolResult {
    let mut text = format!("cairn federation: capability unavailable: {FEDERATION_CAPABILITY}");
    if let Some(hint) = cairn_core::status::remediation_for(FEDERATION_CAPABILITY) {
        text.push_str("\n  hint: ");
        text.push_str(hint);
    }
    let _ = name;
    CallToolResult::error(vec![Content::text(text)])
}

/// Dispatch a federation MCP tool.
///
/// Stub during T17: federation verbs need the keystore + outbox +
/// `consent_lookup` + rebac context that the MCP runtime does not yet
/// plumb through. T19 lands the full wiring and replaces this body
/// with the verb call. The argument shape is intentionally already in
/// the signature so the dispatch table in `handler::call_tool` does not
/// need a follow-up rewrite when the real implementation arrives.
///
/// This branch is unreachable from production callers while
/// [`dispatch_ready`] returns `false`: the `call_tool` dispatch path
/// short-circuits with [`capability_unavailable`] before reaching
/// here. The error body below is the defence-in-depth safety net for
/// any caller that bypasses the gate during testing.
#[must_use]
pub fn dispatch(
    name: &str,
    _arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> CallToolResult {
    CallToolResult::error(vec![Content::text(format!(
        "federation tool dispatch is not implemented: {name}"
    ))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn federation_tool_names_are_in_idl_tools() {
        // Every federation tool name must round-trip against the
        // IDL-generated TOOLS array so a rename in the IDL surfaces as a
        // unit-test failure rather than silent drift.
        let idl_names: Vec<&str> = crate::generated::TOOLS
            .iter()
            .filter(|d| d.capability == Some(FEDERATION_CAPABILITY))
            .map(|d| d.name)
            .collect();
        let mut local: Vec<&str> = FEDERATION_TOOL_NAMES.to_vec();
        let mut idl_sorted = idl_names.clone();
        local.sort_unstable();
        idl_sorted.sort_unstable();
        assert_eq!(local, idl_sorted);
    }

    #[test]
    fn is_federation_tool_matches_only_the_three_verbs() {
        assert!(is_federation_tool("propose_share"));
        assert!(is_federation_tool("accept_share"));
        assert!(is_federation_tool("revoke_share"));
        assert!(!is_federation_tool("ingest"));
        assert!(!is_federation_tool("handshake"));
        assert!(!is_federation_tool("cairn.coord.lease_acquire"));
    }

    #[test]
    fn dispatch_ready_tracks_wiring_constant() {
        assert_eq!(
            dispatch_ready(),
            cairn_core::status::wiring::federation_extension_ready()
        );
    }

    #[test]
    fn runtime_ready_requires_both_dispatch_and_capability() {
        // No capability advertised → never ready.
        assert!(!runtime_ready(&[]));
        // Capability advertised but dispatch unwired → not ready.
        let caps = [Capabilities::CairnMcpV1ExtensionFederation];
        assert_eq!(runtime_ready(&caps), dispatch_ready());
    }
}
