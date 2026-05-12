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
use crate::contract::frontend_adapter::CONTRACT_VERSION;
use crate::contract::frontend_adapter::{
    FrontendAdapter, FrontendAdapterError, FrontendEdit, FrontendIdentityContext,
    FrontendReconcileError,
};
use crate::contract::registry::{PluginName, PluginRegistry};
use crate::domain::{
    ActorChainEntry, CanonicalRecordHash, ChainRole, EvidenceVector, Identity, MemoryClass,
    MemoryKind, MemoryRecord, MemoryVisibility, Provenance, Rfc3339Timestamp, ScopeTuple, TargetId,
};

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
        tier2_quarantines_unrecognized_principal(&*plugin),
        tier2_honors_optimistic_version_check(&*plugin),
        tier2_rejects_expired_signed_intent(&*plugin),
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
        sample_identity_context("hmn:known-user", "2026-04-22T14:07:11Z"),
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
        sample_identity_context("hmn:known-user", "2026-04-22T14:07:11Z"),
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
        sample_identity_context("hmn:known-user", "2026-04-22T14:07:11Z"),
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

fn tier2_quarantines_unrecognized_principal(plugin: &dyn FrontendAdapter) -> CaseOutcome {
    expect_reconcile_error(
        plugin,
        "quarantines_unrecognized_principal",
        sample_identity_context("hmn:unknown-user", "2026-04-22T14:07:11Z"),
        sample_edit(
            100,
            sample_hash("trusted body"),
            BTreeMap::from([(String::from("body"), json!("updated"))]),
        ),
        |error| {
            matches!(
                error,
                FrontendReconcileError::QuarantineRequired { quarantine_id, .. }
                    if quarantine_id.as_deref() == Some("01HQZX9F5N0000000000000001")
            )
        },
    )
}

fn tier2_honors_optimistic_version_check(plugin: &dyn FrontendAdapter) -> CaseOutcome {
    expect_reconcile_error(
        plugin,
        "honors_optimistic_version_check",
        sample_identity_context("hmn:known-user", "2026-04-22T14:07:11Z"),
        sample_edit(
            99,
            sample_hash("trusted body"),
            BTreeMap::from([(String::from("body"), json!("updated"))]),
        ),
        |error| matches!(error, FrontendReconcileError::Conflict { current_version } if *current_version == 100),
    )
}

fn tier2_rejects_expired_signed_intent(plugin: &dyn FrontendAdapter) -> CaseOutcome {
    expect_reconcile_error(
        plugin,
        "rejects_expired_signed_intent",
        sample_identity_context("hmn:known-user", "2026-04-22T14:06:11Z"),
        sample_edit(
            100,
            sample_hash("trusted body"),
            BTreeMap::from([(String::from("body"), json!("updated"))]),
        ),
        |error| {
            matches!(
                error,
                FrontendReconcileError::ExpiredIntent { now, .. }
                    if now == "2026-04-22T15:00:00Z"
            )
        },
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
        Err(FrontendAdapterError::NotImplemented { operation }) => CaseStatus::Failed {
            message: format!(
                "adapter does not implement required conformance operation {operation}"
            ),
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

fn sample_identity_context(principal: &str, expires_at: &str) -> FrontendIdentityContext {
    FrontendIdentityContext {
        principal: Identity::parse(principal).expect("valid principal fixture"),
        agent: None,
        signed_intent: sample_signed_intent(principal, expires_at),
    }
}

fn sample_edit(
    expected_version: u64,
    target_hash: CanonicalRecordHash,
    field_diff: BTreeMap<String, serde_json::Value>,
) -> FrontendEdit {
    FrontendEdit {
        target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target fixture"),
        expected_version,
        target_hash,
        field_diff,
    }
}

fn sample_hash(body: &str) -> CanonicalRecordHash {
    CanonicalRecordHash::compute(&sample_record(body)).expect("sample record hashes")
}

fn sample_signed_intent(
    principal: &str,
    expires_at: &str,
) -> crate::generated::envelope::SignedIntent {
    serde_json::from_value(serde_json::json!({
        "chain_parents": [],
        "expires_at": expires_at,
        "issued_at": "2026-04-22T14:02:11Z",
        "issuer": principal,
        "key_version": 1,
        "nonce": "YWJjZGVmZ2hpamtsbW5vcA==",
        "operation_id": "01HQZX9F5N0000000000000001",
        "scope": {
            "entity": "ent",
            "tenant": "acme",
            "tier": "project",
            "workspace": "ws"
        },
        "server_challenge": "YWJjZGVmZ2hpamtsbW5vcA==",
        "signature": format!("ed25519:{}", "a".repeat(128)),
        "target_hash": sample_hash("trusted body").as_str(),
    }))
    .expect("valid signed intent fixture")
}

fn sample_record(body: &str) -> MemoryRecord {
    let user_id = Identity::parse("hmn:known-user").expect("valid identity");
    MemoryRecord {
        id: crate::domain::RecordId::parse("01HQZX9F5N0000000000000000").expect("valid record id"),
        target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("hmn:known-user".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid identity"),
            created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid timestamp"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "a".repeat(64)),
            consent_ref: "consent:01HQZ".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-22T14:05:11Z").expect("valid timestamp"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid timestamp"),
        }],
        signature: crate::domain::record::Ed25519Signature::parse(format!(
            "ed25519:{}",
            "a".repeat(128)
        ))
        .expect("valid signature"),
        tags: vec!["pref".to_owned()],
        extra_frontmatter: BTreeMap::new(),
        consent_model: None,
    }
}
