//! Snapshot tests for [`cairn_core::error::envelope_error_for`].
//! Locks the wire shape across CLI / MCP / SDK. Any change to the mapper's
//! output will trigger `cargo insta review`.

use cairn_core::domain::DomainError;
use cairn_core::domain::Identity;
use cairn_core::domain::identity::keys::KeyVersion;
use cairn_core::domain::identity::records::ProvisioningState;
use cairn_core::error::envelope_error_for;

fn snapshot_body(name: &str, err: &DomainError) {
    let body = envelope_error_for(err);
    insta::with_settings!({ snapshot_suffix => name }, {
        insta::assert_json_snapshot!(body);
    });
}

#[test]
fn invalid_signature() {
    snapshot_body("invalid_signature", &DomainError::InvalidSignature);
}

#[test]
fn expired_intent() {
    snapshot_body(
        "expired_intent",
        &DomainError::ExpiredIntent {
            issued_at: "2026-04-22T14:02:11Z".into(),
            expires_at: "2026-04-22T14:07:11Z".into(),
            now: "2026-04-22T15:00:00Z".into(),
        },
    );
}

#[test]
fn revoked_key() {
    snapshot_body(
        "revoked_key",
        &DomainError::RevokedKey {
            id: Identity::parse("hmn:tafeng").unwrap(),
            key_version: KeyVersion::FIRST,
            state: ProvisioningState::Revoked,
        },
    );
}

#[test]
fn key_version_mismatch() {
    snapshot_body(
        "key_version_mismatch",
        &DomainError::KeyVersionMismatch {
            id: Identity::parse("hmn:tafeng").unwrap(),
            intent: KeyVersion::FIRST,
            current: None,
        },
    );
}

#[test]
fn scope_denied() {
    snapshot_body(
        "scope_denied",
        &DomainError::ScopeDenied {
            message: "tenant: expected acme, got other".into(),
        },
    );
}

#[test]
fn unauthorized() {
    snapshot_body(
        "unauthorized",
        &DomainError::Unauthorized {
            message: "issuer mismatch".into(),
        },
    );
}

#[test]
fn fallthrough_invalid_identity() {
    snapshot_body(
        "fallthrough_invalid_identity",
        &DomainError::InvalidIdentity {
            message: "bad prefix".into(),
        },
    );
}
