//! Conformance cases for `FrontendAdapter` plugins.
//!
//! Tier-1 cases run against any registered `FrontendAdapter` plugin and assert
//! manifest/identity/version invariants. Tier-2 cases exercise the pure
//! reconcile contract with synthetic inputs and typed error matches.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use crate::contract::conformance::{
    CaseOutcome, CaseStatus, Tier, tier1_manifest_features_match_capabilities,
    tier1_manifest_matches_host,
};
use crate::contract::frontend_adapter::{
    FrontendAdapter, FrontendAdapterError, FrontendEdit, FrontendIdentityContext,
    FrontendReconcileError,
};
use crate::contract::frontend_adapter::CONTRACT_VERSION;
use crate::contract::registry::{PluginName, PluginRegistry};
use crate::domain::{BodyHash, Identity, TargetId};

/// Run tier-1 + tier-2 cases for a `FrontendAdapter` plugin.
#[must_use]
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let Some(plugin) = registry.frontend_adapter(name) else {
        return vec![CaseOutcome {
            id: "typed_plugin_registered",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!(
                    "manifest declared FrontendAdapter but no FrontendAdapter Arc \
                     registered under name {name}"
                ),
            },
        }];
    };
    let caps = plugin.capabilities();

    vec![
        tier1_manifest_matches_host(registry, name, CONTRACT_VERSION),
        tier1_arc_pointer_stable(registry, name, &plugin),
        tier1_capability_self_consistency_floor(&*plugin),
        tier1_manifest_features_match_capabilities(
            registry,
            name,
            &[
                ("frontmatter", caps.frontmatter),
                ("sidecar_files", caps.sidecar_files),
                ("live_plugin", caps.live_plugin),
                ("graph_view", caps.graph_view),
            ],
        ),
        tier2_rejects_immutable_field_edits(&*plugin),
        tier2_rejects_replayed_operation(&*plugin),
        tier2_rejects_tampered_target_hash(&*plugin),
        tier2_rejects_unrecognized_principal(&*plugin),
        tier2_honors_optimistic_version_check(&*plugin),
    ]
}

fn tier1_arc_pointer_stable(
    registry: &PluginRegistry,
    name: &PluginName,
    plugin: &Arc<dyn FrontendAdapter>,
) -> CaseOutcome {
    let Some(resolved) = registry.frontend_adapter(name) else {
        return CaseOutcome {
            id: "arc_pointer_stable",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: "lookup returned None for registered plugin".to_string(),
            },
        };
    };
    let status = if Arc::ptr_eq(plugin, &resolved) {
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

fn tier1_capability_self_consistency_floor(plugin: &dyn FrontendAdapter) -> CaseOutcome {
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
    let _ = (
        caps.frontmatter,
        caps.sidecar_files,
        caps.live_plugin,
        caps.graph_view,
        caps.max_frontmatter_fields,
    );
    CaseOutcome {
        id: "capability_self_consistency_floor",
        tier: Tier::One,
        status: CaseStatus::Ok,
    }
}

fn tier2_rejects_immutable_field_edits(plugin: &dyn FrontendAdapter) -> CaseOutcome {
    expect_reconcile_error(
        plugin,
        "rejects_immutable_field_edits",
        sample_identity_context("hmn:known-user"),
        sample_edit(
            1,
            sample_hash("trusted body"),
            BTreeMap::from([(String::from("operation_id"), json!("mutated"))]),
        ),
        |error| matches!(error, FrontendReconcileError::ImmutableFieldChanged { field } if field == "operation_id"),
    )
}

fn tier2_rejects_replayed_operation(plugin: &dyn FrontendAdapter) -> CaseOutcome {
    expect_reconcile_error(
        plugin,
        "rejects_replayed_operation",
        sample_identity_context("hmn:known-user"),
        sample_edit(
            100,
            sample_hash("trusted body"),
            BTreeMap::from([(String::from("body"), json!("replay://operation"))]),
        ),
        |error| matches!(error, FrontendReconcileError::ReplayDetected),
    )
}

fn tier2_rejects_tampered_target_hash(plugin: &dyn FrontendAdapter) -> CaseOutcome {
    expect_reconcile_error(
        plugin,
        "rejects_tampered_target_hash",
        sample_identity_context("hmn:known-user"),
        sample_edit(
            100,
            sample_hash("tampered body"),
            BTreeMap::from([(String::from("body"), json!("updated"))]),
        ),
        |error| {
            matches!(
                error,
                FrontendReconcileError::PolicyDenied { gate, .. } if gate == "target_hash"
            )
        },
    )
}

fn tier2_rejects_unrecognized_principal(plugin: &dyn FrontendAdapter) -> CaseOutcome {
    expect_reconcile_error(
        plugin,
        "rejects_unrecognized_principal",
        sample_identity_context("hmn:unknown-user"),
        sample_edit(
            100,
            sample_hash("trusted body"),
            BTreeMap::from([(String::from("body"), json!("updated"))]),
        ),
        |error| {
            matches!(
                error,
                FrontendReconcileError::PolicyDenied { gate, .. } if gate == "principal"
            )
        },
    )
}

fn tier2_honors_optimistic_version_check(plugin: &dyn FrontendAdapter) -> CaseOutcome {
    expect_reconcile_error(
        plugin,
        "honors_optimistic_version_check",
        sample_identity_context("hmn:known-user"),
        sample_edit(
            99,
            sample_hash("trusted body"),
            BTreeMap::from([(String::from("body"), json!("updated"))]),
        ),
        |error| matches!(error, FrontendReconcileError::Conflict { current_version } if *current_version == 100),
    )
}

fn expect_reconcile_error(
    plugin: &dyn FrontendAdapter,
    id: &'static str,
    ctx: FrontendIdentityContext,
    edit: FrontendEdit,
    matcher: impl Fn(&FrontendReconcileError) -> bool,
) -> CaseOutcome {
    let status = match plugin.reconcile(ctx, edit) {
        Err(FrontendAdapterError::Reconcile(error)) if matcher(&error) => CaseStatus::Ok,
        Err(FrontendAdapterError::NotImplemented { .. }) => CaseStatus::Pending {
            reason: "adapter did not implement reconcile contract checks",
        },
        Err(FrontendAdapterError::Reconcile(error)) => CaseStatus::Failed {
            message: format!("unexpected reconcile error: {error}"),
        },
        Err(other) => CaseStatus::Failed {
            message: format!("unexpected adapter error: {other}"),
        },
        Ok(_) => CaseStatus::Failed {
            message: "reconcile unexpectedly succeeded".to_string(),
        },
    };
    CaseOutcome {
        id,
        tier: Tier::Two,
        status,
    }
}

fn sample_identity_context(principal: &str) -> FrontendIdentityContext {
    FrontendIdentityContext {
        principal: Identity::parse(principal).expect("valid principal fixture"),
        agent: None,
        signed_intent: None,
    }
}

fn sample_edit(
    expected_version: u64,
    target_hash: BodyHash,
    field_diff: BTreeMap<String, serde_json::Value>,
) -> FrontendEdit {
    FrontendEdit {
        target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target fixture"),
        expected_version,
        target_hash,
        field_diff,
    }
}

fn sample_hash(body: &str) -> BodyHash {
    BodyHash::compute(body)
}
