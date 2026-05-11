#![allow(missing_docs)]

use cairn_sensors_local::policy::{PolicyAction, sanitize_text_payload};

#[test]
fn redacts_common_secret_assignments() {
    let action = sanitize_text_payload("run API_KEY=sk-test TOKEN=abc ok");

    assert_eq!(
        action,
        PolicyAction::Sanitized("run API_KEY=[REDACTED] TOKEN=[REDACTED] ok".to_owned())
    );
}

#[test]
fn redacts_authorization_bearer_header() {
    let action = sanitize_text_payload("Authorization: Bearer abc.def-123");

    assert_eq!(
        action,
        PolicyAction::Sanitized("Authorization: Bearer [REDACTED]".to_owned())
    );
}

#[test]
fn rejects_private_key_blocks() {
    let action =
        sanitize_text_payload("-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----");

    assert_eq!(
        action,
        PolicyAction::Rejected("private key block".to_owned())
    );
}
