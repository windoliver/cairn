//! Conformance cases for `MCPServer` plugins.
//!
//! Tier-1 cases run against any registered `MCPServer` plugin and assert
//! manifest/identity/version invariants. Tier-2 cases (verb behaviour)
//! return `Pending` until per-impl PRs replace the bodies.

use crate::contract::conformance::{CaseOutcome, CaseStatus, Tier, tier1_manifest_matches_host};
use crate::contract::mcp_server::CONTRACT_VERSION;
use crate::contract::registry::{PluginName, PluginRegistry};

/// Run tier-1 + tier-2 cases for an `MCPServer` plugin.
///
/// Returns an empty vec if no `MCPServer` is registered under `name`.
#[must_use]
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let Some(plugin) = registry.mcp_server(name) else {
        return Vec::new();
    };

    vec![
        // Tier 1
        tier1_manifest_matches_host(registry, name, CONTRACT_VERSION),
        tier1_register_round_trip(registry, name, &plugin),
        tier1_capability_self_consistency_floor(&*plugin),
        // Tier 2 (stub)
        CaseOutcome {
            id: "initialize_and_list_tools",
            tier: Tier::Two,
            status: CaseStatus::Pending {
                reason: "real impl pending",
            },
        },
    ]
}

fn tier1_register_round_trip(
    registry: &PluginRegistry,
    name: &PluginName,
    plugin: &std::sync::Arc<dyn crate::contract::mcp_server::MCPServer>,
) -> CaseOutcome {
    let Some(resolved) = registry.mcp_server(name) else {
        return CaseOutcome {
            id: "register_round_trip",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: "lookup returned None for registered plugin".to_string(),
            },
        };
    };
    let status = if std::sync::Arc::ptr_eq(plugin, &resolved) {
        CaseStatus::Ok
    } else {
        CaseStatus::Failed {
            message: "two lookups returned different Arcs".to_string(),
        }
    };
    CaseOutcome {
        id: "register_round_trip",
        tier: Tier::One,
        status,
    }
}

fn tier1_capability_self_consistency_floor(
    plugin: &dyn crate::contract::mcp_server::MCPServer,
) -> CaseOutcome {
    // Floor: capabilities() must return without panic, name() non-empty,
    // supported_contract_versions() must accept the host CONTRACT_VERSION.
    let caps = plugin.capabilities();
    if plugin.name().is_empty() {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: "plugin.name() returned empty string".to_string(),
            },
        };
    }
    if !plugin
        .supported_contract_versions()
        .accepts(CONTRACT_VERSION)
    {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!("plugin does not accept host CONTRACT_VERSION {CONTRACT_VERSION}"),
            },
        };
    }
    // Touch all four bool fields so a future panicking-getter regression
    // would surface here.
    let _ = (caps.stdio, caps.sse, caps.http_streamable, caps.extensions);
    CaseOutcome {
        id: "capability_self_consistency_floor",
        tier: Tier::One,
        status: CaseStatus::Ok,
    }
}
