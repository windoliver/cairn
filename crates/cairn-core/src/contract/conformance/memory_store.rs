//! Conformance cases for `MemoryStore` plugins.
//!
//! Tier-1 cases run against any registered `MemoryStore` plugin and assert
//! manifest/identity/version invariants. Tier-2 cases (verb behaviour)
//! return `Pending` until per-impl PRs replace the bodies.

use crate::contract::conformance::{CaseOutcome, CaseStatus, Tier, tier1_manifest_matches_host};
use crate::contract::memory_store::CONTRACT_VERSION;
use crate::contract::registry::{PluginName, PluginRegistry};

/// Run tier-1 + tier-2 cases for a `MemoryStore` plugin.
///
/// Returns an empty vec if no `MemoryStore` is registered under `name`.
#[must_use]
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let Some(plugin) = registry.memory_store(name) else {
        return Vec::new();
    };

    vec![
        // Tier 1
        tier1_manifest_matches_host(registry, name, CONTRACT_VERSION),
        tier1_arc_pointer_stable(registry, name, &plugin),
        tier1_capability_self_consistency_floor(&*plugin),
        // Tier 2 (stubs)
        CaseOutcome {
            id: "put_get_roundtrip",
            tier: Tier::Two,
            status: CaseStatus::Pending {
                reason: "real impl pending",
            },
        },
        CaseOutcome {
            id: "fts_query_returns_doc",
            tier: Tier::Two,
            status: CaseStatus::Pending {
                reason: "real impl pending",
            },
        },
        CaseOutcome {
            id: "vector_search_when_advertised",
            tier: Tier::Two,
            status: CaseStatus::Pending {
                reason: "real impl pending",
            },
        },
    ]
}

fn tier1_arc_pointer_stable(
    registry: &PluginRegistry,
    name: &PluginName,
    plugin: &std::sync::Arc<dyn crate::contract::memory_store::MemoryStore>,
) -> CaseOutcome {
    let Some(resolved) = registry.memory_store(name) else {
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
    plugin: &dyn crate::contract::memory_store::MemoryStore,
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
    let _ = (caps.fts, caps.vector, caps.graph_edges, caps.transactions);
    CaseOutcome {
        id: "capability_self_consistency_floor",
        tier: Tier::One,
        status: CaseStatus::Ok,
    }
}
