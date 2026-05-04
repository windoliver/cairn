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
fn rejects_invalid_wire_shape_both_sequence_and_challenge(
    signing_key: SigningKey,
    policy: ScopePolicy,
    clock: FixedClock,
) {
    // In-memory caller bypasses serde::Deserialize (which would catch this
    // via RawSignedIntent::try_from). Verifier must independently reject
    // intents that violate the IDL XOR group.
    let mut intent = unsigned_intent();
    intent.server_challenge = Some(common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".into()));
    let intent = sign(&signing_key, intent); // sequence Some + server_challenge Some
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::InvalidSignature));
}

#[rstest]
fn rejects_invalid_wire_shape_neither_sequence_nor_challenge(
    signing_key: SigningKey,
    policy: ScopePolicy,
    clock: FixedClock,
) {
    let mut intent = unsigned_intent();
    intent.sequence = None;
    intent.server_challenge = None;
    let intent = sign(&signing_key, intent);
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::InvalidSignature));
}

#[rstest]
fn rejects_invalid_wire_shape_oversized_chain_parents(
    signing_key: SigningKey,
    policy: ScopePolicy,
    clock: FixedClock,
) {
    // Crockford ULID alphabet (no I, L, O, U). 32 chars → 65 distinct
    // 26-char ULIDs by varying char index 24 (low-order). All start with
    // '0' so the 128-bit cap holds.
    const ULID_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    // RawSignedIntent::try_from caps chain_parents at 64 entries.
    // In-memory caller bypasses serde and could push thousands.
    let mut intent = unsigned_intent();
    intent.chain_parents = (0..65)
        .map(|i| {
            let a = char::from(ULID_ALPHABET[i / 32]);
            let b = char::from(ULID_ALPHABET[i % 32]);
            common::Ulid(format!("01HQZX9F5N000000000000000{a}{b}"))
        })
        .collect();
    // Need exactly 26-char ULIDs. Truncate the format above to 26.
    intent.chain_parents = intent
        .chain_parents
        .into_iter()
        .map(|u| common::Ulid(u.0.chars().take(26).collect()))
        .collect();
    let intent = sign(&signing_key, intent);
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::InvalidSignature));
}

#[rstest]
fn rejects_invalid_wire_shape_malformed_target_hash(
    signing_key: SigningKey,
    policy: ScopePolicy,
    clock: FixedClock,
) {
    let mut intent = unsigned_intent();
    intent.target_hash = "not-a-sha256".into();
    let intent = sign(&signing_key, intent);
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::InvalidSignature));
}

#[rstest]
fn rejects_invalid_wire_shape_empty_scope_entity(
    signing_key: SigningKey,
    policy: ScopePolicy,
    clock: FixedClock,
) {
    let mut intent = unsigned_intent();
    intent.scope.entity = String::new();
    let intent = sign(&signing_key, intent);
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::InvalidSignature));
}

#[rstest]
fn rejects_invalid_wire_shape_sequence_above_safe_integer(
    signing_key: SigningKey,
    policy: ScopePolicy,
    clock: FixedClock,
) {
    // sequence > 2^53 - 1 is rejected by RawSignedIntent::try_from to
    // prevent JSON-decoder disagreement on the represented value.
    let mut intent = unsigned_intent();
    intent.sequence = Some(9_007_199_254_740_992_u64);
    let intent = sign(&signing_key, intent);
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::InvalidSignature));
}

#[rstest]
fn signature_check_runs_before_scope(signing_key: SigningKey, clock: FixedClock) {
    // Brief §14: an unauthenticated caller with the wrong tenant must not
    // observe scope-policy detail through the error envelope. After the
    // authn-first reordering, a tampered signature with mismatched tenant
    // surfaces InvalidSignature, never ScopeDenied.
    let policy = ScopePolicy::new("other-tenant", "ws", ScopePolicy::all_tiers()).unwrap();
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
    assert!(
        matches!(err, DomainError::InvalidSignature),
        "expected InvalidSignature (authn-first), got {err:?}"
    );
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
