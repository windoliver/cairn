//! Integration tests for [`SqliteIdentityRegistry`] (issue #50).
//!
//! Tests in this file exercise the real `SQLite` backend — no mocking.
//! In-memory databases (C4 step 1/2) and `tempfile` on-disk databases
//! (C4 step 2) are both used as appropriate.

use cairn_core::contract::identity_registry::{
    IdentityRegistry, IdentityVisibility, PurgeAcknowledgement, PurgeReason, RegistryError,
};
use cairn_core::domain::identity::{
    Identity, IdentityKind,
    keys::{IdentityRevision, KeyVersion, SigningKey, VaultId, WitnessHash},
    receipts::{ReceiptOpKind, ReceiptPayload, RevocationReceipt, RotationReceipt},
    records::{IdentityKeyEntry, ProvisioningState, PublicIdentityRecord, ReceiptId},
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

// ── C5 · helpers ─────────────────────────────────────────────────────────────

/// Open an in-memory registry and run `reserve_first_identity` so that
/// `vault_meta` exists. Returns `(registry, tempdir)` — caller must keep
/// `_dir` alive to prevent the temp directory from being deleted.
async fn setup_first_bound_registry() -> (SqliteIdentityRegistry, tempfile::TempDir) {
    let r = SqliteIdentityRegistry::open_in_memory().expect("open in-memory registry");
    let dir = tempfile::tempdir().expect("tempdir");
    let binding = dir.path().join("vault.binding.pending");

    let witness = vec![1u8; 32];
    std::fs::write(&binding, &witness).expect("write witness file");
    let hash = WitnessHash::from_witness(&witness);

    let vault = VaultId::mint();
    let id = Identity::parse("hmn:first").expect("valid identity");
    let (record, key) = make_record_and_key(&id);

    r.reserve_first_identity(&vault, &record, &key, hash, &binding)
        .await
        .expect("reserve_first_identity");

    (r, dir)
}

// ── C5 · reserve_identity / activate_identity / list happy path ──────────────

/// Happy path: reserve → `list_pending` → activate → `get_identity` (Operational).
#[tokio::test]
async fn reserve_activate_list_happy_path() {
    let (r, _dir) = setup_first_bound_registry().await;

    let id = Identity::parse("hmn:bob").expect("valid identity");
    let (record, key) = make_record_and_key(&id);

    r.reserve_identity(&record, &key)
        .await
        .expect("reserve_identity");

    // list_pending includes hmn:first (from setup) + hmn:bob — check our entry
    // is present by searching by identity.
    let by_id = r
        .list_pending_by_identity(&id)
        .await
        .expect("list_pending_by_identity");
    assert_eq!(by_id.len(), 1, "expected 1 pending entry for hmn:bob");
    assert_eq!(by_id[0].identity, id);
    assert_eq!(by_id[0].key_version, KeyVersion::FIRST);

    // Total pending must be at least 1 (hmn:bob).
    let all_pending = r.list_pending().await.expect("list_pending");
    assert!(
        all_pending.iter().any(|e| e.identity == id),
        "hmn:bob must be in the pending list"
    );

    // Activate the identity.
    r.activate_identity(&id, KeyVersion::FIRST)
        .await
        .expect("activate_identity");

    // get_identity with Operational visibility should now find it.
    let active = r
        .get_identity(&id, IdentityVisibility::Operational)
        .await
        .expect("get_identity")
        .expect("identity should be present");
    assert_eq!(active.provisioning_state, ProvisioningState::Active);
    assert_eq!(active.id, id);

    // list_pending_by_identity for hmn:bob should now be empty.
    let pending_after = r
        .list_pending_by_identity(&id)
        .await
        .expect("list_pending_by_identity after activate");
    assert_eq!(
        pending_after.len(),
        0,
        "no pending entries for hmn:bob after activation"
    );
}

// ── C5 · reserve_identity rejects when vault_meta missing ────────────────────

/// `reserve_identity` must return `VaultMetaMissing` when first-bind has not
/// been committed yet (no `vault_meta` row).
#[tokio::test]
async fn reserve_identity_rejects_when_vault_meta_missing() {
    // Fresh registry — no first-bind.
    let r = SqliteIdentityRegistry::open_in_memory().expect("open in-memory registry");
    let id = Identity::parse("hmn:carol").expect("valid identity");
    let (record, key) = make_record_and_key(&id);

    let err = r
        .reserve_identity(&record, &key)
        .await
        .expect_err("should fail with VaultMetaMissing");
    assert!(
        matches!(err, RegistryError::VaultMetaMissing),
        "expected VaultMetaMissing, got {err:?}"
    );
}

// ── C5 · delete_pending removes the pending row ───────────────────────────────

/// `delete_pending` must remove the pending identity row so that
/// `list_pending` subsequently returns nothing.
#[tokio::test]
async fn delete_pending_removes_pending_row() {
    let (r, _dir) = setup_first_bound_registry().await;

    let id = Identity::parse("hmn:dave").expect("valid identity");
    let (record, key) = make_record_and_key(&id);

    r.reserve_identity(&record, &key)
        .await
        .expect("reserve_identity");

    // Confirm hmn:dave is pending.
    assert_eq!(
        r.list_pending_by_identity(&id)
            .await
            .expect("list_pending_by_identity")
            .len(),
        1,
        "expected hmn:dave to be pending"
    );

    r.delete_pending(&id, KeyVersion::FIRST)
        .await
        .expect("delete_pending");

    // After deletion, hmn:dave must no longer appear in list_pending_by_identity.
    assert_eq!(
        r.list_pending_by_identity(&id)
            .await
            .expect("list_pending_by_identity after delete")
            .len(),
        0,
        "hmn:dave should be removed after delete_pending"
    );

    // Calling delete_pending again must return NotFound.
    let err = r
        .delete_pending(&id, KeyVersion::FIRST)
        .await
        .expect_err("second delete_pending should fail");
    assert!(
        matches!(err, RegistryError::NotFound),
        "expected NotFound, got {err:?}"
    );
}

// ── C5 · activate twice errors on second call ────────────────────────────────

/// Calling `activate_identity` a second time on an already-active row must
/// return an error (the `WHERE provisioning_state='pending'` clause prevents
/// the update, so we get `Backend("invalid state transition")`).
#[tokio::test]
async fn activate_identity_errors_when_already_active() {
    let (r, _dir) = setup_first_bound_registry().await;

    let id = Identity::parse("hmn:eve").expect("valid identity");
    let (record, key) = make_record_and_key(&id);

    r.reserve_identity(&record, &key)
        .await
        .expect("reserve_identity");
    r.activate_identity(&id, KeyVersion::FIRST)
        .await
        .expect("first activate_identity");

    // Second activate: the row is now 'active', not 'pending', so rowcount=0
    // and the follow-up SELECT detects the non-pending state.
    let err = r
        .activate_identity(&id, KeyVersion::FIRST)
        .await
        .expect_err("second activate should fail");
    assert!(
        matches!(err, RegistryError::Backend(_)),
        "expected Backend error for invalid state transition, got {err:?}"
    );
}

// ── C5 · get_identity visibility filter ──────────────────────────────────────

/// A pending identity must be visible with `IncludingPending` but not with
/// `Operational`.
#[tokio::test]
async fn get_identity_with_visibility_filter() {
    let (r, _dir) = setup_first_bound_registry().await;

    let id = Identity::parse("hmn:frank").expect("valid identity");
    let (record, key) = make_record_and_key(&id);

    r.reserve_identity(&record, &key)
        .await
        .expect("reserve_identity");

    // Operational: pending row must not appear.
    let not_visible = r
        .get_identity(&id, IdentityVisibility::Operational)
        .await
        .expect("get_identity Operational");
    assert!(
        not_visible.is_none(),
        "pending identity must not appear under Operational visibility"
    );

    // IncludingPending: row must be visible.
    let visible = r
        .get_identity(&id, IdentityVisibility::IncludingPending)
        .await
        .expect("get_identity IncludingPending")
        .expect("pending identity must appear under IncludingPending");
    assert_eq!(visible.provisioning_state, ProvisioningState::Pending);
    assert_eq!(visible.id, id);
}

// ── C6 · rotation helpers ─────────────────────────────────────────────────────

/// Build a `RotationReceipt` for `target` with the given key versions, signed
/// by `signing_key` owned by `signer_id`.
fn make_rotation_receipt(
    target: &Identity,
    signer_key_version: KeyVersion,
    old_key_version: KeyVersion,
    new_key_version: KeyVersion,
    signing_key: &SigningKey,
    signer_id: &Identity,
) -> RotationReceipt {
    let payload = ReceiptPayload {
        op_kind: ReceiptOpKind::Rotation,
        target: target.clone(),
        signer: signer_id.clone(),
        signer_key_version,
        old_key_version: Some(old_key_version),
        new_key_version: Some(new_key_version),
        issued_at: chrono::Utc::now(),
    };
    let sig = payload.sign(signing_key).expect("sign payload");
    RotationReceipt {
        id: ReceiptId(0),
        payload,
        signature: sig.to_bytes().to_vec(),
        pending_eviction: false,
    }
}

/// Build an `IdentityKeyEntry` for `identity` at `key_version` using the
/// verifying key bytes from `signing_key`.
fn make_key_entry(
    identity: &Identity,
    key_version: KeyVersion,
    signing_key: &SigningKey,
) -> IdentityKeyEntry {
    let vk = signing_key.verifying_key();
    IdentityKeyEntry {
        identity_id: identity.clone(),
        key_version,
        public_key: vk.to_bytes(),
        signed_predecessor: None,
        created_at: chrono::Utc::now(),
        superseded_at: None,
    }
}

/// Set up a registry with an active identity `hmn:alice` at key v1 and the
/// signing key used for that identity.  Returns `(registry, tempdir,
/// alice_identity, alice_signing_key)`.
async fn setup_active_identity() -> (
    SqliteIdentityRegistry,
    tempfile::TempDir,
    Identity,
    SigningKey,
) {
    let (r, dir) = setup_first_bound_registry().await;

    // Activate the first-bound identity so we can reserve a second one.
    let first = Identity::parse("hmn:first").expect("valid identity");
    r.activate_identity(&first, KeyVersion::FIRST)
        .await
        .expect("activate hmn:first");

    // Reserve + activate hmn:alice.
    let alice = Identity::parse("hmn:alice").expect("valid identity");
    let mut rng = rand_core::OsRng;
    let sk = SigningKey::generate(&mut rng);
    let (record, key) = {
        let now = chrono::Utc::now();
        let record = PublicIdentityRecord {
            id: alice.clone(),
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
            identity_id: alice.clone(),
            key_version: KeyVersion::FIRST,
            public_key: sk.verifying_key().to_bytes(),
            signed_predecessor: None,
            created_at: now,
            superseded_at: None,
        };
        (record, key)
    };
    r.reserve_identity(&record, &key)
        .await
        .expect("reserve hmn:alice");
    r.activate_identity(&alice, KeyVersion::FIRST)
        .await
        .expect("activate hmn:alice");

    (r, dir, alice, sk)
}

// ── C6 · apply_rotation CAS rejects stale expected_current ───────────────────

/// `apply_rotation` with `expected_current` = v2 when the registry holds v1
/// must return `KeyVersionConflict`.
#[tokio::test]
async fn apply_rotation_cas_rejects_stale_observed() {
    let (r, _dir, alice, sk) = setup_active_identity().await;

    let v1 = KeyVersion::FIRST;
    let v2 = v1.next().expect("v2");
    let v3 = v2.next().expect("v3");

    // Build a receipt claiming to rotate from v2→v3, but the registry has v1.
    let receipt = make_rotation_receipt(&alice, v1, v2, v3, &sk, &alice);
    let new_key = make_key_entry(&alice, v3, &sk);

    // Supply expected_current=v2 which is stale (registry holds v1).
    let err = r
        .apply_rotation(&receipt, v2, &new_key)
        .await
        .expect_err("should fail with KeyVersionConflict");
    assert!(
        matches!(err, RegistryError::KeyVersionConflict { .. }),
        "expected KeyVersionConflict, got {err:?}"
    );
}

// ── C6 · apply_rotation happy path ───────────────────────────────────────────

/// Full rotation v1→v2: asserts `current_key_version` advanced, predecessor
/// `superseded_at` is set, new `identity_keys` row exists, receipt row is
/// written, and `pending_rotations` is cleared.
#[tokio::test]
async fn apply_rotation_happy_path() {
    let (r, _dir, alice, sk) = setup_active_identity().await;

    let v1 = KeyVersion::FIRST;
    let v2 = v1.next().expect("v2");

    // Insert a pending_rotation intent first (apply_rotation should clear it).
    r.insert_pending_rotation(&alice, v2, "test-handle")
        .await
        .expect("insert_pending_rotation");
    assert_eq!(
        r.list_pending_rotations(&alice).await.expect("list").len(),
        1,
        "pending rotation should be present"
    );

    // Build the rotation receipt and new key.
    let mut rng = rand_core::OsRng;
    let new_sk = SigningKey::generate(&mut rng);
    let receipt = make_rotation_receipt(&alice, v1, v1, v2, &sk, &alice);
    let new_key = make_key_entry(&alice, v2, &new_sk);

    r.apply_rotation(&receipt, v1, &new_key)
        .await
        .expect("apply_rotation");

    // current_key_version must have advanced to v2.
    let rec = r
        .get_identity(&alice, IdentityVisibility::Operational)
        .await
        .expect("get_identity")
        .expect("alice must exist");
    assert_eq!(
        rec.current_key_version, v2,
        "current_key_version should be v2"
    );

    // Both key versions must appear in list_keys; v1 must be superseded.
    let keys = r.list_keys(&alice).await.expect("list_keys");
    assert_eq!(keys.len(), 2, "two key rows expected");
    let k1 = keys.iter().find(|k| k.key_version == v1).expect("v1 key");
    assert!(k1.superseded_at.is_some(), "v1 must be superseded");
    let k2 = keys.iter().find(|k| k.key_version == v2).expect("v2 key");
    assert!(k2.superseded_at.is_none(), "v2 must not be superseded yet");

    // pending_rotations must be empty after apply.
    let pending = r
        .list_pending_rotations(&alice)
        .await
        .expect("list after rotation");
    assert!(pending.is_empty(), "pending_rotations must be cleared");
}

// ── C6 · pending_rotation lifecycle ──────────────────────────────────────────

/// Insert a pending rotation, verify it appears in `list_pending_rotations`,
/// then delete it and verify the list is empty.
#[tokio::test]
async fn pending_rotation_lifecycle() {
    let (r, _dir, alice, _sk) = setup_active_identity().await;

    let v2 = KeyVersion::FIRST.next().expect("v2");

    // Initially empty.
    let before = r
        .list_pending_rotations(&alice)
        .await
        .expect("list before insert");
    assert!(before.is_empty(), "no pending rotations initially");

    // Insert.
    r.insert_pending_rotation(&alice, v2, "my-handle")
        .await
        .expect("insert_pending_rotation");

    let after_insert = r
        .list_pending_rotations(&alice)
        .await
        .expect("list after insert");
    assert_eq!(after_insert.len(), 1);
    assert_eq!(after_insert[0].0, v2);
    assert_eq!(after_insert[0].1, "my-handle");

    // Delete.
    r.delete_pending_rotation(&alice, v2)
        .await
        .expect("delete_pending_rotation");

    let after_delete = r
        .list_pending_rotations(&alice)
        .await
        .expect("list after delete");
    assert!(
        after_delete.is_empty(),
        "pending rotation cleared after delete"
    );

    // Second delete must return NotFound.
    let err = r
        .delete_pending_rotation(&alice, v2)
        .await
        .expect_err("second delete should fail");
    assert!(
        matches!(err, RegistryError::NotFound),
        "expected NotFound, got {err:?}"
    );
}

// ── C7 · revocation helpers ───────────────────────────────────────────────────

/// Build a [`RevocationReceipt`] for `target` signed by `signing_key` owned
/// by `signer_id` at `signer_key_version`.
fn make_revocation_receipt(
    target: &Identity,
    signer_id: &Identity,
    signer_key_version: KeyVersion,
    signing_key: &SigningKey,
) -> RevocationReceipt {
    let payload = ReceiptPayload {
        op_kind: ReceiptOpKind::Revocation,
        target: target.clone(),
        signer: signer_id.clone(),
        signer_key_version,
        old_key_version: Some(KeyVersion::FIRST),
        new_key_version: None,
        issued_at: chrono::Utc::now(),
    };
    let sig = payload.sign(signing_key).expect("sign revocation payload");
    RevocationReceipt {
        id: ReceiptId(0),
        payload,
        signature: sig.to_bytes().to_vec(),
        pending_key_disable: true,
    }
}

// ── C7 · begin_revocation transitions to revoke_pending ──────────────────────

/// `begin_revocation` must transition an `active` identity to `revoke_pending`
/// and insert a receipt row with `pending_key_disable = 1`.
#[tokio::test]
async fn begin_revocation_transitions_to_revoke_pending() {
    let (r, _dir, alice, sk) = setup_active_identity().await;

    let receipt = make_revocation_receipt(&alice, &alice, KeyVersion::FIRST, &sk);
    r.begin_revocation(&receipt)
        .await
        .expect("begin_revocation should succeed");

    // Identity must now be revoke_pending (visible under Audit).
    let rec = r
        .get_identity(&alice, IdentityVisibility::Audit)
        .await
        .expect("get_identity")
        .expect("alice must exist");
    assert_eq!(
        rec.provisioning_state,
        ProvisioningState::RevokePending,
        "state must be revoke_pending"
    );
    assert!(rec.revoked_at.is_some(), "revoked_at must be set");

    // list_revoke_pending must include alice.
    let pending = r.list_revoke_pending().await.expect("list_revoke_pending");
    assert!(
        pending.iter().any(|e| e.identity == alice),
        "alice must appear in revoke_pending list"
    );
}

// ── C7 · finalise_revocation sets revoked and clears flag ────────────────────

/// `finalise_revocation` after `begin_revocation` must move state to `Revoked`
/// and clear `pending_key_disable`. `list_revoke_pending` must then be empty.
#[tokio::test]
async fn finalise_revocation_sets_revoked_and_clears_flag() {
    let (r, _dir, alice, sk) = setup_active_identity().await;

    let receipt = make_revocation_receipt(&alice, &alice, KeyVersion::FIRST, &sk);
    r.begin_revocation(&receipt)
        .await
        .expect("begin_revocation");

    r.finalise_revocation(&alice)
        .await
        .expect("finalise_revocation");

    // Audit visibility must show Revoked state.
    let rec = r
        .get_identity(&alice, IdentityVisibility::Audit)
        .await
        .expect("get_identity")
        .expect("alice must still exist");
    assert_eq!(
        rec.provisioning_state,
        ProvisioningState::Revoked,
        "state must be Revoked after finalise"
    );

    // list_revoke_pending must be empty.
    let pending = r
        .list_revoke_pending()
        .await
        .expect("list_revoke_pending after finalise");
    assert!(
        pending.is_empty(),
        "list_revoke_pending must be empty after finalise"
    );
}

// ── C7 · begin_revocation rejects non-active state ───────────────────────────

/// `begin_revocation` on a `pending` identity must return `AlreadyRevoked`
/// (or an equivalent rejection error), since only `active` identities can be
/// revoked.
#[tokio::test]
async fn begin_revocation_rejects_non_active() {
    let (r, _dir) = setup_first_bound_registry().await;

    // Reserve a new identity (pending state, not active).
    let id = Identity::parse("hmn:pending-target").expect("valid identity");
    let (record, key) = make_record_and_key(&id);
    r.reserve_identity(&record, &key)
        .await
        .expect("reserve_identity");

    let mut rng = rand_core::OsRng;
    let sk = SigningKey::generate(&mut rng);
    let receipt = make_revocation_receipt(&id, &id, KeyVersion::FIRST, &sk);

    let err = r
        .begin_revocation(&receipt)
        .await
        .expect_err("begin_revocation on pending identity must fail");
    assert!(
        matches!(
            err,
            RegistryError::AlreadyRevoked { .. } | RegistryError::NotFound
        ),
        "expected AlreadyRevoked or NotFound, got {err:?}"
    );
}

// ── C8 · mark_purge_pending admits pending start state ───────────────────────

/// `mark_purge_pending` must succeed when the identity is in `pending` state
/// and transition it to `purge_pending`.
#[tokio::test]
async fn mark_purge_pending_admits_pending_start_state() {
    let (r, _dir) = setup_first_bound_registry().await;

    let id = Identity::parse("hmn:purge-pending-test").expect("valid identity");
    let (record, key) = make_record_and_key(&id);
    r.reserve_identity(&record, &key)
        .await
        .expect("reserve_identity");

    // Confirm state is Pending.
    let rec = r
        .get_identity(&id, IdentityVisibility::IncludingPending)
        .await
        .expect("get_identity")
        .expect("must exist");
    assert_eq!(rec.provisioning_state, ProvisioningState::Pending);

    let ack = PurgeAcknowledgement::for_test();
    let reason = PurgeReason("GDPR erasure test".to_owned());
    r.mark_purge_pending(&id, &ack, reason)
        .await
        .expect("mark_purge_pending from pending state must succeed");

    // State must now be PurgePending.
    let rec_after = r
        .get_identity(&id, IdentityVisibility::IncludingPurgePending)
        .await
        .expect("get_identity after mark_purge_pending")
        .expect("identity must still be visible");
    assert_eq!(
        rec_after.provisioning_state,
        ProvisioningState::PurgePending,
        "state must be PurgePending"
    );
    assert!(
        rec_after.purge_requested_at.is_some(),
        "purge_requested_at must be set"
    );

    // list_purge_pending must include this identity.
    let purge_list = r.list_purge_pending().await.expect("list_purge_pending");
    assert!(
        purge_list.iter().any(|e| e.identity == id),
        "identity must appear in purge_pending list"
    );
    let entry = purge_list
        .iter()
        .find(|e| e.identity == id)
        .expect("entry must exist");
    assert_eq!(entry.purge_reason, "GDPR erasure test");
}

// ── C8 · mark_purge_pending rejects revoke_pending state ─────────────────────

/// `mark_purge_pending` on an identity in `revoke_pending` state must return
/// `InvalidPurgeStartState`.
#[tokio::test]
async fn mark_purge_pending_rejects_revoke_pending() {
    let (r, _dir, alice, sk) = setup_active_identity().await;

    // Begin revocation to put alice in revoke_pending.
    let receipt = make_revocation_receipt(&alice, &alice, KeyVersion::FIRST, &sk);
    r.begin_revocation(&receipt)
        .await
        .expect("begin_revocation");

    let ack = PurgeAcknowledgement::for_test();
    let reason = PurgeReason("test".to_owned());
    let err = r
        .mark_purge_pending(&alice, &ack, reason)
        .await
        .expect_err("must reject revoke_pending state");
    assert!(
        matches!(err, RegistryError::InvalidPurgeStartState { .. }),
        "expected InvalidPurgeStartState, got {err:?}"
    );
}

// ── C8 · finalise_purge sets purged ──────────────────────────────────────────

/// `finalise_purge` after `mark_purge_pending` must transition the identity to
/// `Purged` state.
#[tokio::test]
async fn finalise_purge_sets_purged() {
    let (r, _dir) = setup_first_bound_registry().await;

    let id = Identity::parse("hmn:finalise-purge-test").expect("valid identity");
    let (record, key) = make_record_and_key(&id);
    r.reserve_identity(&record, &key)
        .await
        .expect("reserve_identity");

    let ack = PurgeAcknowledgement::for_test();
    let reason = PurgeReason("test finalise".to_owned());
    r.mark_purge_pending(&id, &ack, reason)
        .await
        .expect("mark_purge_pending");

    r.finalise_purge(&id).await.expect("finalise_purge");

    // Audit visibility must show Purged.
    let rec = r
        .get_identity(&id, IdentityVisibility::Audit)
        .await
        .expect("get_identity")
        .expect("identity must be visible under Audit after purge");
    assert_eq!(
        rec.provisioning_state,
        ProvisioningState::Purged,
        "state must be Purged after finalise_purge"
    );
    assert!(rec.purged_at.is_some(), "purged_at must be set");
}

// ── C8 · list_purge_pending returns pending rows ──────────────────────────────

/// `list_purge_pending` must return all identities in `purge_pending` state
/// and exclude those that have been finalised.
#[tokio::test]
async fn list_purge_pending_returns_pending_rows() {
    let (r, _dir) = setup_first_bound_registry().await;

    let id1 = Identity::parse("hmn:purge-list-1").expect("valid identity");
    let id2 = Identity::parse("hmn:purge-list-2").expect("valid identity");

    for id in [&id1, &id2] {
        let (record, key) = make_record_and_key(id);
        r.reserve_identity(&record, &key)
            .await
            .expect("reserve_identity");
        let ack = PurgeAcknowledgement::for_test();
        let reason = PurgeReason(format!("reason for {}", id.as_str()));
        r.mark_purge_pending(id, &ack, reason)
            .await
            .expect("mark_purge_pending");
    }

    // Both must appear.
    let list = r.list_purge_pending().await.expect("list_purge_pending");
    assert!(
        list.iter().any(|e| e.identity == id1),
        "id1 must appear in purge_pending list"
    );
    assert!(
        list.iter().any(|e| e.identity == id2),
        "id2 must appear in purge_pending list"
    );

    // Finalise id1 — it must be removed from the list.
    r.finalise_purge(&id1).await.expect("finalise_purge id1");

    let list_after = r
        .list_purge_pending()
        .await
        .expect("list_purge_pending after finalise");
    assert!(
        !list_after.iter().any(|e| e.identity == id1),
        "id1 must not appear after finalise_purge"
    );
    assert!(
        list_after.iter().any(|e| e.identity == id2),
        "id2 must still appear"
    );
}

// ── C9 · clear_pending_eviction / list_pending_evictions ─────────────────────

/// After `apply_rotation` with `pending_eviction = true`, calling
/// `clear_pending_eviction` must clear the flag so that
/// `list_pending_evictions` returns empty.
#[tokio::test]
async fn clear_pending_eviction_clears_flag() {
    let (r, _dir, alice, sk) = setup_active_identity().await;

    let v1 = KeyVersion::FIRST;
    let v2 = v1.next().expect("v2");

    // Build a rotation receipt with pending_eviction = true.
    let mut rng = rand_core::OsRng;
    let new_sk = SigningKey::generate(&mut rng);
    let mut receipt = make_rotation_receipt(&alice, v1, v1, v2, &sk, &alice);
    receipt.pending_eviction = true;
    let new_key = make_key_entry(&alice, v2, &new_sk);

    r.apply_rotation(&receipt, v1, &new_key)
        .await
        .expect("apply_rotation with pending_eviction");

    // list_pending_evictions must return the entry.
    let evictions = r
        .list_pending_evictions()
        .await
        .expect("list_pending_evictions");
    assert_eq!(evictions.len(), 1, "one eviction pending after rotation");
    let entry = &evictions[0];
    assert_eq!(entry.identity, alice);
    assert_eq!(entry.evict_version, v2);

    // Clear the flag.
    r.clear_pending_eviction(&entry.receipt_id)
        .await
        .expect("clear_pending_eviction");

    // Now list must be empty.
    let after = r
        .list_pending_evictions()
        .await
        .expect("list_pending_evictions after clear");
    assert!(
        after.is_empty(),
        "list_pending_evictions must be empty after clear"
    );
}

/// After a rotation with `pending_eviction = true` and WITHOUT clearing,
/// `list_pending_evictions` must return one entry.
#[tokio::test]
async fn list_pending_evictions_lists_unfinalised() {
    let (r, _dir, alice, sk) = setup_active_identity().await;

    let v1 = KeyVersion::FIRST;
    let v2 = v1.next().expect("v2");
    let mut rng = rand_core::OsRng;
    let new_sk = SigningKey::generate(&mut rng);
    let mut receipt = make_rotation_receipt(&alice, v1, v1, v2, &sk, &alice);
    receipt.pending_eviction = true;
    let new_key = make_key_entry(&alice, v2, &new_sk);

    r.apply_rotation(&receipt, v1, &new_key)
        .await
        .expect("apply_rotation");

    let evictions = r
        .list_pending_evictions()
        .await
        .expect("list_pending_evictions");
    assert_eq!(evictions.len(), 1, "one entry pending eviction");
    assert_eq!(evictions[0].identity, alice);
}

// ── C9 · clear_pending_key_disable / list_pending_key_disables ───────────────

/// `begin_revocation` sets `pending_key_disable = 1`. After
/// `finalise_revocation`, the flag is cleared (done inside `finalise_revocation`
/// itself). Verify that `list_pending_key_disables` is empty after finalise.
#[tokio::test]
async fn clear_pending_key_disable_after_finalise() {
    let (r, _dir, alice, sk) = setup_active_identity().await;

    let receipt = make_revocation_receipt(&alice, &alice, KeyVersion::FIRST, &sk);
    r.begin_revocation(&receipt)
        .await
        .expect("begin_revocation");

    // Flag should be set: verify list is non-empty.
    let before = r
        .list_pending_key_disables()
        .await
        .expect("list_pending_key_disables before finalise");
    assert_eq!(
        before.len(),
        1,
        "one pending_key_disable before finalise_revocation"
    );

    r.finalise_revocation(&alice)
        .await
        .expect("finalise_revocation");

    // After finalise, list must be empty (finalise_revocation cleared the flag).
    let after = r
        .list_pending_key_disables()
        .await
        .expect("list_pending_key_disables after finalise");
    assert!(
        after.is_empty(),
        "list_pending_key_disables must be empty after finalise_revocation"
    );
}

/// `begin_revocation` without `finalise_revocation` must leave one entry in
/// `list_pending_key_disables`.
#[tokio::test]
async fn list_pending_key_disables_returns_revocation_in_flight() {
    let (r, _dir, alice, sk) = setup_active_identity().await;

    let receipt = make_revocation_receipt(&alice, &alice, KeyVersion::FIRST, &sk);
    r.begin_revocation(&receipt)
        .await
        .expect("begin_revocation");

    let pending = r
        .list_pending_key_disables()
        .await
        .expect("list_pending_key_disables");
    assert_eq!(pending.len(), 1, "one pending_key_disable in-flight");
    assert_eq!(pending[0].identity, alice);
    // retained_versions for an unrevoked identity is the active key.
    assert!(!pending[0].retained_versions.is_empty(), "retained_versions must be non-empty");
}
