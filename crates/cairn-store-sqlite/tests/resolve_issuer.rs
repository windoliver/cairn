//! Integration test: [`cairn_core::verifier::resolve_issuer`] against a
//! real [`cairn_store_sqlite::SqliteIdentityRegistry`].

use chrono::Utc;
use rand_core::OsRng;

use cairn_core::contract::identity_registry::IdentityRegistry;
use cairn_core::domain::DomainError;
use cairn_core::domain::Identity;
use cairn_core::domain::identity::IdentityKind;
use cairn_core::domain::identity::keys::{
    IdentityRevision, KeyVersion, SigningKey, VaultId, WitnessHash,
};
use cairn_core::domain::identity::receipts::{
    ReceiptOpKind, ReceiptPayload, RevocationReceipt, RotationReceipt,
};
use cairn_core::domain::identity::records::{
    IdentityKeyEntry, ProvisioningState, PublicIdentityRecord, ReceiptId,
};
use cairn_core::verifier::resolve_issuer;
use cairn_store_sqlite::SqliteIdentityRegistry;

/// Build a fresh in-memory registry and bind a single identity at
/// `KeyVersion::FIRST` in the requested `state`.
///
/// Performs the first-bind dance (`vault_meta` row + binding witness) so
/// the identity row exists, then drives the lifecycle to `state` via
/// `activate_identity` (and, for `Revoked`, `begin_revocation` +
/// `finalise_revocation` with a self-signed receipt).
async fn registry_with_identity(
    state: ProvisioningState,
    key: &SigningKey,
) -> (SqliteIdentityRegistry, Identity, tempfile::TempDir) {
    let reg = SqliteIdentityRegistry::open_in_memory().expect("open in-memory");
    let dir = tempfile::tempdir().expect("tempdir");
    let binding = dir.path().join("vault.binding.pending");
    let witness = vec![1u8; 32];
    std::fs::write(&binding, &witness).expect("write witness file");
    let hash = WitnessHash::from_witness(&witness);
    let vault = VaultId::mint();

    let id = Identity::parse("hmn:tafeng").unwrap();
    // The first-bind always lands the identity as Pending; activated_at /
    // revoked_at on the supplied record are advisory metadata.
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

    match state {
        ProvisioningState::Pending => {
            // Leave as Pending; nothing else to do.
        }
        ProvisioningState::Active => {
            reg.activate_identity(&id, KeyVersion::FIRST)
                .await
                .expect("activate_identity");
        }
        ProvisioningState::Revoked => {
            // Activate first, then drive a self-signed two-phase revocation.
            reg.activate_identity(&id, KeyVersion::FIRST)
                .await
                .expect("activate_identity");
            let payload = ReceiptPayload {
                op_kind: ReceiptOpKind::Revocation,
                target: id.clone(),
                signer: id.clone(),
                signer_key_version: KeyVersion::FIRST,
                old_key_version: Some(KeyVersion::FIRST),
                new_key_version: None,
                issued_at: Utc::now(),
            };
            let sig = payload.sign(key).expect("sign revocation payload");
            let receipt = RevocationReceipt {
                id: ReceiptId(0),
                payload,
                signature: sig.to_bytes().to_vec(),
                pending_key_disable: true,
            };
            reg.begin_revocation(&receipt)
                .await
                .expect("begin_revocation");
            reg.finalise_revocation(&id)
                .await
                .expect("finalise_revocation");
        }
        ProvisioningState::RevokePending
        | ProvisioningState::PurgePending
        | ProvisioningState::Purged => {
            panic!("unsupported state for this fixture: {state:?}");
        }
    }

    (reg, id, dir)
}

#[tokio::test]
async fn resolves_active_issuer() {
    let key = SigningKey::generate(&mut OsRng);
    let (reg, id, _dir) = registry_with_identity(ProvisioningState::Active, &key).await;
    let resolved = resolve_issuer(&reg, &id, KeyVersion::FIRST).await.unwrap();
    assert_eq!(resolved.identity(), &id);
    assert_eq!(resolved.key_version(), KeyVersion::FIRST);
    assert!(matches!(resolved.state(), ProvisioningState::Active));
    assert_eq!(
        resolved.verifying_key().to_bytes(),
        key.verifying_key().to_bytes()
    );
}

#[tokio::test]
async fn unknown_issuer_returns_unauthorized() {
    let reg = SqliteIdentityRegistry::open_in_memory().expect("open in-memory");
    let id = Identity::parse("hmn:nobody").unwrap();
    let err = resolve_issuer(&reg, &id, KeyVersion::FIRST)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Unauthorized { .. }));
}

#[tokio::test]
async fn unknown_key_version_returns_mismatch() {
    let key = SigningKey::generate(&mut OsRng);
    let (reg, id, _dir) = registry_with_identity(ProvisioningState::Active, &key).await;
    let v2 = KeyVersion::new(std::num::NonZeroU32::new(2).unwrap());
    let err = resolve_issuer(&reg, &id, v2).await.unwrap_err();
    let DomainError::KeyVersionMismatch {
        intent, current, ..
    } = err
    else {
        panic!("expected KeyVersionMismatch, got {err:?}");
    };
    assert_eq!(intent, v2);
    assert_eq!(current, Some(KeyVersion::FIRST));
}

#[tokio::test]
async fn superseded_key_after_rotation_is_rejected() {
    // After rotation v1 → v2, the v1 key row remains in `identity_keys`
    // for audit but with `superseded_at = Some(_)`. resolve_issuer must
    // reject any envelope claiming to be signed under v1, even though
    // the row still exists and the public key still decodes — otherwise
    // an attacker who captured the rotated-away private key could keep
    // forging envelopes indefinitely.
    let v1_key = SigningKey::generate(&mut OsRng);
    let v2_key = SigningKey::generate(&mut OsRng);
    let (reg, id, _dir) = registry_with_identity(ProvisioningState::Active, &v1_key).await;

    let v2 = KeyVersion::FIRST.next().expect("KeyVersion::next");
    let payload = ReceiptPayload {
        op_kind: ReceiptOpKind::Rotation,
        target: id.clone(),
        signer: id.clone(),
        signer_key_version: KeyVersion::FIRST,
        old_key_version: Some(KeyVersion::FIRST),
        new_key_version: Some(v2),
        issued_at: Utc::now(),
    };
    let sig = payload.sign(&v1_key).expect("sign rotation payload");
    let receipt = RotationReceipt {
        id: ReceiptId(0),
        payload,
        signature: sig.to_bytes().to_vec(),
        pending_eviction: false,
    };
    let new_key_entry = IdentityKeyEntry {
        identity_id: id.clone(),
        key_version: v2,
        public_key: v2_key.verifying_key().to_bytes(),
        signed_predecessor: None,
        created_at: Utc::now(),
        superseded_at: None,
    };
    reg.insert_pending_rotation(&id, v2, "kc:test:v2")
        .await
        .expect("insert_pending_rotation");
    reg.apply_rotation(&receipt, KeyVersion::FIRST, &new_key_entry)
        .await
        .expect("apply_rotation");

    // Resolving v1 must now fail — old key is superseded, current is v2.
    let err = resolve_issuer(&reg, &id, KeyVersion::FIRST)
        .await
        .unwrap_err();
    let DomainError::KeyVersionMismatch {
        intent, current, ..
    } = err
    else {
        panic!("expected KeyVersionMismatch for superseded key, got {err:?}");
    };
    assert_eq!(intent, KeyVersion::FIRST);
    assert_eq!(current, Some(v2));

    // Resolving v2 succeeds.
    let resolved = resolve_issuer(&reg, &id, v2).await.expect("resolve v2");
    assert_eq!(resolved.key_version(), v2);
    assert_eq!(
        resolved.verifying_key().to_bytes(),
        v2_key.verifying_key().to_bytes()
    );
}

#[tokio::test]
async fn revoked_issuer_is_resolved_state_revoked() {
    let key = SigningKey::generate(&mut OsRng);
    let (reg, id, _dir) = registry_with_identity(ProvisioningState::Revoked, &key).await;
    // resolve_issuer succeeds; the verifier is what rejects on state.
    let resolved = resolve_issuer(&reg, &id, KeyVersion::FIRST).await.unwrap();
    assert!(matches!(resolved.state(), ProvisioningState::Revoked));
}
