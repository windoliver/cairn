//! T20f — OAuth token round-trip + webhook signature lifecycle.
//!
//! Three test functions:
//!   1. `missing_credential_returns_auth_expired` — `InMemoryCredentialStore::get`
//!      on an absent scope returns `ConnectorError::AuthExpired`.
//!   2. `credential_round_trip_then_delete` — put, get, delete, get again
//!      confirms deletion.
//!   3. `signature_round_trip` — `hex_hmac_sha256` + `verify_hmac_sha256` passes
//!      on the correct body and fails (`SignatureMismatch`) on a tampered body.
//!
//! Issue #130, brief §9.1 OAuth + webhook contracts.

#![allow(missing_docs)]

use cairn_connectors_core::webhook::{WebhookRequest, hex_hmac_sha256, verify_hmac_sha256};
use cairn_connectors_core::{ConnectorError, CredentialStore, InMemoryCredentialStore};

// ---------------------------------------------------------------------------
// Credential store lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_credential_returns_auth_expired() {
    let store = InMemoryCredentialStore::default();
    match store.get("absent").await {
        Err(ConnectorError::AuthExpired { scope }) => {
            assert_eq!(scope, "absent", "AuthExpired scope must be \"absent\"");
        }
        other => panic!("expected AuthExpired, got {other:?}"),
    }
}

#[tokio::test]
async fn credential_round_trip_then_delete() {
    let store = InMemoryCredentialStore::default();

    // Put and retrieve.
    store
        .put("s", b"v".to_vec())
        .await
        .expect("put must succeed");
    let handle = store.get("s").await.expect("get must succeed after put");
    assert_eq!(
        handle.bytes(),
        b"v",
        "retrieved bytes must match stored bytes"
    );

    // Delete then confirm absence.
    store.delete("s").await.expect("delete must succeed");
    assert!(store.get("s").await.is_err(), "get must fail after delete");
}

// ---------------------------------------------------------------------------
// Webhook signature lifecycle
// ---------------------------------------------------------------------------

#[test]
fn signature_round_trip() {
    let secret = b"k";
    let body = b"payload";

    // Produce a valid hex-encoded HMAC-SHA256 signature.
    let sig = hex_hmac_sha256(secret, body);

    // Verification must succeed when body and secret match.
    let req_ok = WebhookRequest {
        connector: "f".into(),
        body: body.to_vec(),
        headers: vec![("X-Sig".into(), sig.clone())],
    };
    verify_hmac_sha256(&req_ok, "X-Sig", secret)
        .expect("verify_hmac_sha256 must succeed for correct body + secret");

    // Verification must fail when the body has been tampered with.
    let req_bad = WebhookRequest {
        connector: "f".into(),
        body: b"tampered".to_vec(),
        headers: vec![("X-Sig".into(), sig)],
    };
    match verify_hmac_sha256(&req_bad, "X-Sig", secret) {
        Err(ConnectorError::SignatureMismatch) => { /* expected */ }
        other => panic!("expected SignatureMismatch, got {other:?}"),
    }
}
