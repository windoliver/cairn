//! Frontend adapter contract coverage for issue #113.
//!
//! These tests are written against the planned frontend contract surface so
//! they fail red until the production types land.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::conformance::{CaseStatus, Tier, run_conformance_for_plugin};
use cairn_core::contract::frontend_adapter::{
    FrontendAdapter, FrontendAdapterCapabilities, FrontendAdapterError, FrontendBackendState,
    FrontendEdit, FrontendFieldClass, FrontendFieldPolicy, FrontendIdentityContext,
    FrontendProjection, FrontendProjectionRequest, FrontendReconcileError,
    FrontendReconcileRequest,
};
use cairn_core::contract::manifest::PluginManifest;
use cairn_core::contract::memory_store::StoredRecord;
use cairn_core::contract::registry::{PluginName, PluginRegistry};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::{
    ActorChainEntry, CanonicalRecordHash, ChainRole, EvidenceVector, Identity, MemoryClass,
    MemoryKind, MemoryRecord, MemoryVisibility, Provenance, Rfc3339Timestamp, ScopeTuple, TargetId,
};

const FRONTEND_MANIFEST: &str = r#"
name = "stub-frontend-runner"
contract = "FrontendAdapter"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
frontmatter = false
sidecar_files = false
live_plugin = false
graph_view = false
"#;

#[derive(Default)]
struct StubFrontend;

#[async_trait::async_trait]
impl FrontendAdapter for StubFrontend {
    fn name(&self) -> &'static str {
        "stub-frontend-runner"
    }

    fn capabilities(&self) -> &FrontendAdapterCapabilities {
        static CAPS: FrontendAdapterCapabilities = FrontendAdapterCapabilities {
            frontmatter: false,
            sidecar_files: false,
            live_plugin: false,
            graph_view: false,
            max_frontmatter_fields: 0,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
}

#[derive(Default)]
struct ConformanceFrontend;

#[async_trait::async_trait]
impl FrontendAdapter for ConformanceFrontend {
    fn name(&self) -> &'static str {
        "stub-frontend-runner"
    }

    fn capabilities(&self) -> &FrontendAdapterCapabilities {
        static CAPS: FrontendAdapterCapabilities = FrontendAdapterCapabilities {
            frontmatter: false,
            sidecar_files: false,
            live_plugin: false,
            graph_view: false,
            max_frontmatter_fields: 0,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    fn project(
        &self,
        request: &FrontendProjectionRequest,
    ) -> Result<FrontendProjection, FrontendAdapterError> {
        Ok(FrontendProjection {
            body: request.backend.stored.record.body.clone(),
            frontmatter: vec![("version".into(), request.backend.stored.version.to_string())],
            sidecars: Vec::new(),
            target_hash: request.backend.target_hash.clone(),
        })
    }

    fn reconcile(
        &self,
        ctx: FrontendIdentityContext,
        edit: FrontendEdit,
    ) -> Result<FrontendReconcileRequest, FrontendAdapterError> {
        if edit
            .field_diff
            .keys()
            .any(|field| !FrontendFieldPolicy::is_mutable_from_frontend(field))
        {
            let field = edit
                .field_diff
                .keys()
                .find(|field| !FrontendFieldPolicy::is_mutable_from_frontend(field))
                .expect("immutable field present")
                .clone();
            return Err(FrontendReconcileError::ImmutableFieldChanged { field }.into());
        }
        if edit
            .field_diff
            .get("body")
            .and_then(serde_json::Value::as_str)
            == Some("replay://operation")
        {
            return Err(FrontendReconcileError::ReplayDetected.into());
        }
        if ctx.signed_intent.expires_at.as_str() != "2026-04-22T14:07:11Z" {
            return Err(FrontendReconcileError::ExpiredIntent {
                issued_at: ctx.signed_intent.issued_at.clone(),
                expires_at: ctx.signed_intent.expires_at.clone(),
                now: "2026-04-22T15:00:00Z".into(),
            }
            .into());
        }
        if ctx.principal.as_str() != "hmn:known-user" {
            return Err(FrontendReconcileError::QuarantineRequired {
                reason: "principal is not registered for this adapter".into(),
                quarantine_id: Some("01HQZX9F5N0000000000000001".into()),
            }
            .into());
        }
        if edit.expected_version != 100 {
            return Err(FrontendReconcileError::Conflict {
                current_version: 100,
            }
            .into());
        }
        if edit.target_hash != sample_target_hash() {
            return Err(FrontendReconcileError::PolicyDenied {
                gate: "target_hash".into(),
                reason: "projection hash does not match canonical record hash".into(),
            }
            .into());
        }
        Ok(FrontendReconcileRequest {
            target_id: edit.target_id,
            expected_version: edit.expected_version,
            target_hash: edit.target_hash,
            field_diff: edit.field_diff,
            ctx,
        })
    }
}

#[test]
fn frontend_field_policy_allows_user_content_and_metadata_only() {
    assert!(FrontendFieldPolicy::is_mutable_from_frontend("body"));
    assert!(FrontendFieldPolicy::is_mutable_from_frontend("tags"));
    assert!(FrontendFieldPolicy::is_mutable_from_frontend(
        "last_read_at"
    ));

    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("kind"));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend(
        "operation_id"
    ));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("visibility"));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("version"));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend(
        "unknown_future_field"
    ));
}

#[test]
fn frontend_field_policy_classifies_unknown_fields_as_version_audit() {
    assert_eq!(
        FrontendFieldPolicy::classify("unknown_future_field"),
        FrontendFieldClass::VersionAudit
    );
}

#[test]
fn frontend_capabilities_default_to_no_projection_features() {
    let caps = FrontendAdapterCapabilities::default();
    assert!(!caps.frontmatter);
    assert!(!caps.sidecar_files);
    assert!(!caps.live_plugin);
    assert!(!caps.graph_view);
    assert_eq!(caps.max_frontmatter_fields, 0);
}

#[test]
fn frontend_reconcile_error_exposes_immutable_field_variant() {
    let err = FrontendReconcileError::ImmutableFieldChanged {
        field: "operation_id".into(),
    };
    let rendered = err.to_string();
    assert!(rendered.contains("operation_id"));
}

#[test]
fn frontend_reconcile_error_exposes_expired_intent_variant() {
    let err = FrontendReconcileError::ExpiredIntent {
        issued_at: "2026-04-22T14:02:11Z".into(),
        expires_at: "2026-04-22T14:07:11Z".into(),
        now: "2026-04-22T15:00:00Z".into(),
    };
    let rendered = err.to_string();
    assert!(rendered.contains("expired"));
    assert!(rendered.contains("2026-04-22T15:00:00Z"));
}

#[test]
fn frontend_reconcile_error_exposes_quarantine_variant() {
    let err = FrontendReconcileError::QuarantineRequired {
        reason: "daemon user mismatch".into(),
        quarantine_id: Some("01HQZX9F5N0000000000000001".into()),
    };
    let rendered = err.to_string();
    assert!(rendered.contains("quarantine"));
    assert!(rendered.contains("01HQZX9F5N0000000000000001"));
}

#[test]
fn frontend_projection_request_carries_backend_state_snapshot() {
    let request = FrontendProjectionRequest {
        target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id"),
        expected_version: 100,
        backend: FrontendBackendState {
            stored: StoredRecord {
                record: sample_record(),
                version: 100,
                schema_version: None,
            },
            target_hash: sample_target_hash(),
        },
    };

    let projection = ConformanceFrontend
        .project(&request)
        .expect("projection should succeed");
    assert_eq!(projection.body, "trusted body");
    assert_eq!(
        projection.frontmatter,
        vec![("version".into(), "100".into())]
    );
    assert_eq!(projection.target_hash, sample_target_hash());
}

#[test]
fn frontend_identity_context_carries_required_signed_intent() {
    let ctx = FrontendIdentityContext {
        principal: Identity::parse("hmn:known-user").expect("valid identity"),
        agent: None,
        signed_intent: sample_signed_intent("2026-04-22T14:07:11Z"),
    };

    assert_eq!(ctx.signed_intent.issuer.0, "hmn:known-user");
    assert_eq!(ctx.signed_intent.expires_at, "2026-04-22T14:07:11Z");
}

#[test]
fn frontend_project_to_reconcile_round_trip_preserves_snapshot_binding() {
    let adapter = ConformanceFrontend;
    let request = sample_projection_request();
    let projection = adapter
        .project(&request)
        .expect("projection should succeed");

    let reconcile = adapter
        .reconcile(
            FrontendIdentityContext {
                principal: Identity::parse("hmn:known-user").expect("valid identity"),
                agent: Some(Identity::parse("agt:test:frontend:v1").expect("valid identity")),
                signed_intent: sample_signed_intent("2026-04-22T14:07:11Z"),
            },
            FrontendEdit {
                target_id: request.target_id.clone(),
                expected_version: request.expected_version,
                target_hash: projection.target_hash.clone(),
                field_diff: std::collections::BTreeMap::from([(
                    "body".into(),
                    serde_json::json!("updated body"),
                )]),
            },
        )
        .expect("reconcile should succeed");

    assert_eq!(reconcile.target_id, request.target_id);
    assert_eq!(reconcile.expected_version, request.expected_version);
    assert_eq!(reconcile.target_hash, projection.target_hash);
    assert_eq!(
        reconcile.field_diff.get("body"),
        Some(&serde_json::json!("updated body"))
    );
    assert_eq!(reconcile.ctx.principal.as_str(), "hmn:known-user");
    assert_eq!(
        reconcile
            .ctx
            .agent
            .expect("agent should round-trip")
            .as_str(),
        "agt:test:frontend:v1"
    );
}

#[test]
fn frontend_reconcile_accepts_mutable_metadata_edits() {
    let reconcile = ConformanceFrontend
        .reconcile(
            FrontendIdentityContext {
                principal: Identity::parse("hmn:known-user").expect("valid identity"),
                agent: None,
                signed_intent: sample_signed_intent("2026-04-22T14:07:11Z"),
            },
            FrontendEdit {
                target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id"),
                expected_version: 100,
                target_hash: sample_target_hash(),
                field_diff: std::collections::BTreeMap::from([(
                    "last_read_at".into(),
                    serde_json::json!("2026-04-22T14:06:11Z"),
                )]),
            },
        )
        .expect("mutable metadata edit should succeed");

    assert_eq!(
        reconcile.field_diff.get("last_read_at"),
        Some(&serde_json::json!("2026-04-22T14:06:11Z"))
    );
}

#[test]
fn frontend_reconcile_quarantines_unrecognized_principal_before_version_or_hash_checks() {
    let err = ConformanceFrontend
        .reconcile(
            FrontendIdentityContext {
                principal: Identity::parse("hmn:unknown-user").expect("valid identity"),
                agent: None,
                signed_intent: sample_signed_intent("2026-04-22T14:07:11Z"),
            },
            FrontendEdit {
                target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id"),
                expected_version: 99,
                target_hash: sample_hash("tampered body"),
                field_diff: std::collections::BTreeMap::from([(
                    "body".into(),
                    serde_json::json!("updated"),
                )]),
            },
        )
        .expect_err("unknown principal should quarantine before later checks");

    assert!(matches!(
        err,
        FrontendAdapterError::Reconcile(FrontendReconcileError::QuarantineRequired { .. })
    ));
}

#[test]
fn frontend_reconcile_rejects_expired_intent_before_quarantine() {
    let err = ConformanceFrontend
        .reconcile(
            FrontendIdentityContext {
                principal: Identity::parse("hmn:unknown-user").expect("valid identity"),
                agent: None,
                signed_intent: sample_signed_intent("2026-04-22T14:06:11Z"),
            },
            FrontendEdit {
                target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id"),
                expected_version: 100,
                target_hash: sample_target_hash(),
                field_diff: std::collections::BTreeMap::from([(
                    "body".into(),
                    serde_json::json!("updated"),
                )]),
            },
        )
        .expect_err("expired intent should fail before quarantine path");

    assert!(matches!(
        err,
        FrontendAdapterError::Reconcile(FrontendReconcileError::ExpiredIntent { .. })
    ));
}

#[test]
fn frontend_adapter_runner_reports_expected_tier2_case_ids() {
    let mut reg = PluginRegistry::new();
    let name = PluginName::new("stub-frontend-runner").expect("valid");
    let manifest = PluginManifest::parse_toml(FRONTEND_MANIFEST).expect("manifest parses");
    reg.register_frontend_adapter_with_manifest(name.clone(), manifest, Arc::new(StubFrontend))
        .expect("registers");

    let outcomes = run_conformance_for_plugin(&reg, &name);
    let tier2_ids: Vec<_> = outcomes
        .iter()
        .filter(|outcome| outcome.tier == Tier::Two)
        .map(|outcome| outcome.id)
        .collect();

    assert!(tier2_ids.contains(&"rejects_immutable_field_edits"));
    assert!(tier2_ids.contains(&"rejects_replayed_operation"));
    assert!(tier2_ids.contains(&"rejects_tampered_target_hash"));
    assert!(tier2_ids.contains(&"quarantines_unrecognized_principal"));
    assert!(tier2_ids.contains(&"honors_optimistic_version_check"));
    assert!(tier2_ids.contains(&"rejects_expired_signed_intent"));
}

#[test]
fn frontend_adapter_runner_fails_unimplemented_reconcile_cases() {
    let mut reg = PluginRegistry::new();
    let name = PluginName::new("stub-frontend-runner").expect("valid");
    let manifest = PluginManifest::parse_toml(FRONTEND_MANIFEST).expect("manifest parses");
    reg.register_frontend_adapter_with_manifest(name.clone(), manifest, Arc::new(StubFrontend))
        .expect("registers");

    let outcomes = run_conformance_for_plugin(&reg, &name);

    for id in [
        "rejects_immutable_field_edits",
        "rejects_replayed_operation",
        "rejects_tampered_target_hash",
        "quarantines_unrecognized_principal",
        "honors_optimistic_version_check",
        "rejects_expired_signed_intent",
    ] {
        let outcome = outcomes
            .iter()
            .find(|outcome| outcome.id == id)
            .expect("required tier-2 case exists");
        assert!(
            matches!(outcome.status, CaseStatus::Failed { .. }),
            "tier-2 case {id} must fail closed when reconcile is unimplemented, got {:?}",
            outcome.status
        );
    }
}

#[test]
fn frontend_adapter_runner_marks_contract_stub_tier2_cases_ok() {
    let mut reg = PluginRegistry::new();
    let name = PluginName::new("stub-frontend-runner").expect("valid");
    let manifest = PluginManifest::parse_toml(FRONTEND_MANIFEST).expect("manifest parses");
    reg.register_frontend_adapter_with_manifest(
        name.clone(),
        manifest,
        Arc::new(ConformanceFrontend),
    )
    .expect("registers");

    let outcomes = run_conformance_for_plugin(&reg, &name);

    for id in [
        "rejects_immutable_field_edits",
        "rejects_replayed_operation",
        "rejects_tampered_target_hash",
        "quarantines_unrecognized_principal",
        "honors_optimistic_version_check",
        "rejects_expired_signed_intent",
    ] {
        let outcome = outcomes
            .iter()
            .find(|outcome| outcome.id == id)
            .expect("required tier-2 case exists");
        assert!(
            matches!(outcome.status, CaseStatus::Ok),
            "tier-2 case {id} must pass, got {:?}",
            outcome.status
        );
    }
}

fn sample_target_hash() -> CanonicalRecordHash {
    CanonicalRecordHash::compute(&sample_record()).expect("sample record hashes")
}

fn sample_hash(body: &str) -> CanonicalRecordHash {
    CanonicalRecordHash::compute(&sample_record_with_body(body)).expect("sample record hashes")
}

fn sample_projection_request() -> FrontendProjectionRequest {
    FrontendProjectionRequest {
        target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id"),
        expected_version: 100,
        backend: FrontendBackendState {
            stored: StoredRecord {
                record: sample_record(),
                version: 100,
                schema_version: None,
            },
            target_hash: sample_target_hash(),
        },
    }
}

fn sample_signed_intent(expires_at: &str) -> cairn_core::generated::envelope::SignedIntent {
    serde_json::from_value(serde_json::json!({
        "chain_parents": [],
        "expires_at": expires_at,
        "issued_at": "2026-04-22T14:02:11Z",
        "issuer": "hmn:known-user",
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
        "target_hash": sample_target_hash().as_str(),
    }))
    .expect("valid signed intent fixture")
}

fn sample_record() -> MemoryRecord {
    sample_record_with_body("trusted body")
}

fn sample_record_with_body(body: &str) -> MemoryRecord {
    let user_id = Identity::parse("hmn:known-user").expect("valid identity");
    MemoryRecord {
        id: cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000000")
            .expect("valid record id"),
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
        signature: cairn_core::domain::record::Ed25519Signature::parse(format!(
            "ed25519:{}",
            "a".repeat(128)
        ))
        .expect("valid signature"),
        tags: vec!["pref".to_owned()],
        extra_frontmatter: std::collections::BTreeMap::new(),
        consent_model: None,
    }
}
