//! Conformance cases for `SensorIngress` plugins.
//!
//! Tier-1 cases run against any registered `SensorIngress` plugin and
//! assert manifest/identity/version invariants. Tier-2 cases (verb
//! behaviour) return `Pending` until per-impl PRs replace the bodies.

use crate::contract::conformance::{CaseOutcome, CaseStatus, Tier, tier1_manifest_matches_host};
use crate::contract::registry::{PluginName, PluginRegistry};
use crate::contract::sensor_ingress::CONTRACT_VERSION;

/// Run tier-1 + tier-2 cases for a `SensorIngress` plugin.
///
/// Returns an empty vec if no `SensorIngress` is registered under `name`.
#[must_use]
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let Some(plugin) = registry.sensor_ingress_plugin(name) else {
        return Vec::new();
    };

    vec![
        // Tier 1
        tier1_manifest_matches_host(registry, name, CONTRACT_VERSION),
        tier1_arc_pointer_stable(registry, name, &plugin),
        tier1_capability_self_consistency_floor(&*plugin),
        // Tier 2 (stub)
        CaseOutcome {
            id: "emits_envelope_when_poked",
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
    plugin: &std::sync::Arc<dyn crate::contract::sensor_ingress::SensorIngress>,
) -> CaseOutcome {
    let Some(resolved) = registry.sensor_ingress_plugin(name) else {
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
    plugin: &dyn crate::contract::sensor_ingress::SensorIngress,
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
    // Touch all three bool fields so a future panicking-getter regression
    // would surface here.
    let _ = (caps.batches, caps.streaming, caps.consent_aware);
    CaseOutcome {
        id: "capability_self_consistency_floor",
        tier: Tier::One,
        status: CaseStatus::Ok,
    }
}
