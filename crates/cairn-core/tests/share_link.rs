//! Signed share-link shape and signature tests.

use cairn_core::domain::identity::keys::SigningKey;
use cairn_core::domain::sharing::{ShareLinkPayload, SharingRevocationState, SignedShareLink};
use cairn_core::domain::{
    Ed25519Signature, Identity, MemoryVisibility, Rfc3339Timestamp, ScopeTuple,
};

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[9_u8; 32])
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

fn scope() -> ScopeTuple {
    ScopeTuple {
        tenant: Some("default".to_owned()),
        workspace: Some("vault-a".to_owned()),
        entity: Some("session".to_owned()),
        ..ScopeTuple::default()
    }
}

fn link_payload() -> ShareLinkPayload {
    ShareLinkPayload {
        operation_id: "01HQZX9F5N0000000000000004".to_owned(),
        nonce: "AAAAAAAAAAAAAAAAAAAAAA==".to_owned(),
        target_hash: format!("sha256:{}", "c".repeat(64)),
        target_id_hashes: vec![format!("hash:{}", "d".repeat(32))],
        scope: scope(),
        grant_tier: MemoryVisibility::Team,
        grantee: Some(Identity::parse("agt:cairn-cli:default:reader:v1").expect("agent")),
        issuer: Identity::parse("hmn:tafeng").expect("human"),
        issued_at: Rfc3339Timestamp::parse("2026-05-21T12:00:00Z").expect("issued"),
        expires_at: Rfc3339Timestamp::parse("2026-05-22T12:00:00Z").expect("expires"),
        key_version: 1,
    }
}

fn signed_link() -> SignedShareLink {
    let payload = link_payload();
    let signature = signature_for(&payload);
    SignedShareLink {
        link_id: "share-01HQZX9F5N0000000000000004".to_owned(),
        payload,
        signature,
    }
}

#[test]
fn share_link_signature_verifies() {
    let link = signed_link();
    link.verify_signature(&signer().verifying_key())
        .expect("signature verifies");
}

#[test]
fn share_link_rejects_tampered_scope() {
    let mut link = signed_link();
    link.payload.scope.entity = Some("other".to_owned());
    let err = link
        .verify_signature(&signer().verifying_key())
        .expect_err("tampering breaks signature");
    assert!(matches!(
        err,
        cairn_core::domain::DomainError::InvalidSignature
    ));
}

#[test]
fn share_link_shape_rejects_empty_target_id_hashes() {
    let mut link = signed_link();
    link.payload.target_id_hashes.clear();
    let err = link
        .validate_shape()
        .expect_err("empty target set rejected");
    assert!(matches!(
        err,
        cairn_core::domain::DomainError::InvalidPayloadHash { .. }
    ));
}

#[test]
fn sharing_revocation_state_serializes_and_denies_unknown_fields() {
    let mut state = SharingRevocationState::default();
    state
        .revoked_receipt_ids
        .insert("rcpt-01HQZX9F5N0000000000000002".to_owned());
    state
        .revoked_share_link_ids
        .insert("share-01HQZX9F5N0000000000000004".to_owned());
    state.signer_key_revoked = true;

    let json = serde_json::to_value(&state).expect("serialize revocation state");
    assert_eq!(json["signer_key_revoked"], true);

    let err = serde_json::from_value::<SharingRevocationState>(serde_json::json!({
        "revoked_receipt_ids": [],
        "revoked_share_link_ids": [],
        "signer_key_revoked": false,
        "unexpected": true
    }))
    .expect_err("unknown field rejected");
    assert!(err.to_string().contains("unknown field"));
}
