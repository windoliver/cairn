//! Signed share-link shape and signature tests.

use cairn_core::domain::identity::keys::SigningKey;
use cairn_core::domain::sharing::{ShareLinkPayload, SharingRevocationState, SignedShareLink};
use cairn_core::domain::{
    self, Ed25519Signature, Identity, MemoryVisibility, Rfc3339Timestamp, ScopeTuple,
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

fn assert_link_shape_err(
    name: &str,
    mutate: impl FnOnce(&mut SignedShareLink),
    expected: fn(domain::DomainError) -> bool,
) {
    let mut link = signed_link();
    mutate(&mut link);
    let err = link
        .validate_shape()
        .expect_err(&format!("{name} should fail shape validation"));
    assert!(expected(err), "{name} returned unexpected error");
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
fn share_link_rejects_rewrapped_link_id() {
    assert_link_shape_err(
        "rewrapped link_id",
        |link| link.link_id = "share-01HQZX9F5N0000000000000009".to_owned(),
        |err| matches!(err, domain::DomainError::MalformedScope { .. }),
    );
}

#[test]
fn share_link_shape_rejects_too_many_and_duplicate_target_id_hashes() {
    assert_link_shape_err(
        "too many target_id_hashes",
        |link| {
            link.payload.target_id_hashes = (0..65).map(|idx| format!("hash:{idx:032x}")).collect();
        },
        |err| matches!(err, domain::DomainError::InvalidPayloadHash { .. }),
    );

    assert_link_shape_err(
        "duplicate target_id_hashes",
        |link| {
            link.payload.target_id_hashes = vec![
                format!("hash:{}", "d".repeat(32)),
                format!("hash:{}", "d".repeat(32)),
            ];
        },
        |err| matches!(err, domain::DomainError::InvalidPayloadHash { .. }),
    );
}

#[test]
fn share_link_shape_rejects_required_bad_fields() {
    let cases: &[(
        &str,
        fn(&mut SignedShareLink),
        fn(domain::DomainError) -> bool,
    )] = &[
        (
            "malformed link_id",
            |l| l.link_id = "bad id".to_owned(),
            |e| matches!(e, domain::DomainError::MalformedScope { .. }),
        ),
        (
            "malformed operation_id",
            |l| {
                l.payload.operation_id = "not-a-ulid".to_owned();
                l.link_id = "share-not-a-ulid".to_owned();
            },
            |e| matches!(e, domain::DomainError::MalformedScope { .. }),
        ),
        (
            "malformed nonce",
            |l| l.payload.nonce = "not-base64".to_owned(),
            |e| matches!(e, domain::DomainError::MissingSignature { .. }),
        ),
        (
            "bad target_hash",
            |l| {
                l.payload.target_hash = format!("sha256:{}", "C".repeat(64));
            },
            |e| matches!(e, domain::DomainError::InvalidPayloadHash { .. }),
        ),
        (
            "invalid tier",
            |l| l.payload.grant_tier = MemoryVisibility::Private,
            |e| matches!(e, domain::DomainError::UnsupportedVisibility { .. }),
        ),
        (
            "non-human issuer",
            |l| {
                l.payload.issuer =
                    Identity::parse("agt:cairn-cli:default:reader:v1").expect("agent");
            },
            |e| matches!(e, domain::DomainError::Unauthorized { .. }),
        ),
        (
            "zero key_version",
            |l| l.payload.key_version = 0,
            |e| matches!(e, domain::DomainError::Unauthorized { .. }),
        ),
        (
            "expires at issued_at",
            |l| l.payload.expires_at = l.payload.issued_at.clone(),
            |e| matches!(e, domain::DomainError::ExpiredIntent { .. }),
        ),
    ];

    for (name, mutate, expected) in cases {
        assert_link_shape_err(name, *mutate, *expected);
    }
}

#[test]
fn share_link_denies_unknown_wrapper_and_payload_fields() {
    let wrapper_err = serde_json::from_value::<SignedShareLink>(serde_json::json!({
        "link_id": "share-01HQZX9F5N0000000000000004",
        "payload": link_payload(),
        "signature": signature_for(&link_payload()),
        "unexpected": true
    }))
    .expect_err("wrapper unknown field rejected");
    assert!(wrapper_err.to_string().contains("unknown field"));

    let payload_err = serde_json::from_value::<ShareLinkPayload>(serde_json::json!({
        "operation_id": "01HQZX9F5N0000000000000004",
        "nonce": "AAAAAAAAAAAAAAAAAAAAAA==",
        "target_hash": format!("sha256:{}", "c".repeat(64)),
        "target_id_hashes": [format!("hash:{}", "d".repeat(32))],
        "scope": scope(),
        "grant_tier": "team",
        "grantee": "agt:cairn-cli:default:reader:v1",
        "issuer": "hmn:tafeng",
        "issued_at": "2026-05-21T12:00:00Z",
        "expires_at": "2026-05-22T12:00:00Z",
        "key_version": 1,
        "unexpected": true
    }))
    .expect_err("payload unknown field rejected");
    assert!(payload_err.to_string().contains("unknown field"));
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
