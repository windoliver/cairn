//! MCP authorization substrate: per-request auth context, scope-resolution
//! trait, transport tag, and the shared `mcp_graph_tools_available` enum.
//!
//! Pure types only — no I/O. Living in `cairn-core` keeps the dep boundary
//! intact: every other workspace crate may depend on these types.

pub mod scope;

pub use scope::{ConfigBackedScope, McpSessionScope, ScopeResolutionError};

use crate::domain::ScopeTuple;

/// Transport the MCP server is running. Drives the
/// `mcp_graph_tools_available` predicate's transport-precondition row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpTransport {
    /// Process-pair stdio. The only transport this PR ships.
    Stdio,
}

/// Per-request authorization context handed to a [`McpSessionScope`].
///
/// On stdio (this PR) `principal` is fixed at server-construction time and
/// `request_id` varies per call. Future SSE / HTTP transports will extend
/// the struct (still `#[non_exhaustive]`) with per-request principal
/// extraction from `RequestContext::extensions`.
#[derive(Debug)]
#[non_exhaustive]
pub struct McpAuthContext<'a> {
    /// The principal asserted for this request. On stdio, the
    /// server-construction-time scope tuple from `cairn.toml::[mcp.stdio]
    /// principal`.
    pub principal: &'a ScopeTuple,
    /// Opaque MCP request id, for diagnostics. Empty when called from
    /// `tools/list` pre-flight discovery.
    pub request_id: &'a str,
}

impl<'a> McpAuthContext<'a> {
    /// Construct a context. `request_id` may be empty for pre-flight
    /// discovery (e.g. `tools/list` invoked before any
    /// `tools/call`).
    #[must_use]
    pub fn new(principal: &'a ScopeTuple, request_id: &'a str) -> Self {
        Self {
            principal,
            request_id,
        }
    }
}

/// Result of asking "should this deployment list/call MCP graph tools?"
///
/// `cairn status` reports the discriminant; the (future) MCP handler reuses
/// the same enum to gate `tools/list` and `tools/call`. Both surfaces share
/// one code path so they cannot drift.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpGraphAvailability {
    /// Graph tools available; `tool_count` is the number of `graph.*`
    /// tools the manifest would expose. Plan A ships zero graph tools,
    /// so this variant is **not produced** by the current
    /// `mcp_graph_tools_available` body — Plan C will flip the body to
    /// return it once it lands the actual tools.
    Available {
        /// Number of `graph.*` tools listed when this state holds.
        tool_count: u8,
    },
    /// Stdio is not in `single_tenant = true` mode.
    UnavailableSingleTenantOff,
    /// Store does not advertise `graph_edges` capability.
    UnavailableNoStoreCapability,
    /// No `McpSessionScope` resolver is wired into the handler.
    UnavailableNoScopeResolver,
}

impl McpGraphAvailability {
    /// Stable kebab-case label for status output / snapshot tests.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Available { .. } => "available",
            Self::UnavailableSingleTenantOff => "unavailable:single-tenant-off",
            Self::UnavailableNoStoreCapability => "unavailable:no-store-capability",
            Self::UnavailableNoScopeResolver => "unavailable:no-scope-resolver",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ScopeTuple;

    #[test]
    fn auth_context_carries_principal_and_request_id() {
        let principal = ScopeTuple {
            tenant: Some("acme".into()),
            ..ScopeTuple::default()
        };
        let ctx = McpAuthContext::new(&principal, "req-42");
        assert_eq!(ctx.principal.tenant.as_deref(), Some("acme"));
        assert_eq!(ctx.request_id, "req-42");
    }

    #[test]
    fn graph_availability_labels_are_stable() {
        assert_eq!(
            McpGraphAvailability::Available { tool_count: 5 }.label(),
            "available",
        );
        assert_eq!(
            McpGraphAvailability::UnavailableSingleTenantOff.label(),
            "unavailable:single-tenant-off",
        );
        assert_eq!(
            McpGraphAvailability::UnavailableNoStoreCapability.label(),
            "unavailable:no-store-capability",
        );
        assert_eq!(
            McpGraphAvailability::UnavailableNoScopeResolver.label(),
            "unavailable:no-scope-resolver",
        );
    }
}
