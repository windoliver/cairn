//! Integration tests for [`SqliteIdentityRegistry`] (issue #50).
//!
//! Tests in this file exercise the real `SQLite` backend — no mocking.
//! In-memory databases (C4 step 1/2) and `tempfile` on-disk databases
//! (C4 step 2) are both used as appropriate.

use cairn_core::contract::identity_registry::{
    IdentityRegistry, IdentityVisibility, RegistryError,
};
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
