#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::conformance::{CaseStatus, run_conformance_for_plugin};
use cairn_core::contract::frontend_adapter::{
    FrontendAdapter, FrontendAdapterError, FrontendBackendState, FrontendEdit,
    FrontendIdentityContext, FrontendProjection, FrontendProjectionRequest, FrontendReconcileError,
};
use cairn_core::contract::memory_store::StoredRecord;
use cairn_core::contract::registry::{PluginName, PluginRegistry};
use cairn_core::domain::{
    ActorChainEntry, CanonicalRecordHash, ChainRole, EvidenceVector, Identity, MemoryClass,
    MemoryKind, MemoryRecord, MemoryVisibility, Provenance, Rfc3339Timestamp, ScopeTuple, TargetId,
};
use cairn_frontend_logseq::LogseqFrontendAdapter;
use pretty_assertions::assert_eq;

#[test]
fn logseq_declares_outline_markdown_capabilities() {
    let adapter = LogseqFrontendAdapter;
    let caps = adapter.capabilities();

    assert!(caps.frontmatter);
    assert!(caps.sidecar_files);
    assert!(caps.live_plugin);
    assert!(caps.graph_view);
    assert_eq!(caps.max_frontmatter_fields, 14);
}

#[test]
fn logseq_projects_frontmatter_sidecars_and_outline() {
    let adapter = LogseqFrontendAdapter;
    let projection = adapter
        .project(&sample_projection_request())
        .expect("projection succeeds");

    assert_eq!(projection.body, "trusted body");
    assert!(
        projection
            .frontmatter
            .iter()
            .any(|(key, value)| key == "version" && value == "100")
    );
    assert!(
        projection
            .sidecars
            .iter()
            .any(|(name, body)| name == "timeline.md" && body.contains("version: 100"))
    );
    assert!(
        projection
            .sidecars
            .iter()
            .any(|(name, body)| name == "evidence.md" && body.contains("confidence: 0.7"))
    );
    assert!(
        projection
            .sidecars
            .iter()
            .any(|(name, body)| name == "consent.md" && body.contains("visibility: private"))
    );
    assert!(
        projection
            .sidecars
            .iter()
            .any(|(name, body)| name == "outline.md" && body.contains("- trusted body"))
    );
    assert!(
        projection
            .sidecars
            .iter()
            .any(|(name, body)| name == "backlinks.md" && body.contains("source: 01HQZX"))
    );
    assert!(
        projection
            .sidecars
            .iter()
            .any(|(name, body)| name == "live.md" && body.contains("adapter: logseq"))
    );
}

#[test]
fn logseq_projection_snapshot() {
    let adapter = LogseqFrontendAdapter;
    let projection = adapter
        .project(&sample_projection_request())
        .expect("projection succeeds");

    insta::assert_json_snapshot!("logseq_projection", projection_snapshot(&projection));
}

#[test]
fn logseq_reconcile_preserves_mutable_edit_request() {
    let adapter = LogseqFrontendAdapter;
    let request = sample_projection_request();
    let projection = adapter.project(&request).expect("projection succeeds");
    let reconcile = adapter
        .reconcile(
            FrontendIdentityContext {
                principal: Identity::parse("hmn:known-user").expect("valid identity"),
                agent: None,
                signed_intent: sample_signed_intent("2026-04-22T14:07:11Z"),
            },
            FrontendEdit {
                target_id: request.target_id.clone(),
                expected_version: request.expected_version,
                target_hash: projection.target_hash,
                field_diff: std::collections::BTreeMap::from([(
                    "tags".to_string(),
                    serde_json::json!(["pref", "logseq"]),
                )]),
            },
        )
        .expect("mutable tag edit reconciles");

    assert_eq!(reconcile.target_id, request.target_id);
    assert_eq!(
        reconcile.field_diff.get("tags"),
        Some(&serde_json::json!(["pref", "logseq"]))
    );
}

#[test]
fn logseq_reconcile_rejects_reverse_edit_conflicts() {
    let adapter = LogseqFrontendAdapter;
    let err = adapter
        .reconcile(
            FrontendIdentityContext {
                principal: Identity::parse("hmn:known-user").expect("valid identity"),
                agent: None,
                signed_intent: sample_signed_intent("2026-04-22T14:07:11Z"),
            },
            FrontendEdit {
                target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id"),
                expected_version: 99,
                target_hash: sample_target_hash(),
                field_diff: std::collections::BTreeMap::from([(
                    "body".to_string(),
                    serde_json::json!("stale edit"),
                )]),
            },
        )
        .expect_err("stale frontend version must conflict");

    assert!(matches!(
        err,
        FrontendAdapterError::Reconcile(FrontendReconcileError::Conflict {
            current_version: 100
        })
    ));
}

#[test]
fn logseq_reconcile_rejects_immutable_reverse_edits() {
    let adapter = LogseqFrontendAdapter;
    let err = adapter
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
                    "operation_id".to_string(),
                    serde_json::json!("mutated"),
                )]),
            },
        )
        .expect_err("immutable field mutation must reject");

    assert!(matches!(
        err,
        FrontendAdapterError::Reconcile(FrontendReconcileError::ImmutableFieldChanged {
            field
        }) if field == "operation_id"
    ));
}

fn projection_snapshot(projection: &FrontendProjection) -> serde_json::Value {
    serde_json::json!({
        "body": projection.body,
        "frontmatter": projection.frontmatter,
        "sidecars": projection
            .sidecars
            .iter()
            .map(|(name, body)| serde_json::json!({
                "name": name,
                "body": body,
            }))
            .collect::<Vec<_>>(),
    })
}

#[test]
fn logseq_passes_frontend_adapter_conformance() {
    let mut reg = PluginRegistry::new();
    cairn_frontend_logseq::register(&mut reg).expect("adapter registers");
    let name = PluginName::new("cairn-frontend-logseq").expect("valid plugin name");

    let resolved = reg.frontend_adapter(&name).expect("registered adapter");
    assert!(Arc::ptr_eq(
        &resolved,
        &reg.frontend_adapter(&name).expect("registered adapter")
    ));

    let outcomes = run_conformance_for_plugin(&reg, &name);
    for outcome in outcomes {
        assert!(
            matches!(outcome.status, CaseStatus::Ok),
            "conformance case {} failed: {:?}",
            outcome.id,
            outcome.status
        );
    }
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

fn sample_target_hash() -> CanonicalRecordHash {
    CanonicalRecordHash::compute(&sample_record()).expect("sample record hashes")
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
        body: "trusted body".to_owned(),
        source_ids: vec![
            cairn_core::domain::SourceId::parse("01HQZX9F5N0000000000000001")
                .expect("valid source id"),
        ],
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid identity"),
            created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid timestamp"),
            originating_agent_id: user_id.clone(),
            source_ids: vec![
                cairn_core::domain::SourceId::parse("01HQZX9F5N0000000000000001")
                    .expect("valid source id"),
            ],
            source_hash: format!("sha256:{}", "a".repeat(64)),
            consent_ref: "consent:01HQZ".to_owned(),
            llm_id_if_any: None,
            source_refs: Vec::new(),
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
