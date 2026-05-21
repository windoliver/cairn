//! Promotion consent receipt shape and signature tests.

use cairn_core::domain::identity::keys::SigningKey;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::sharing::{PromotionConsentPayload, PromotionConsentReceipt};
use cairn_core::domain::{
    CanonicalRecordHash, Ed25519Signature, Identity, MemoryVisibility, Rfc3339Timestamp, ScopeTuple,
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
            | cairn_core::domain::DomainError::ScopeDenied { .. }
    ));
}
