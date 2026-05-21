//! Conformance cases for `MCPServer` plugins.
//!
//! Tier-1 cases run against any registered `MCPServer` plugin and assert
//! manifest/identity/version invariants. Tier-2 cases (verb behaviour)
//! return `Pending` until per-impl PRs replace the bodies.

use crate::contract::conformance::{
    CaseOutcome, CaseStatus, Tier, tier1_manifest_features_match_capabilities,
    tier1_manifest_matches_host,
};
use crate::contract::mcp_server::CONTRACT_VERSION;
use crate::contract::registry::{PluginName, PluginRegistry};

/// Run tier-1 + tier-2 cases for an `MCPServer` plugin.
///
/// Returns an empty vec if no `MCPServer` is registered under `name`.
#[must_use]
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let Some(plugin) = registry.mcp_server(name) else {
        return vec![CaseOutcome {
            id: "typed_plugin_registered",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!(
                    "manifest declared MCPServer but no MCPServer Arc \
                     registered under name {name}"
                ),
            },
        }];
    };
    let caps = plugin.capabilities();

    vec![
        // Tier 1
        tier1_manifest_matches_host(registry, name, CONTRACT_VERSION),
        tier1_arc_pointer_stable(registry, name, &plugin),
        tier1_capability_self_consistency_floor(&*plugin),
        tier1_manifest_features_match_capabilities(
            registry,
            name,
            &[
                ("stdio", caps.stdio),
                ("sse", caps.sse),
                ("http_streamable", caps.http_streamable),
                ("extensions", caps.extensions),
            ],
        ),
        // Tier 2
        tier2_tool_availability(registry, name),
    ]
}

fn tier1_arc_pointer_stable(
    registry: &PluginRegistry,
    name: &PluginName,
    plugin: &std::sync::Arc<dyn crate::contract::mcp_server::MCPServer>,
) -> CaseOutcome {
    let Some(resolved) = registry.mcp_server(name) else {
        return CaseOutcome {
            id: "arc_pointer_stable",
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
        id: "arc_pointer_stable",
        tier: Tier::One,
        status,
    }
}

fn tier1_capability_self_consistency_floor(
    plugin: &dyn crate::contract::mcp_server::MCPServer,
) -> CaseOutcome {
    // Floor: capabilities() must return without panic, name() non-empty,
    // supported_contract_versions() must accept the host CONTRACT_VERSION.
    // The panic-vs-typed-error half of the spec §4.3 floor (verb methods
    // returning `CapabilityUnavailable` for un-advertised capabilities) is
    // deferred to per-impl PRs (#46 et al) where verb methods land.
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

fn tier2_tool_availability(registry: &PluginRegistry, name: &PluginName) -> CaseOutcome {
    // cairn-core cannot depend on cairn-mcp (wrong dependency direction),
    // so it cannot initialize the stdio service or call tools/list here.
    // Keep this pending until an adapter-level executable conformance path
    // can exercise the real transport.
    let Some(plugin) = registry.mcp_server(name) else {
        return CaseOutcome {
            id: "initialize_and_list_tools",
            tier: Tier::Two,
            status: CaseStatus::Failed {
                message: "plugin not registered".to_string(),
            },
        };
    };
    let status = if plugin.capabilities().stdio {
        CaseStatus::Pending {
            reason: "stdio advertised, but core cannot exercise initialize/tools-list without the MCP adapter",
        }
    } else {
        CaseStatus::Pending {
            reason: "stdio transport not yet advertised; tool list unavailable without a server",
        }
    };
    CaseOutcome {
        id: "initialize_and_list_tools",
        tier: Tier::Two,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::manifest::PluginManifest;
    use crate::contract::mcp_server::{MCPServer, MCPServerCapabilities};
    use crate::contract::version::{ContractVersion, VersionRange};

    struct StdioMcp;

    #[async_trait::async_trait]
    impl MCPServer for StdioMcp {
        fn name(&self) -> &'static str {
            "stdio-mcp"
        }

        fn capabilities(&self) -> &MCPServerCapabilities {
            static CAPS: MCPServerCapabilities = MCPServerCapabilities {
                stdio: true,
                sse: false,
                http_streamable: false,
                extensions: false,
            };
            &CAPS
        }

        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }
    }

    #[test]
    fn tool_availability_stays_pending_when_core_cannot_exercise_stdio() {
        let mut registry = PluginRegistry::new();
        let name = PluginName::new("stdio-mcp").expect("valid plugin name");
        let manifest = PluginManifest::parse_toml(
            r#"
name = "stdio-mcp"
contract = "MCPServer"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
stdio = true
sse = false
http_streamable = false
extensions = false
"#,
        )
        .expect("manifest parses");
        registry
            .register_mcp_server_with_manifest(
                name.clone(),
                manifest,
                std::sync::Arc::new(StdioMcp),
            )
            .expect("registers");

        let outcome = tier2_tool_availability(&registry, &name);

        assert_eq!(outcome.id, "initialize_and_list_tools");
        assert!(matches!(outcome.status, CaseStatus::Pending { .. }));
    }
}
