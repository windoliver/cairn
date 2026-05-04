//! Lifecycle-mapping integration test for [`SqliteIssuerKeyResolver`]
//! (issue #51).
//!
//! Exercises the real `SqliteIdentityRegistry` backend — no mocking. Covers
//! the three lifecycle states reachable through public APIs:
//! - `Active` → `KeyLifecycle::Active`
//! - `Pending` → `KeyLifecycle::NonOperational`
//! - `Revoked` → `KeyLifecycle::Revoked { effective_at }`

use std::sync::Arc;

use cairn_core::contract::IdentityRegistry;
use cairn_core::contract::identity_registry::IdentityVisibility;
use cairn_core::contract::issuer_key_resolver::{IssuerKeyResolver, KeyLifecycle};
use cairn_core::domain::identity::{
    Identity, IdentityKind,
    keys::{IdentityRevision, KeyVersion, SigningKey, VaultId, WitnessHash},
    receipts::{ReceiptOpKind, ReceiptPayload, RevocationReceipt},
    records::{IdentityKeyEntry, ProvisioningState, PublicIdentityRecord, ReceiptId},
};
use cairn_store_sqlite::{SqliteIdentityRegistry, SqliteIssuerKeyResolver};

/// Open an in-memory registry, perform first-bind, and activate the
/// first-bound `hmn:first` so subsequent identities can be reserved.
async fn first_bound_registry() -> (SqliteIdentityRegistry, tempfile::TempDir) {
    let r = SqliteIdentityRegistry::open_in_memory().expect("open in-memory registry");
    let dir = tempfile::tempdir().expect("tempdir");
    let binding = dir.path().join("vault.binding.pending");
    let witness = vec![1_u8; 32];
    std::fs::write(&binding, &witness).expect("write witness");
    let hash = WitnessHash::from_witness(&witness);

    let vault = VaultId::mint();
    let first = Identity::parse("hmn:first").expect("identity");
    let now = chrono::Utc::now();
    let record = PublicIdentityRecord {
        id: first.clone(),
        kind: IdentityKind::Human,
        current_key_version: KeyVersion::FIRST,
        revision: IdentityRevision::FIRST,
        provisioning_state: ProvisioningState::Pending,
        created_at: now,
        activated_at: None,
        revoked_at: None,
        purge_requested_at: None,
        purged_at: None,
    };
    let mut rng = rand_core::OsRng;
    let sk = SigningKey::generate(&mut rng);
    let key = IdentityKeyEntry {
        identity_id: first.clone(),
        key_version: KeyVersion::FIRST,
        public_key: sk.verifying_key().to_bytes(),
        signed_predecessor: None,
        created_at: now,
        superseded_at: None,
    };
    r.reserve_first_identity(&vault, &record, &key, hash, &binding)
        .await
        .expect("reserve_first_identity");
    r.activate_identity(&first, KeyVersion::FIRST)
        .await
        .expect("activate hmn:first");

    (r, dir)
}

/// Reserve + activate `id` on `r` with a fresh keypair. Returns the public
/// key bytes so tests can compare against `ResolvedKey.public_key`.
async fn seed_active(r: &SqliteIdentityRegistry, id: &Identity) -> ([u8; 32], SigningKey) {
    let mut rng = rand_core::OsRng;
    let sk = SigningKey::generate(&mut rng);
    let pubkey = sk.verifying_key().to_bytes();
    let now = chrono::Utc::now();
    let record = PublicIdentityRecord {
        id: id.clone(),
        kind: IdentityKind::Human,
        current_key_version: KeyVersion::FIRST,
        revision: IdentityRevision::FIRST,
        provisioning_state: ProvisioningState::Pending,
        created_at: now,
        activated_at: None,
        revoked_at: None,
        purge_requested_at: None,
        purged_at: None,
    };
    let key = IdentityKeyEntry {
        identity_id: id.clone(),
        key_version: KeyVersion::FIRST,
        public_key: pubkey,
        signed_predecessor: None,
        created_at: now,
        superseded_at: None,
    };
    r.reserve_identity(&record, &key)
        .await
        .expect("reserve_identity");
    r.activate_identity(id, KeyVersion::FIRST)
        .await
        .expect("activate_identity");
    (pubkey, sk)
}

fn make_revocation_receipt(target: &Identity, sk: &SigningKey) -> RevocationReceipt {
    let payload = ReceiptPayload {
        op_kind: ReceiptOpKind::Revocation,
        target: target.clone(),
        signer: target.clone(),
        signer_key_version: KeyVersion::FIRST,
        old_key_version: Some(KeyVersion::FIRST),
        new_key_version: None,
        issued_at: chrono::Utc::now(),
    };
    let sig = payload.sign(sk).expect("sign payload");
    RevocationReceipt {
        id: ReceiptId(0),
        payload,
        signature: sig.to_bytes().to_vec(),
        pending_key_disable: true,
    }
}

#[tokio::test]
async fn unknown_issuer_returns_none() {
    let (r, _dir) = first_bound_registry().await;
    let resolver = SqliteIssuerKeyResolver::new(Arc::new(r));
    let id = Identity::parse("hmn:nobody").expect("parse");
    let out = resolver
        .lookup(&id, KeyVersion::FIRST)
        .await
        .expect("lookup ok");
    assert!(out.is_none());
}

#[tokio::test]
async fn active_identity_resolves_to_active_lifecycle() {
    let (r, _dir) = first_bound_registry().await;
    let alice = Identity::parse("hmn:alice").expect("parse");
    let (expected_pubkey, _sk) = seed_active(&r, &alice).await;

    let resolver = SqliteIssuerKeyResolver::new(Arc::new(r));
    let entry = resolver
        .lookup(&alice, KeyVersion::FIRST)
        .await
        .expect("ok")
        .expect("present");
    assert_eq!(entry.public_key, expected_pubkey);
    assert!(matches!(entry.lifecycle, KeyLifecycle::Active));
}

#[tokio::test]
async fn pending_identity_resolves_to_non_operational() {
    let (r, _dir) = first_bound_registry().await;
    // Reserve but do NOT activate — leaves the row in `Pending`.
    let bob = Identity::parse("hmn:bob").expect("parse");
    let mut rng = rand_core::OsRng;
    let sk = SigningKey::generate(&mut rng);
    let now = chrono::Utc::now();
    let record = PublicIdentityRecord {
        id: bob.clone(),
        kind: IdentityKind::Human,
        current_key_version: KeyVersion::FIRST,
        revision: IdentityRevision::FIRST,
        provisioning_state: ProvisioningState::Pending,
        created_at: now,
        activated_at: None,
        revoked_at: None,
        purge_requested_at: None,
        purged_at: None,
    };
    let key = IdentityKeyEntry {
        identity_id: bob.clone(),
        key_version: KeyVersion::FIRST,
        public_key: sk.verifying_key().to_bytes(),
        signed_predecessor: None,
        created_at: now,
        superseded_at: None,
    };
    r.reserve_identity(&record, &key)
        .await
        .expect("reserve_identity");

    let resolver = SqliteIssuerKeyResolver::new(Arc::new(r));
    let entry = resolver
        .lookup(&bob, KeyVersion::FIRST)
        .await
        .expect("ok")
        .expect("present");
    assert!(matches!(entry.lifecycle, KeyLifecycle::NonOperational));
}

#[tokio::test]
async fn revoked_identity_resolves_to_revoked_with_effective_at() {
    let (r, _dir) = first_bound_registry().await;
    let carol = Identity::parse("hmn:carol").expect("parse");
    let (_, sk) = seed_active(&r, &carol).await;

    let receipt = make_revocation_receipt(&carol, &sk);
    r.begin_revocation(&receipt)
        .await
        .expect("begin_revocation");
    r.finalise_revocation(&carol)
        .await
        .expect("finalise_revocation");

    // Snapshot revoked_at before moving the registry into the resolver.
    let rec = r
        .get_identity(&carol, IdentityVisibility::Audit)
        .await
        .expect("ok")
        .expect("present");
    assert_eq!(rec.provisioning_state, ProvisioningState::Revoked);
    let revoked_at = rec.revoked_at.expect("revoked_at set").to_rfc3339();

    let resolver = SqliteIssuerKeyResolver::new(Arc::new(r));
    let entry = resolver
        .lookup(&carol, KeyVersion::FIRST)
        .await
        .expect("ok")
        .expect("present");
    match entry.lifecycle {
        KeyLifecycle::Revoked { effective_at } => {
            assert_eq!(effective_at.as_str(), revoked_at);
        }
        other => panic!("expected Revoked, got {other:?}"),
    }
}
