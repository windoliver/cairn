//! Negative-path integration test: any envelope that fails verification
//! must leave the registry's WAL untouched. This is the load-bearing
//! acceptance criterion for issue #51.
//!
//! Note: the per-record store WAL (`records`/`wal`) tables ship with
//! issue #8, not #51 — at this point in the codebase, no record WAL
//! helper exists *to* call. The verifier-rejection assertion is therefore
//! the functional guarantee. As a defensive sanity check we assert that
//! `identity_wal` (the only mutable table reachable here) is NOT
//! incremented by a rejected verify.

use std::collections::HashSet;

use chrono::Utc;
use ed25519_dalek::SigningKey as DalekSigningKey;
use rand_core::OsRng;

use cairn_core::contract::identity_registry::IdentityRegistry;
use cairn_core::domain::DomainError;
use cairn_core::domain::Identity;
use cairn_core::domain::identity::IdentityKind;
use cairn_core::domain::identity::keys::{
    IdentityRevision, KeyVersion, SigningKey as CoreSigningKey, VaultId, WitnessHash,
};
use cairn_core::domain::identity::records::{
    IdentityKeyEntry, ProvisioningState, PublicIdentityRecord,
};
use cairn_core::generated::common;
use cairn_core::generated::envelope::SignedIntentScopeTier;
use cairn_core::verifier::{EnvelopeVerifier, ScopePolicy, resolve_issuer};
use cairn_store_sqlite::SqliteIdentityRegistry;
use cairn_test_fixtures::intent::{
    FIXTURE_ISSUER, fixed_clock_at, scope_policy_default, sign_intent, unsigned_intent,
};

const NOW: &str = "2026-04-22T14:05:00Z";

/// Bootstrap helper — provisions a single Active identity in a fresh
/// in-memory registry. Returns the registry, the issuer Identity, and
/// the tempdir holding the binding witness file (must outlive the
/// registry).
async fn registry_with_active_identity(
    key: &CoreSigningKey,
) -> (SqliteIdentityRegistry, Identity, tempfile::TempDir) {
    let reg = SqliteIdentityRegistry::open_in_memory().expect("open in-memory");
    let dir = tempfile::tempdir().expect("tempdir");
    let binding = dir.path().join("vault.binding.pending");
    let witness = vec![1u8; 32];
    std::fs::write(&binding, &witness).expect("write witness file");
    let hash = WitnessHash::from_witness(&witness);
    let vault = VaultId::mint();

    let id = Identity::parse(FIXTURE_ISSUER).expect("parse issuer");
    let record = PublicIdentityRecord {
        id: id.clone(),
        kind: IdentityKind::Human,
        current_key_version: KeyVersion::FIRST,
        revision: IdentityRevision::FIRST,
        provisioning_state: ProvisioningState::Pending,
        created_at: Utc::now(),
        activated_at: None,
        revoked_at: None,
        purge_requested_at: None,
        purged_at: None,
    };
    let key_entry = IdentityKeyEntry {
        identity_id: id.clone(),
        key_version: KeyVersion::FIRST,
        public_key: key.verifying_key().to_bytes(),
        signed_predecessor: None,
        created_at: Utc::now(),
        superseded_at: None,
    };

    reg.reserve_first_identity(&vault, &record, &key_entry, hash, &binding)
        .await
        .expect("reserve_first_identity");
    reg.activate_identity(&id, KeyVersion::FIRST)
        .await
        .expect("activate_identity");

    (reg, id, dir)
}

/// Count rows in `identity_wal`. Defensive sanity check — a verifier
/// rejection MUST NOT add to this table.
fn identity_wal_count(reg: &SqliteIdentityRegistry) -> i64 {
    let conn = reg.test_connection();
    conn.query_row("SELECT COUNT(*) FROM identity_wal", [], |row| row.get(0))
        .expect("count identity_wal rows")
}

#[tokio::test]
async fn tampered_signature_blocks_verify() {
    let key = CoreSigningKey::generate(&mut OsRng);
    let dalek = DalekSigningKey::from_bytes(&key.expose_secret_bytes());
    let (reg, id, _dir) = registry_with_active_identity(&key).await;

    let policy = scope_policy_default();
    let clock = fixed_clock_at(NOW);
    let verifier = EnvelopeVerifier::new(&policy, &clock);

    let mut intent = sign_intent(&dalek, unsigned_intent());
    // Flip the last hex char of the signature.
    let hex = intent
        .signature
        .0
        .strip_prefix("ed25519:")
        .expect("ed25519 prefix")
        .to_owned();
    let mut chars: Vec<char> = hex.chars().collect();
    let last = chars.last_mut().expect("non-empty hex tail");
    *last = if *last == 'a' { 'b' } else { 'a' };
    let mutated: String = chars.into_iter().collect();
    intent.signature = common::Ed25519Signature(format!("ed25519:{mutated}"));

    let resolved = resolve_issuer(&reg, &id, KeyVersion::FIRST)
        .await
        .expect("resolve_issuer");

    let before = identity_wal_count(&reg);
    let result = verifier.verify(intent, &resolved);
    let after = identity_wal_count(&reg);

    assert!(
        matches!(result, Err(DomainError::InvalidSignature)),
        "expected InvalidSignature, got {result:?}"
    );
    assert_eq!(
        before, after,
        "verifier rejection must not write identity_wal rows"
    );
}

#[tokio::test]
async fn expired_intent_blocks_verify() {
    let key = CoreSigningKey::generate(&mut OsRng);
    let dalek = DalekSigningKey::from_bytes(&key.expose_secret_bytes());
    let (reg, id, _dir) = registry_with_active_identity(&key).await;

    let policy = scope_policy_default();
    // FIXTURE_EXPIRES_AT is 2026-04-22T14:07:11Z; pick a clock past that.
    let clock = fixed_clock_at("2026-04-22T15:00:00Z");
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let intent = sign_intent(&dalek, unsigned_intent());
    let resolved = resolve_issuer(&reg, &id, KeyVersion::FIRST)
        .await
        .expect("resolve_issuer");

    let before = identity_wal_count(&reg);
    let result = verifier.verify(intent, &resolved);
    let after = identity_wal_count(&reg);

    assert!(
        matches!(result, Err(DomainError::ExpiredIntent { .. })),
        "expected ExpiredIntent, got {result:?}"
    );
    assert_eq!(
        before, after,
        "verifier rejection must not write identity_wal rows"
    );
}

#[tokio::test]
async fn scope_denied_blocks_verify() {
    let key = CoreSigningKey::generate(&mut OsRng);
    let dalek = DalekSigningKey::from_bytes(&key.expose_secret_bytes());
    let (reg, id, _dir) = registry_with_active_identity(&key).await;

    // Restrict allow-list to Org tier — Project intent denied.
    let mut tiers = HashSet::new();
    tiers.insert(SignedIntentScopeTier::Org);
    let policy = ScopePolicy::new("acme", "ws", tiers).expect("policy");
    let clock = fixed_clock_at(NOW);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let intent = sign_intent(&dalek, unsigned_intent());
    let resolved = resolve_issuer(&reg, &id, KeyVersion::FIRST)
        .await
        .expect("resolve_issuer");

    let before = identity_wal_count(&reg);
    let result = verifier.verify(intent, &resolved);
    let after = identity_wal_count(&reg);

    assert!(
        matches!(result, Err(DomainError::ScopeDenied { .. })),
        "expected ScopeDenied, got {result:?}"
    );
    assert_eq!(
        before, after,
        "verifier rejection must not write identity_wal rows"
    );
}

// TODO(issue #8): once the per-record `records`/`wal` tables ship,
// extend this test to assert SELECT count(*) FROM records = 0 and
// SELECT count(*) FROM wal = 0 after each rejection. At this point in
// the codebase those tables do not exist; the verifier rejection
// assertion above is the functional guarantee that no record-level WAL
// row could have been written.
