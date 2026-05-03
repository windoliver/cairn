//! Unit tests for [`super::EnvelopeVerifier`].
//!
//! All cases use a single deterministic seed signing key and build an
//! intent inside a fixed `(issued_at, expires_at)` window. Each test
//! mutates exactly one input dimension and asserts the matching error.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey};
use rstest::{fixture, rstest};

use crate::domain::DomainError;
use crate::domain::Identity;
use crate::domain::canonical::canonical_bytes_signed_intent;
use crate::domain::identity::keys::KeyVersion;
use crate::domain::identity::records::ProvisioningState;
use crate::domain::time::FixedClock;
use crate::generated::common;
use crate::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};

use super::{EnvelopeVerifier, ResolvedIssuer, ScopePolicy};

const ISSUER_WIRE: &str = "hmn:tafeng";
const ISSUED_AT: &str = "2026-04-22T14:02:11Z";
const EXPIRES_AT: &str = "2026-04-22T14:07:11Z";
const NOW: &str = "2026-04-22T14:05:00Z";

#[fixture]
fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

#[fixture]
fn policy() -> ScopePolicy {
    ScopePolicy::new("acme", "ws", ScopePolicy::all_tiers()).unwrap()
}

#[fixture]
fn clock() -> FixedClock {
    let t: DateTime<Utc> = NOW.parse().unwrap();
    FixedClock(t)
}

fn unsigned_intent() -> SignedIntent {
    SignedIntent {
        chain_parents: vec![],
        expires_at: EXPIRES_AT.into(),
        issued_at: ISSUED_AT.into(),
        issuer: common::Identity(ISSUER_WIRE.into()),
        key_version: 1,
        nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".into()),
        operation_id: common::Ulid("01HQZX9F5N0000000000000000".into()),
        scope: SignedIntentScope {
            tenant: "acme".into(),
            workspace: "ws".into(),
            entity: "ent".into(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence: Some(1),
        server_challenge: None,
        signature: common::Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
        target_hash: format!("sha256:{}", "a".repeat(64)),
    }
}

fn sign(key: &SigningKey, mut intent: SignedIntent) -> SignedIntent {
    let bytes = canonical_bytes_signed_intent(&intent).unwrap();
    let sig = key.sign(&bytes);
    let hex = sig
        .to_bytes()
        .iter()
        .fold(String::with_capacity(128), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(&mut s, "{b:02x}");
            s
        });
    intent.signature = common::Ed25519Signature(format!("ed25519:{hex}"));
    intent
}

fn resolved_active(key: &SigningKey) -> ResolvedIssuer {
    ResolvedIssuer::for_test(
        Identity::parse(ISSUER_WIRE).unwrap(),
        KeyVersion::FIRST,
        key.verifying_key(),
        ProvisioningState::Active,
    )
}

#[rstest]
fn accepts_valid(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let verified = verifier.verify(intent, &resolved).expect("valid envelope");
    assert_eq!(verified.as_inner().issuer.0, ISSUER_WIRE);
}

#[rstest]
fn rejects_tampered_signature(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let mut intent = sign(&signing_key, unsigned_intent());
    let hex_tail = intent
        .signature
        .0
        .strip_prefix("ed25519:")
        .unwrap()
        .to_owned();
    let mut chars: Vec<char> = hex_tail.chars().collect();
    let last = chars.last_mut().unwrap();
    *last = if *last == 'a' { 'b' } else { 'a' };
    let mutated: String = chars.into_iter().collect();
    intent.signature = common::Ed25519Signature(format!("ed25519:{mutated}"));
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::InvalidSignature));
}

#[rstest]
fn rejects_tampered_payload(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let mut intent = sign(&signing_key, unsigned_intent());
    intent.scope.entity = "different".into();
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::InvalidSignature));
}

#[rstest]
fn rejects_expired(signing_key: SigningKey, policy: ScopePolicy) {
    let after_expiry: DateTime<Utc> = "2026-04-22T15:00:00Z".parse().unwrap();
    let clock = FixedClock(after_expiry);
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::ExpiredIntent { .. }));
}

#[rstest]
fn rejects_pre_issued(signing_key: SigningKey, policy: ScopePolicy) {
    let before_issue: DateTime<Utc> = "2026-04-22T14:00:00Z".parse().unwrap();
    let clock = FixedClock(before_issue);
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::ExpiredIntent { .. }));
}

#[rstest]
fn rejects_revoked(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = ResolvedIssuer::for_test(
        Identity::parse(ISSUER_WIRE).unwrap(),
        KeyVersion::FIRST,
        signing_key.verifying_key(),
        ProvisioningState::Revoked,
    );
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(
        err,
        DomainError::RevokedKey {
            state: ProvisioningState::Revoked,
            ..
        }
    ));
}

#[rstest]
fn rejects_pending(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = ResolvedIssuer::for_test(
        Identity::parse(ISSUER_WIRE).unwrap(),
        KeyVersion::FIRST,
        signing_key.verifying_key(),
        ProvisioningState::Pending,
    );
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(
        err,
        DomainError::RevokedKey {
            state: ProvisioningState::Pending,
            ..
        }
    ));
}

#[rstest]
fn rejects_scope_tenant(signing_key: SigningKey, clock: FixedClock) {
    let policy = ScopePolicy::new("other-tenant", "ws", ScopePolicy::all_tiers()).unwrap();
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    let DomainError::ScopeDenied { message } = err else {
        panic!("expected ScopeDenied, got {err:?}");
    };
    assert!(message.starts_with("tenant"));
}

#[rstest]
fn rejects_scope_workspace(signing_key: SigningKey, clock: FixedClock) {
    let policy = ScopePolicy::new("acme", "other-ws", ScopePolicy::all_tiers()).unwrap();
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    let DomainError::ScopeDenied { message } = err else {
        panic!("expected ScopeDenied, got {err:?}");
    };
    assert!(message.starts_with("workspace"));
}

#[rstest]
fn rejects_scope_tier(signing_key: SigningKey, clock: FixedClock) {
    let mut allowed = HashSet::new();
    allowed.insert(SignedIntentScopeTier::Org); // Project not allowed
    let policy = ScopePolicy::new("acme", "ws", allowed).unwrap();
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    let DomainError::ScopeDenied { message } = err else {
        panic!("expected ScopeDenied, got {err:?}");
    };
    assert!(message.starts_with("tier"));
}

#[rstest]
fn rejects_issuer_mismatch(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = ResolvedIssuer::for_test(
        Identity::parse("hmn:someone-else").unwrap(),
        KeyVersion::FIRST,
        signing_key.verifying_key(),
        ProvisioningState::Active,
    );
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::Unauthorized { .. }));
}

#[rstest]
fn rejects_wrong_key_version(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let intent = sign(&signing_key, unsigned_intent()); // intent.key_version = 1
    let resolved = ResolvedIssuer::for_test(
        Identity::parse(ISSUER_WIRE).unwrap(),
        KeyVersion::new(std::num::NonZeroU32::new(2).unwrap()), // resolved version 2
        signing_key.verifying_key(),
        ProvisioningState::Active,
    );
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::Unauthorized { .. }));
}
