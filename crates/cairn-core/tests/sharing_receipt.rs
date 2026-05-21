//! Promotion consent receipt shape and signature tests.

use std::sync::OnceLock;

use cairn_core::domain::identity::keys::SigningKey;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::sharing::{
    PromotionConsentPayload, PromotionConsentReceipt, PromotionGateInput, SharingDecisionKind,
    SharingRevocationState, verify_promotion_gate,
};
use cairn_core::domain::{
    self, CanonicalRecordHash, ConsentEvent, ConsentKind, Ed25519Signature, Identity,
    MemoryVisibility, Rfc3339Timestamp, ScopeTuple,
};
use cairn_core::policy_trace::{PolicyGate, PolicyOutcome};
use cairn_core::rebac::{RebacAction, RebacContext, RebacRelation};

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[7_u8; 32])
}

fn signer_verifying_key() -> &'static ed25519_dalek::VerifyingKey {
    static KEY: OnceLock<ed25519_dalek::VerifyingKey> = OnceLock::new();
    KEY.get_or_init(|| signer().verifying_key())
}

fn signature_for<T: serde::Serialize>(payload: &T) -> Ed25519Signature {
    let bytes = cairn_core::domain::canonical::canonical_bytes(payload).expect("canonical bytes");
    let sig = signer().sign(&bytes);
    Ed25519Signature::parse(format!("ed25519:{}", hex(&sig.to_bytes()))).expect("signature")
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn scoped_record() -> cairn_core::domain::MemoryRecord {
    let mut record = sample_record();
    record.scope = receipt_scope();
    record.visibility = MemoryVisibility::Private;
    record
}

fn receipt_scope() -> ScopeTuple {
    ScopeTuple {
        tenant: Some("default".to_owned()),
        workspace: Some("vault-a".to_owned()),
        entity: Some("ingest".to_owned()),
        user: Some("hmn:tafeng".to_owned()),
        ..ScopeTuple::default()
    }
}

fn receipt_payload() -> PromotionConsentPayload {
    let record = scoped_record();
    PromotionConsentPayload {
        operation_id: "01HQZX9F5N0000000000000002".to_owned(),
        nonce: "AAAAAAAAAAAAAAAAAAAAAA==".to_owned(),
        chain_parents: vec!["01HQZX9F5N0000000000000003".to_owned()],
        target_hash: CanonicalRecordHash::compute(&record)
            .expect("record hash")
            .as_str()
            .to_owned(),
        target_id_hash: format!("hash:{}", "b".repeat(32)),
        from_tier: MemoryVisibility::Private,
        to_tier: MemoryVisibility::Team,
        scope: receipt_scope(),
        human_identity: Identity::parse("hmn:tafeng").expect("human"),
        issued_at: Rfc3339Timestamp::parse("2026-05-21T12:00:00Z").expect("issued"),
        expires_at: Rfc3339Timestamp::parse("2026-05-22T12:00:00Z").expect("expires"),
        key_version: 1,
    }
}

fn signed_receipt() -> PromotionConsentReceipt {
    let payload = receipt_payload();
    let signature = signature_for(&payload);
    PromotionConsentReceipt {
        receipt_id: "rcpt-01HQZX9F5N0000000000000002".to_owned(),
        payload,
        signature,
    }
}

fn assert_receipt_shape_err(
    name: &str,
    mutate: impl FnOnce(&mut PromotionConsentReceipt),
    expected: fn(domain::DomainError) -> bool,
) {
    let mut receipt = signed_receipt();
    mutate(&mut receipt);
    let err = receipt
        .validate_shape()
        .expect_err(&format!("{name} should fail shape validation"));
    assert!(expected(err), "{name} returned unexpected error");
}

#[test]
fn promotion_receipt_signature_verifies() {
    let receipt = signed_receipt();
    receipt
        .verify_signature(&signer().verifying_key())
        .expect("signature verifies");
}

#[test]
fn promotion_receipt_rejects_tampered_target_hash() {
    let mut receipt = signed_receipt();
    receipt.payload.target_hash = format!("sha256:{}", "0".repeat(64));
    let err = receipt
        .verify_signature(&signer().verifying_key())
        .expect_err("tampering breaks signature");
    assert!(matches!(
        err,
        cairn_core::domain::DomainError::InvalidSignature
    ));
}

#[test]
fn promotion_receipt_shape_rejects_raw_target_id_hash() {
    let mut receipt = signed_receipt();
    receipt.payload.target_id_hash = "01HQZX9F5N0000000000000000".to_owned();
    let err = receipt.validate_shape().expect_err("raw id rejected");
    assert!(matches!(
        err,
        cairn_core::domain::DomainError::InvalidPayloadHash { .. }
    ));
}

#[test]
fn promotion_receipt_rejects_rewrapped_receipt_id() {
    assert_receipt_shape_err(
        "rewrapped receipt_id",
        |receipt| receipt.receipt_id = "rcpt-01HQZX9F5N0000000000000009".to_owned(),
        |err| matches!(err, domain::DomainError::MalformedScope { .. }),
    );
}

#[test]
fn promotion_receipt_shape_rejects_required_bad_fields() {
    let cases: &[(
        &str,
        fn(&mut PromotionConsentReceipt),
        fn(domain::DomainError) -> bool,
    )] = &[
        (
            "malformed receipt_id",
            |r| r.receipt_id = "bad id".to_owned(),
            |e| matches!(e, domain::DomainError::MalformedScope { .. }),
        ),
        (
            "malformed operation_id",
            |r| {
                r.payload.operation_id = "not-a-ulid".to_owned();
                r.receipt_id = "rcpt-not-a-ulid".to_owned();
            },
            |e| matches!(e, domain::DomainError::MalformedScope { .. }),
        ),
        (
            "malformed nonce",
            |r| r.payload.nonce = "not-base64".to_owned(),
            |e| matches!(e, domain::DomainError::MissingSignature { .. }),
        ),
        (
            "bad target_hash",
            |r| {
                r.payload.target_hash = format!("sha256:{}", "A".repeat(64));
            },
            |e| matches!(e, domain::DomainError::InvalidPayloadHash { .. }),
        ),
        (
            "invalid tier",
            |r| r.payload.to_tier = MemoryVisibility::Private,
            |e| matches!(e, domain::DomainError::UnsupportedVisibility { .. }),
        ),
        (
            "non-human signer",
            |r| {
                r.payload.human_identity =
                    Identity::parse("agt:cairn-cli:default:reader:v1").expect("agent");
            },
            |e| matches!(e, domain::DomainError::Unauthorized { .. }),
        ),
        (
            "zero key_version",
            |r| r.payload.key_version = 0,
            |e| matches!(e, domain::DomainError::Unauthorized { .. }),
        ),
        (
            "expires at issued_at",
            |r| r.payload.expires_at = r.payload.issued_at.clone(),
            |e| matches!(e, domain::DomainError::ExpiredIntent { .. }),
        ),
    ];

    for (name, mutate, expected) in cases {
        assert_receipt_shape_err(name, *mutate, *expected);
    }
}

#[test]
fn promotion_receipt_denies_unknown_wrapper_and_payload_fields() {
    let wrapper_err = serde_json::from_value::<PromotionConsentReceipt>(serde_json::json!({
        "receipt_id": "rcpt-01HQZX9F5N0000000000000002",
        "payload": receipt_payload(),
        "signature": signature_for(&receipt_payload()),
        "unexpected": true
    }))
    .expect_err("wrapper unknown field rejected");
    assert!(wrapper_err.to_string().contains("unknown field"));

    let payload_err = serde_json::from_value::<PromotionConsentPayload>(serde_json::json!({
        "operation_id": "01HQZX9F5N0000000000000002",
        "nonce": "AAAAAAAAAAAAAAAAAAAAAA==",
        "chain_parents": ["01HQZX9F5N0000000000000003"],
        "target_hash": receipt_payload().target_hash,
        "target_id_hash": format!("hash:{}", "b".repeat(32)),
        "from_tier": "private",
        "to_tier": "team",
        "scope": receipt_scope(),
        "human_identity": "hmn:tafeng",
        "issued_at": "2026-05-21T12:00:00Z",
        "expires_at": "2026-05-22T12:00:00Z",
        "key_version": 1,
        "unexpected": true
    }))
    .expect_err("payload unknown field rejected");
    assert!(payload_err.to_string().contains("unknown field"));
}

fn rebac_for_team_write() -> RebacContext {
    let principal = Identity::parse("hmn:tafeng").expect("human");
    let scope = receipt_scope();
    RebacContext::new(
        principal.clone(),
        vec![RebacRelation::new(
            principal,
            RebacAction::Write,
            scope,
            MemoryVisibility::Team,
        )],
    )
}

fn rebac_for_other_team_write() -> RebacContext {
    let principal = Identity::parse("hmn:other").expect("human");
    let scope = receipt_scope();
    RebacContext::new(
        principal.clone(),
        vec![RebacRelation::new(
            principal,
            RebacAction::Write,
            scope,
            MemoryVisibility::Team,
        )],
    )
}

fn promotion_input<'a>(
    record: &'a cairn_core::domain::MemoryRecord,
    receipt: &'a PromotionConsentReceipt,
    now: &'a Rfc3339Timestamp,
    revocation: &'a SharingRevocationState,
    rebac: &'a RebacContext,
) -> PromotionGateInput<'a> {
    PromotionGateInput {
        record,
        from_tier: MemoryVisibility::Private,
        to_tier: MemoryVisibility::Team,
        receipt,
        now,
        operation_id: "01HQZX9F5N0000000000000002",
        signer_key: signer_verifying_key(),
        revocation,
        rebac,
    }
}

fn assert_promotion_rejection_detail(input: PromotionGateInput<'_>, expected: SharingDecisionKind) {
    let rejection = verify_promotion_gate(input).expect_err("promotion gate should reject");
    assert_eq!(rejection.trace.gate, PolicyGate::ConsentReceipt);
    assert_eq!(rejection.trace.outcome, PolicyOutcome::Deny);
    assert_eq!(
        rejection.trace.detail.to_wire_string(),
        format!("consent:promote:{}", expected.as_str())
    );
}

#[test]
fn promotion_gate_allows_valid_receipt_and_rebac_relation() {
    let record = scoped_record();
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    let trace = verify_promotion_gate(promotion_input(
        &record,
        &receipt,
        &now,
        &revocation,
        &rebac,
    ))
    .expect("promotion gate allows valid receipt");

    assert_eq!(trace.gate, PolicyGate::ConsentReceipt);
    assert_eq!(trace.outcome, PolicyOutcome::Pass);
    assert_eq!(trace.detail.to_wire_string(), "consent:promote:allowed");
}

#[test]
fn promotion_gate_rejects_invalid_shape() {
    let record = scoped_record();
    let mut receipt = signed_receipt();
    receipt.payload.nonce = "not-base64".to_owned();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::InvalidShape,
    );
}

#[test]
fn promotion_gate_rejects_bad_signature() {
    let record = scoped_record();
    let mut receipt = signed_receipt();
    receipt.payload.target_id_hash = format!("hash:{}", "c".repeat(32));
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::BadSignature,
    );
}

#[test]
fn promotion_gate_rejects_expired_receipt_at_apply_time() {
    let record = scoped_record();
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-23T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::Expired,
    );
}

#[test]
fn promotion_gate_rejects_target_hash_mismatch() {
    let mut record = scoped_record();
    let receipt = signed_receipt();
    record.body.push_str(" changed after signing");
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::TargetMismatch,
    );
}

#[test]
fn promotion_gate_rejects_scope_mismatch() {
    let record = scoped_record();
    let mut receipt = signed_receipt();
    receipt.payload.scope.workspace = Some("vault-b".to_owned());
    receipt.signature = signature_for(&receipt.payload);
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::ScopeMismatch,
    );
}

#[test]
fn promotion_gate_rejects_operation_id_mismatch() {
    let record = scoped_record();
    let mut receipt = signed_receipt();
    receipt.payload.operation_id = "01HQZX9F5N0000000000000004".to_owned();
    receipt.receipt_id = "rcpt-01HQZX9F5N0000000000000004".to_owned();
    receipt.signature = signature_for(&receipt.payload);
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::ScopeMismatch,
    );
}

#[test]
fn promotion_gate_rejects_tier_mismatch() {
    let record = scoped_record();
    let mut receipt = signed_receipt();
    receipt.payload.from_tier = MemoryVisibility::Session;
    receipt.signature = signature_for(&receipt.payload);
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::TierMismatch,
    );
}

#[test]
fn promotion_gate_rejects_record_visibility_mismatch() {
    let mut record = scoped_record();
    record.visibility = MemoryVisibility::Session;
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::TierMismatch,
    );
}

#[test]
fn promotion_gate_rejects_revoked_receipt() {
    let record = scoped_record();
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let mut revocation = SharingRevocationState::default();
    revocation
        .revoked_receipt_ids
        .insert("rcpt-01HQZX9F5N0000000000000002".to_owned());
    let rebac = rebac_for_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::Revoked,
    );
}

#[test]
fn promotion_gate_rejects_revoked_signer_key() {
    let record = scoped_record();
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState {
        signer_key_revoked: true,
        ..SharingRevocationState::default()
    };
    let rebac = rebac_for_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::Revoked,
    );
}

#[test]
fn promotion_gate_rejects_non_human_signer() {
    let record = scoped_record();
    let mut receipt = signed_receipt();
    receipt.payload.human_identity =
        Identity::parse("agt:cairn-cli:default:reader:v1").expect("agent");
    receipt.signature = signature_for(&receipt.payload);
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::NotHuman,
    );
}

#[test]
fn promotion_gate_rejects_rebac_principal_mismatch() {
    let record = scoped_record();
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_other_team_write();

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::NoRebacRelation,
    );
}

#[test]
fn promotion_gate_rejects_missing_rebac_write_relation() {
    let record = scoped_record();
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = RebacContext::for_principal(Identity::parse("hmn:tafeng").expect("human"));

    assert_promotion_rejection_detail(
        promotion_input(&record, &receipt, &now, &revocation, &rebac),
        SharingDecisionKind::NoRebacRelation,
    );
}

#[test]
fn promotion_receipt_consent_event_is_body_free_and_valid() {
    let receipt = signed_receipt();
    let payload = receipt.promote_consent_payload();
    let event = ConsentEvent {
        consent_id: "01HQZX9F5N0000000000000005".to_owned(),
        kind: ConsentKind::PromoteReceipt,
        actor: Identity::parse("hmn:tafeng").expect("human"),
        subject: receipt.payload.target_id_hash.clone(),
        scope: "tenant=default,workspace=vault-a,entity=ingest,user=hmn:tafeng".to_owned(),
        op_id: Some(receipt.payload.operation_id.clone()),
        sensor_id: None,
        payload,
        decided_at: Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("decided"),
        expires_at: Some(receipt.payload.expires_at.clone()),
    };

    event.validate().expect("journal event valid");
    let value = serde_json::to_value(&event).expect("json");
    let serialized = value.to_string();
    for banned in [
        "\"body\"",
        "\"text\"",
        "\"content\"",
        "\"raw\"",
        "\"snippet\"",
        "\"command\"",
        "\"url\"",
        "\"title\"",
        "\"file_path\"",
        "\"input\"",
        "\"message\"",
    ] {
        assert!(!serialized.contains(banned), "banned field {banned}");
    }
}
