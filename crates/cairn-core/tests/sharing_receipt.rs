//! Promotion consent receipt shape and signature tests.

use cairn_core::domain::identity::keys::SigningKey;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::sharing::{PromotionConsentPayload, PromotionConsentReceipt};
use cairn_core::domain::{
    self, CanonicalRecordHash, Ed25519Signature, Identity, MemoryVisibility, Rfc3339Timestamp,
    ScopeTuple,
};

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[7_u8; 32])
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
