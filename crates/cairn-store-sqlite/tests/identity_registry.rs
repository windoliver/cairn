//! Integration tests for [`SqliteIdentityRegistry`] (issue #50).
//!
//! Tests in this file exercise the real `SQLite` backend — no mocking.
//! In-memory databases (C4 step 1/2) and `tempfile` on-disk databases
//! (C4 step 2) are both used as appropriate.

use cairn_core::contract::identity_registry::{IdentityRegistry, RegistryError};
use cairn_core::domain::identity::{
    Identity, IdentityKind,
    keys::{IdentityRevision, KeyVersion, VaultId, WitnessHash},
    records::{IdentityKeyEntry, ProvisioningState, PublicIdentityRecord},
};
use cairn_store_sqlite::SqliteIdentityRegistry;

// ── C4 · read_vault_meta ──────────────────────────────────────────────────────

/// An empty registry (no first-bind yet) must return `None` from
/// `read_vault_meta`.
#[tokio::test]
async fn read_vault_meta_empty() {
    let r = SqliteIdentityRegistry::open_in_memory().unwrap();
    assert!(r.read_vault_meta().await.unwrap().is_none());
}

// ── C4 · reserve_first_identity ──────────────────────────────────────────────

/// Helper: build a minimal `PublicIdentityRecord` + `IdentityKeyEntry` pair
/// for `id`.
fn make_record_and_key(id: &Identity) -> (PublicIdentityRecord, IdentityKeyEntry) {
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
        public_key: [0u8; 32],
        signed_predecessor: None,
        created_at: now,
        superseded_at: None,
    };
    (record, key)
}

/// `reserve_first_identity` must:
/// 1. Write `vault_meta` and an `identities` row.
/// 2. Return `Ok(())` a second time with identical arguments (idempotent).
/// 3. Return `FirstBindMismatch` when called with a *different* `vault_id`.
/// 4. Return `WitnessMismatch` when `binding_path` contents differ from
///    `witness_hash`.
#[tokio::test]
async fn reserve_first_identity_writes_vault_meta_and_pending_row() {
    let r = SqliteIdentityRegistry::open_in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let pending = dir.path().join("vault.binding.pending");

    // Write 32 zero-bytes as the witness payload and compute its hash.
    let witness = vec![0u8; 32];
    std::fs::write(&pending, &witness).unwrap();
    let hash = WitnessHash::from_witness(&witness);

    let vault = VaultId::mint();
    let id = Identity::parse("hmn:alice").unwrap();
    let (record, key) = make_record_and_key(&id);

    // First call must succeed.
    r.reserve_first_identity(&vault, &record, &key, hash, &pending)
        .await
        .unwrap();

    // vault_meta must now be readable and match.
    let (stored_id, _) = r.read_vault_meta().await.unwrap().unwrap();
    assert_eq!(stored_id, vault);

    // Idempotent resume: second call with identical args must return Ok.
    r.reserve_first_identity(&vault, &record, &key, hash, &pending)
        .await
        .unwrap();

    // Different vault_id → FirstBindMismatch.
    let other = VaultId::mint();
    let err = r
        .reserve_first_identity(&other, &record, &key, hash, &pending)
        .await
        .unwrap_err();
    assert!(
        matches!(err, RegistryError::FirstBindMismatch { .. }),
        "expected FirstBindMismatch, got {err:?}"
    );
}

/// `reserve_first_identity` must return `WitnessMismatch` when the bytes at
/// `binding_path` do not hash to `witness_hash`.
#[tokio::test]
async fn reserve_first_identity_rejects_witness_mismatch() {
    let r = SqliteIdentityRegistry::open_in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let pending = dir.path().join("vault.binding.pending");

    // Write one payload but pass the hash of a *different* payload.
    std::fs::write(&pending, b"real content").unwrap();
    let wrong_hash = WitnessHash::from_witness(b"different content");

    let vault = VaultId::mint();
    let id = Identity::parse("hmn:bob").unwrap();
    let (record, key) = make_record_and_key(&id);

    let err = r
        .reserve_first_identity(&vault, &record, &key, wrong_hash, &pending)
        .await
        .unwrap_err();
    assert!(
        matches!(err, RegistryError::WitnessMismatch),
        "expected WitnessMismatch, got {err:?}"
    );
}
