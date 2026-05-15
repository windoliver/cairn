//! End-to-end coverage for bundled frontend adapter alphas.

#![allow(missing_docs)]

use std::collections::BTreeMap;

use cairn_cli::plugins::host;
use cairn_core::contract::frontend_adapter::{
    FrontendAdapterError, FrontendBackendState, FrontendEdit, FrontendIdentityContext,
    FrontendProjectionRequest, FrontendReconcileError,
};
use cairn_core::contract::memory_store::StoredRecord;
use cairn_core::contract::registry::PluginName;
use cairn_core::domain::{
    ActorChainEntry, CanonicalRecordHash, ChainRole, EvidenceVector, Identity, MemoryClass,
    MemoryKind, MemoryRecord, MemoryVisibility, Provenance, Rfc3339Timestamp, ScopeTuple, TargetId,
};

#[test]
fn bundled_frontend_adapters_project_and_reconcile_through_host_e2e() {
    let registry = host::register_all().expect("bundled plugins register");
    let request = sample_projection_request();

    for case in frontend_cases() {
        let name = PluginName::new(case.name).expect("valid plugin name");
        let adapter = registry
            .frontend_adapter(&name)
            .expect("adapter registered");

        let projection = adapter.project(&request).expect("projection succeeds");
        assert_eq!(projection.body, "trusted body", "{}", case.name);
        assert_eq!(
            projection.target_hash, request.backend.target_hash,
            "{}",
            case.name
        );
        assert!(
            projection
                .frontmatter
                .iter()
                .any(|(key, value)| key == "version" && value == "100"),
            "{} projected version frontmatter",
            case.name
        );
        assert_sidecar_contains(&projection.sidecars, "live.md", case.live_marker);
        assert_sidecar_contains(
            &projection.sidecars,
            "live.md",
            sample_target_hash().as_str(),
        );
        for expected_sidecar in case.sidecars {
            assert!(
                projection
                    .sidecars
                    .iter()
                    .any(|(name, _)| name == expected_sidecar),
                "{} projected {expected_sidecar}",
                case.name
            );
        }

        let reconcile = adapter
            .reconcile(
                known_identity("2026-04-22T14:07:11Z", sample_target_hash()),
                edit(
                    100,
                    sample_target_hash(),
                    [("body", serde_json::json!("edited through host"))],
                ),
            )
            .expect("mutable edit reconciles through host");

        assert_eq!(reconcile.target_id, request.target_id, "{}", case.name);
        assert_eq!(
            reconcile.field_diff.get("body"),
            Some(&serde_json::json!("edited through host")),
            "{}",
            case.name
        );
    }
}

#[test]
fn bundled_frontend_adapters_fail_closed_for_corner_cases_e2e() {
    let registry = host::register_all().expect("bundled plugins register");

    for case in frontend_cases() {
        let name = PluginName::new(case.name).expect("valid plugin name");
        let adapter = registry
            .frontend_adapter(&name)
            .expect("adapter registered");

        assert_reconcile_error(
            adapter.reconcile(
                known_identity("2026-04-22T14:07:11Z", sample_target_hash()),
                edit(
                    100,
                    sample_target_hash(),
                    [("operation_id", serde_json::json!("mutated"))],
                ),
            ),
            |err| {
                matches!(
                    err,
                    FrontendReconcileError::ImmutableFieldChanged { field }
                        if field == "operation_id"
                )
            },
            case.name,
            "immutable operation_id is rejected before apply",
        );

        assert_reconcile_error(
            adapter.reconcile(
                known_identity("2026-04-22T14:07:11Z", sample_target_hash()),
                edit(
                    100,
                    sample_target_hash(),
                    [("body", serde_json::json!("replay://operation"))],
                ),
            ),
            |err| matches!(err, FrontendReconcileError::ReplayDetected),
            case.name,
            "replay sentinel is rejected",
        );

        assert_reconcile_error(
            adapter.reconcile(
                known_identity("2026-04-22T14:06:11Z", sample_target_hash()),
                edit(
                    99,
                    sample_target_hash(),
                    [("body", serde_json::json!("expired and stale"))],
                ),
            ),
            |err| matches!(err, FrontendReconcileError::ExpiredIntent { .. }),
            case.name,
            "expired intent wins over stale version",
        );

        assert_reconcile_error(
            adapter.reconcile(
                unknown_identity("2026-04-22T14:07:11Z", sample_target_hash()),
                edit(
                    99,
                    sample_target_hash(),
                    [("body", serde_json::json!("unknown and stale"))],
                ),
            ),
            |err| matches!(err, FrontendReconcileError::QuarantineRequired { .. }),
            case.name,
            "unknown principal is quarantined before stale version",
        );

        assert_reconcile_error(
            adapter.reconcile(
                known_identity("2026-04-22T14:07:11Z", sample_target_hash()),
                edit(
                    99,
                    sample_target_hash(),
                    [("body", serde_json::json!("stale"))],
                ),
            ),
            |err| {
                matches!(
                    err,
                    FrontendReconcileError::Conflict {
                        current_version: 100
                    }
                )
            },
            case.name,
            "stale version conflicts",
        );

        assert_reconcile_error(
            adapter.reconcile(
                known_identity("2026-04-22T14:07:11Z", sample_target_hash()),
                edit(
                    100,
                    alternate_target_hash(),
                    [("body", serde_json::json!("hash mismatch"))],
                ),
            ),
            |err| {
                matches!(
                    err,
                    FrontendReconcileError::PolicyDenied { gate, .. } if gate == "target_hash"
                )
            },
            case.name,
            "target hash mismatch is policy denied",
        );
    }
}

#[derive(Clone, Copy)]
struct FrontendCase {
    name: &'static str,
    live_marker: &'static str,
    sidecars: &'static [&'static str],
}

fn frontend_cases() -> [FrontendCase; 3] {
    [
        FrontendCase {
            name: "cairn-frontend-logseq",
            live_marker: "adapter: logseq",
            sidecars: &[
                "timeline.md",
                "evidence.md",
                "consent.md",
                "outline.md",
                "backlinks.md",
                "live.md",
            ],
        },
        FrontendCase {
            name: "cairn-frontend-obsidian",
            live_marker: "adapter: obsidian",
            sidecars: &[
                "timeline.md",
                "evidence.md",
                "consent.md",
                "backlinks.md",
                "live.md",
            ],
        },
        FrontendCase {
            name: "cairn-frontend-vscode",
            live_marker: "adapter: vscode",
            sidecars: &[
                "timeline.md",
                "evidence.md",
                "consent.md",
                "backlinks.md",
                "live.md",
            ],
        },
    ]
}

fn assert_sidecar_contains(sidecars: &[(String, String)], name: &str, needle: &str) {
    let (_, body) = sidecars
        .iter()
        .find(|(candidate, _)| candidate == name)
        .unwrap_or_else(|| panic!("missing sidecar {name}"));
    assert!(body.contains(needle), "{name} should contain {needle}");
}

fn assert_reconcile_error(
    result: Result<
        cairn_core::contract::frontend_adapter::FrontendReconcileRequest,
        FrontendAdapterError,
    >,
    matches_expected: impl FnOnce(FrontendReconcileError) -> bool,
    adapter_name: &str,
    label: &str,
) {
    let err = result.expect_err(label);
    let FrontendAdapterError::Reconcile(reconcile_err) = err else {
        panic!("{adapter_name}: {label}: expected reconcile error, got {err:?}");
    };
    assert!(
        matches_expected(reconcile_err.clone()),
        "{adapter_name}: {label}: unexpected error {reconcile_err:?}"
    );
}

fn edit(
    expected_version: u64,
    target_hash: CanonicalRecordHash,
    fields: impl IntoIterator<Item = (&'static str, serde_json::Value)>,
) -> FrontendEdit {
    FrontendEdit {
        target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id"),
        expected_version,
        target_hash,
        field_diff: fields
            .into_iter()
            .map(|(field, value)| (field.to_owned(), value))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn known_identity(
    expires_at: &str,
    signed_target_hash: CanonicalRecordHash,
) -> FrontendIdentityContext {
    identity_context("hmn:known-user", expires_at, signed_target_hash)
}

fn unknown_identity(
    expires_at: &str,
    signed_target_hash: CanonicalRecordHash,
) -> FrontendIdentityContext {
    identity_context("hmn:unknown-user", expires_at, signed_target_hash)
}

fn identity_context(
    principal: &str,
    expires_at: &str,
    signed_target_hash: CanonicalRecordHash,
) -> FrontendIdentityContext {
    FrontendIdentityContext {
        principal: Identity::parse(principal).expect("valid identity"),
        agent: None,
        signed_intent: serde_json::from_value(serde_json::json!({
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
            "target_hash": signed_target_hash.as_str(),
        }))
        .expect("valid signed intent fixture"),
    }
}

fn sample_projection_request() -> FrontendProjectionRequest {
    FrontendProjectionRequest {
        target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id"),
        expected_version: 100,
        backend: FrontendBackendState {
            stored: StoredRecord {
                record: sample_record("trusted body"),
                version: 100,
                schema_version: None,
            },
            target_hash: sample_target_hash(),
        },
    }
}

fn sample_target_hash() -> CanonicalRecordHash {
    CanonicalRecordHash::compute(&sample_record("trusted body")).expect("sample record hashes")
}

fn alternate_target_hash() -> CanonicalRecordHash {
    CanonicalRecordHash::compute(&sample_record("alternate body")).expect("sample record hashes")
}

fn sample_record(body: &str) -> MemoryRecord {
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
        extra_frontmatter: BTreeMap::new(),
        consent_model: None,
    }
}
