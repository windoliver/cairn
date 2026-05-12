//! `[mcp]` config block (issue #190 prereq).
//!
//! `[mcp.stdio]` carries the single-tenant opt-in flag and the principal
//! that `ConfigBackedScope` returns. Defaults are fail-closed:
//! `single_tenant = false` means MCP graph tools are unavailable on this
//! deployment, no principal needed.

use serde::{Deserialize, Serialize};

use crate::domain::ScopeTuple;

/// `[mcp]` section. Currently only the stdio sub-block exists.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Per-transport stdio configuration.
    pub stdio: McpStdioConfig,
}

/// `[mcp.stdio]` section.
///
/// Defaults: `single_tenant = false`, `principal = None`. A deployment that
/// does not enable single-tenant mode never advertises graph tools and
/// never needs a principal — the configuration round-trips cleanly with no
/// `[mcp.stdio]` block at all.
///
/// Validation: if `single_tenant = true` then `principal` MUST be `Some`.
/// `validate_mcp_config` returns
/// [`ConfigError::McpStdioMissingPrincipal`](super::ConfigError::McpStdioMissingPrincipal)
/// otherwise.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpStdioConfig {
    /// Operator opt-in: this stdio process serves exactly one principal
    /// for its entire lifetime. The construction-time `principal` is the
    /// only configuration where it is a faithful authorization key
    /// (spec §2.1.1).
    pub single_tenant: bool,
    /// Principal returned by `ConfigBackedScope::allowed_scopes` on every
    /// request. Required when `single_tenant = true`; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<ScopeTuple>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_fail_closed() {
        let cfg = McpStdioConfig::default();
        assert!(!cfg.single_tenant, "default: deny");
        assert!(cfg.principal.is_none(), "default: no principal");
    }

    #[test]
    fn deserialize_omitted_block_yields_default() {
        let yaml = "";
        let cfg: McpConfig = yaml_serde::from_str(yaml).unwrap();
        assert_eq!(cfg, McpConfig::default());
    }

    #[test]
    fn deserialize_single_tenant_with_principal_round_trips() {
        let yaml = "
stdio:
  single_tenant: true
  principal:
    tenant: acme
    workspace: eng
";
        let cfg: McpConfig = yaml_serde::from_str(yaml).unwrap();
        assert!(cfg.stdio.single_tenant);
        let p = cfg.stdio.principal.as_ref().unwrap();
        assert_eq!(p.tenant.as_deref(), Some("acme"));
        assert_eq!(p.workspace.as_deref(), Some("eng"));
    }

    #[test]
    fn deserialize_rejects_unknown_keys() {
        let yaml = "
stdio:
  single_tenant: true
  unknown_field: oops
";
        let err = yaml_serde::from_str::<McpConfig>(yaml).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown_field") || msg.contains("unknown field"),
            "expected unknown-field rejection, got: {msg}",
        );
    }
}
