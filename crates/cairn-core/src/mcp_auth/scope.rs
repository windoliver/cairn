//! Scope resolution trait + default config-backed impl.

use crate::domain::ScopeTuple;
use crate::mcp_auth::McpAuthContext;

/// Errors a [`McpSessionScope`] resolver may surface.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScopeResolutionError {
    /// The resolver was unable to identify the caller (e.g. no
    /// principal extractable from a transport that requires one).
    #[error("scope resolver could not identify caller")]
    Unidentified,
    /// The deployment is configured but the active configuration
    /// does not authorise any record scope for this caller.
    #[error("scope resolver returned an empty allowed-scope set")]
    EmptyScope,
    /// Resolver-specific failure (e.g. registry lookup failed).
    #[error("scope resolver failed: {reason}")]
    Other {
        /// Human-readable description of the failure mode.
        reason: String,
    },
}

/// Resolve the requesting MCP session's allowed record scope ids.
///
/// Stdio implementations may ignore `ctx.request_id` and return a fixed
/// vector — that is honest single-tenant behaviour, not a per-caller
/// isolation claim. Future per-request-identity transports extract the
/// principal from `ctx` and key the resolution off it.
pub trait McpSessionScope: Send + Sync {
    /// Resolve allowed scopes for this request.
    ///
    /// # Errors
    /// Returns [`ScopeResolutionError`] when the caller cannot be
    /// identified or when resolution itself fails. **An empty `Vec` is
    /// fail-closed: the (future) tool layer treats it the same as
    /// `Err(_)` — `CapabilityUnavailable`.**
    fn allowed_scopes(
        &self,
        ctx: &McpAuthContext<'_>,
    ) -> Result<Vec<ScopeTuple>, ScopeResolutionError>;
}

/// Default scope resolver: returns the deployment's configured principal
/// as the single allowed scope on every call.
///
/// Honest single-tenant: every request gets the same `Vec<ScopeTuple>`
/// (length 1). The MCP handler still re-invokes the resolver per request,
/// so swapping in a per-caller resolver later is a drop-in change.
#[derive(Debug, Clone)]
pub struct ConfigBackedScope {
    principal: ScopeTuple,
}

impl ConfigBackedScope {
    /// Construct from the principal carried in
    /// `cairn.toml::[mcp.stdio] principal`.
    #[must_use]
    pub fn new(principal: ScopeTuple) -> Self {
        Self { principal }
    }

    /// Borrow the configured principal.
    #[must_use]
    pub fn principal(&self) -> &ScopeTuple {
        &self.principal
    }
}

impl McpSessionScope for ConfigBackedScope {
    fn allowed_scopes(
        &self,
        _ctx: &McpAuthContext<'_>,
    ) -> Result<Vec<ScopeTuple>, ScopeResolutionError> {
        Ok(vec![self.principal.clone()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_backed_scope_returns_single_principal_per_call() {
        let principal = ScopeTuple {
            tenant: Some("acme".into()),
            workspace: Some("eng".into()),
            ..ScopeTuple::default()
        };
        let scope = ConfigBackedScope::new(principal.clone());
        let ctx_principal = principal.clone();
        let ctx = McpAuthContext::new(&ctx_principal, "req-1");

        let resolved = scope
            .allowed_scopes(&ctx)
            .expect("ConfigBackedScope never errors");
        assert_eq!(resolved, vec![principal]);
    }

    #[test]
    fn config_backed_scope_ignores_request_id() {
        // Honest single-tenant: request_id varies, scope does not.
        let principal = ScopeTuple {
            tenant: Some("a".into()),
            ..ScopeTuple::default()
        };
        let scope = ConfigBackedScope::new(principal.clone());
        let ctx_a = McpAuthContext::new(&principal, "req-a");
        let ctx_b = McpAuthContext::new(&principal, "req-b");
        assert_eq!(
            scope.allowed_scopes(&ctx_a).unwrap(),
            scope.allowed_scopes(&ctx_b).unwrap(),
        );
    }
}
