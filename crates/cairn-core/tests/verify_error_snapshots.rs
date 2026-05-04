//! Snapshot tests for `VerifyError::Display` — locks the wire-stable
//! error wording demanded by issue #51 acceptance criterion 3.

use cairn_core::contract::issuer_key_resolver::ResolverError;
use cairn_core::domain::identity::keys::KeyVersion;
use cairn_core::domain::{Identity, IdentityKind};
use cairn_core::generated::envelope::SignedIntentScopeTier;
use cairn_core::intent::{ExpiryReason, VerifyError};

#[test]
fn malformed_snapshot() {
    let e = VerifyError::Malformed {
        field: "issuer",
        reason: "bad prefix".to_owned(),
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn expired_skewed_snapshot() {
    let e = VerifyError::ExpiredIntent {
        issued_at: "2026-04-22T14:02:11Z".to_owned(),
        expires_at: "2026-04-22T14:07:11Z".to_owned(),
        now: "2026-04-22T14:30:00Z".to_owned(),
        kind: ExpiryReason::Skewed,
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn expired_past_snapshot() {
    let e = VerifyError::ExpiredIntent {
        issued_at: "2026-04-22T14:02:11Z".to_owned(),
        expires_at: "2026-04-22T14:03:11Z".to_owned(),
        now: "2026-04-22T14:30:00Z".to_owned(),
        kind: ExpiryReason::Past,
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn expired_ttl_exceeded_snapshot() {
    let e = VerifyError::ExpiredIntent {
        issued_at: "2026-04-22T14:02:11Z".to_owned(),
        expires_at: "2026-04-22T14:30:11Z".to_owned(),
        now: "2026-04-22T14:02:30Z".to_owned(),
        kind: ExpiryReason::TtlExceeded,
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn scope_denied_snapshot() {
    let e = VerifyError::ScopeDenied {
        issuer_kind: IdentityKind::Agent,
        requested_tier: SignedIntentScopeTier::Team,
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn unknown_key_snapshot() {
    let e = VerifyError::UnknownKey {
        issuer: Identity::parse("hmn:tafeng").expect("parse"),
        key_version: KeyVersion::FIRST,
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn revoked_key_snapshot() {
    let e = VerifyError::RevokedKey {
        issuer: Identity::parse("hmn:tafeng").expect("parse"),
        key_version: KeyVersion::FIRST,
        effective_at: "2026-04-22T14:00:00Z".to_owned(),
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn invalid_signature_snapshot() {
    let e = VerifyError::InvalidSignature;
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn resolver_failure_snapshot() {
    let inner: Box<dyn std::error::Error + Send + Sync> = "boom".into();
    let e = VerifyError::ResolverFailure(ResolverError::Backend(inner));
    insta::assert_snapshot!(e.to_string());
}
