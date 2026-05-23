//! MCP tool registration and dispatch for the `cairn.federation.v1`
//! extension (brief §12.a, issue #123).
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
//! * [`dispatch`] — real verb dispatcher that routes through the
//!   verb layer when `FederationState` is available. Returns
//!   `CapabilityUnavailable` when no federation state is configured.
//!
//! The hand-written [`From`] conversions between codegenerated args /
//! data types and the verb-layer request / response types live in
//! [`crate::federation_conv`] — keeping them out of this file lets the
//! tool-registration surface stay focused on gating.

use cairn_core::domain::time::SystemClock;
use cairn_core::error::federation::FederationError;
use cairn_core::generated::common::Capabilities;
use cairn_core::generated::envelope::{ResponseData, ResponseVerb};
use cairn_core::rebac::RebacContext;
use rmcp::model::{CallToolResult, Content};

use crate::handler::FederationState;

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
/// Mirrors [`crate::coord_tools::dispatch_ready`]. All five federation
/// wiring constants are `true`, so this returns `true`.
#[must_use]
pub const fn dispatch_ready() -> bool {
    cairn_core::status::wiring::federation_extension_ready()
}

/// Build a `CapabilityUnavailable` `CallToolResult` for a federation
/// tool that was called while the capability is unwired or no
/// `FederationState` is configured.
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

/// Dispatch a federation MCP tool through the verb layer.
///
/// When `state` is `Some`, routes to the matching verb function and
/// converts the result to a `CallToolResult`. When `None`, returns
/// `CapabilityUnavailable` — the deployment has no federation config.
pub async fn dispatch(
    name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
    state: Option<&FederationState>,
) -> CallToolResult {
    let Some(state) = state else {
        return capability_unavailable(name);
    };

    let args_value = arguments
        .map(serde_json::Value::Object)
        .unwrap_or(serde_json::Value::Null);

    match name {
        "propose_share" => dispatch_propose_share(args_value, state).await,
        "accept_share" => dispatch_accept_share(args_value, state).await,
        "revoke_share" => dispatch_revoke_share(args_value, state).await,
        _ => CallToolResult::error(vec![Content::text(format!(
            "cairn federation: unknown tool: {name}"
        ))]),
    }
}

async fn dispatch_propose_share(
    args_value: serde_json::Value,
    state: &FederationState,
) -> CallToolResult {
    let args: cairn_core::generated::verbs::propose_share::ProposeShareArgs =
        match serde_json::from_value(args_value) {
            Ok(a) => a,
            Err(e) => {
                return federation_error_result(
                    ResponseVerb::ProposeShare,
                    &FederationError::InvalidShape,
                    &format!("invalid propose_share arguments: {e}"),
                );
            }
        };

    let request = match crate::federation_conv::propose_share_request_from_args(args) {
        Ok(r) => r,
        Err(e) => return federation_error_result(ResponseVerb::ProposeShare, &e, ""),
    };

    let clock = SystemClock;
    let rebac = RebacContext::default();
    let deps = cairn_core::verbs::propose_share::ProposeShareDeps {
        store: state.store.as_ref(),
        outbox: state.store.as_ref(),
        signing_key: &state.signing_key,
        signer_identity: &state.identity,
        signer_key_version: state.key_version,
        rebac: &rebac,
        clock: &clock,
        federation_ready: true,
    };

    match cairn_core::verbs::propose_share::propose_share(request, &deps).await {
        Ok(response) => {
            match crate::federation_conv::propose_share_data_from_response(response) {
                Ok(data) => {
                    let response = crate::verb_envelope::committed(
                        ResponseVerb::ProposeShare,
                        ResponseData::ProposeShare(data),
                        Vec::new(),
                    );
                    crate::verb_envelope::call_result_from_response(&response)
                }
                Err(e) => federation_error_result(ResponseVerb::ProposeShare, &e, ""),
            }
        }
        Err(e) => federation_error_result(
            ResponseVerb::ProposeShare,
            &e,
            &format!("propose_share: {e}"),
        ),
    }
}

async fn dispatch_accept_share(
    args_value: serde_json::Value,
    state: &FederationState,
) -> CallToolResult {
    let args: cairn_core::generated::verbs::accept_share::AcceptShareArgs =
        match serde_json::from_value(args_value) {
            Ok(a) => a,
            Err(e) => {
                return federation_error_result(
                    ResponseVerb::AcceptShare,
                    &FederationError::InvalidShape,
                    &format!("invalid accept_share arguments: {e}"),
                );
            }
        };

    let request = crate::federation_conv::accept_share_request_from_args(args);

    let clock = SystemClock;
    let rebac = RebacContext::default();
    let verifying_key = state.signing_key.verifying_key();
    let inbound_sensor = state.identity.clone();
    let deps = cairn_core::verbs::accept_share::AcceptShareDeps {
        store: state.store.as_ref(),
        outbox: state.store.as_ref(),
        consent_lookup: state.store.as_ref(),
        local_signing_key: &state.signing_key,
        receiver_identity: &state.identity,
        issuer_verifying_key: &verifying_key,
        rebac: &rebac,
        clock: &clock,
        inbound_sensor: &inbound_sensor,
        federation_ready: true,
    };

    match cairn_core::verbs::accept_share::accept_share(request, &deps).await {
        Ok(response) => {
            let data = crate::federation_conv::accept_share_data_from_response(response);
            let response = crate::verb_envelope::committed(
                ResponseVerb::AcceptShare,
                ResponseData::AcceptShare(data),
                Vec::new(),
            );
            crate::verb_envelope::call_result_from_response(&response)
        }
        Err(e) => federation_error_result(
            ResponseVerb::AcceptShare,
            &e,
            &format!("accept_share: {e}"),
        ),
    }
}

async fn dispatch_revoke_share(
    args_value: serde_json::Value,
    state: &FederationState,
) -> CallToolResult {
    let args: cairn_core::generated::verbs::revoke_share::RevokeShareArgs =
        match serde_json::from_value(args_value) {
            Ok(a) => a,
            Err(e) => {
                return federation_error_result(
                    ResponseVerb::RevokeShare,
                    &FederationError::InvalidShape,
                    &format!("invalid revoke_share arguments: {e}"),
                );
            }
        };

    let request = crate::federation_conv::revoke_share_request_from_args(args);

    let clock = SystemClock;
    let deps = cairn_core::verbs::revoke_share::RevokeShareDeps {
        outbox: state.store.as_ref(),
        consent_lookup: state.store.as_ref(),
        signing_key: &state.signing_key,
        signer_identity: &state.identity,
        signer_key_version: state.key_version,
        clock: &clock,
        federation_ready: true,
    };

    match cairn_core::verbs::revoke_share::revoke_share(request, &deps).await {
        Ok(response) => {
            let data = crate::federation_conv::revoke_share_data_from_response(response);
            let response = crate::verb_envelope::committed(
                ResponseVerb::RevokeShare,
                ResponseData::RevokeShare(data),
                Vec::new(),
            );
            crate::verb_envelope::call_result_from_response(&response)
        }
        Err(e) => federation_error_result(
            ResponseVerb::RevokeShare,
            &e,
            &format!("revoke_share: {e}"),
        ),
    }
}

/// Convert a [`FederationError`] into a `CallToolResult` with the
/// matching envelope policy. `CapabilityDisabled` maps to the
/// capability-unavailable result; everything else is an aborted
/// internal error.
fn federation_error_result(
    verb: ResponseVerb,
    err: &FederationError,
    detail: &str,
) -> CallToolResult {
    if matches!(err, FederationError::CapabilityDisabled) {
        return capability_unavailable("");
    }
    let msg = if detail.is_empty() {
        format!("{err}")
    } else {
        detail.to_owned()
    };
    let response = crate::verb_envelope::aborted_internal(verb, &msg);
    crate::verb_envelope::call_result_from_response(&response)
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
    fn dispatch_ready_is_true() {
        assert!(dispatch_ready());
    }

    #[test]
    fn runtime_ready_requires_both_dispatch_and_capability() {
        // No capability advertised → never ready.
        assert!(!runtime_ready(&[]));
        // Capability advertised AND dispatch wired → ready.
        let caps = [Capabilities::CairnMcpV1ExtensionFederation];
        assert_eq!(runtime_ready(&caps), dispatch_ready());
    }

    #[tokio::test]
    async fn dispatch_returns_capability_unavailable_when_no_state() {
        let result = dispatch("propose_share", None, None).await;
        assert!(result.is_error.unwrap_or(false));
        let text = result
            .content
            .first()
            .and_then(|c| {
                if let rmcp::model::RawContent::Text(t) = &**c {
                    Some(t.text.as_str())
                } else {
                    None
                }
            })
            .unwrap_or("");
        assert!(
            text.contains("capability unavailable"),
            "expected capability unavailable, got: {text}"
        );
    }
}
