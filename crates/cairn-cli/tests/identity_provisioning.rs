//! Integration tests for [`IdentityService::open`] and
//! [`IdentityService::open_for_maintenance`] (issue #50, D2).

use std::fs;

use cairn_core::{
    contract::identity_registry::IdentityRegistry as _,
    domain::identity::{
        Identity, IdentityKind,
        keys::{IdentityRevision, KeyVersion, VaultId, WitnessHash},
        records::{IdentityKeyEntry, ProvisioningState, PublicIdentityRecord},
    },
    error::identity::IdentityServiceError,
};
use cairn_store_sqlite::SqliteIdentityRegistry;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build a minimal `PublicIdentityRecord` + `IdentityKeyEntry` for `id`.
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

/// Create a minimal vault layout under `dir`:
///
/// 1. Creates `dir/.cairn/`.
/// 2. Opens a file-backed `SqliteIdentityRegistry` at `dir/.cairn/cairn.db`.
/// 3. Runs `reserve_first_identity` to write `vault_meta`.
/// 4. Writes the same `vault_id` to `dir/.cairn/vault.id`.
///
/// Returns the `VaultId` that was committed so the caller can tamper with it.
async fn setup_vault_with_first_bind(dir: &tempfile::TempDir) -> VaultId {
    let cairn_dir = dir.path().join(".cairn");
    fs::create_dir_all(&cairn_dir).expect("create .cairn dir");

    let db_path = cairn_dir.join("cairn.db");
    let registry = SqliteIdentityRegistry::open(&db_path).expect("open registry");

    // Write a witness file for reserve_first_identity to verify.
    let binding_path = cairn_dir.join("vault.binding.pending");
    let witness_bytes = vec![1u8; 32];
    fs::write(&binding_path, &witness_bytes).expect("write witness file");
    let hash = WitnessHash::from_witness(&witness_bytes);

    let vault_id = VaultId::mint();
    let id = Identity::parse("hmn:tester").expect("valid identity");
    let (record, key) = make_record_and_key(&id);

    registry
        .reserve_first_identity(&vault_id, &record, &key, hash, &binding_path)
        .await
        .expect("reserve_first_identity");

    // Write the matching vault.id file.
    let vault_id_path = cairn_dir.join("vault.id");
    fs::write(&vault_id_path, vault_id.as_str()).expect("write vault.id");

    vault_id
}

// ── D2 tests ──────────────────────────────────────────────────────────────────

/// `IdentityService::open` must fail with `VaultIdConflict` when `.cairn/vault.id`
/// disagrees with the `vault_meta` row in the DB.
///
/// This is the primary fail-closed guard of the consistency check (spec §3.5).
#[tokio::test]
async fn open_fails_closed_when_file_id_disagrees_with_db() {
    let dir = tempfile::tempdir().expect("tempdir");
    setup_vault_with_first_bind(&dir).await;

    // Overwrite .cairn/vault.id with a freshly minted (different) VaultId.
    let different_id = VaultId::mint();
    let vault_id_path = dir.path().join(".cairn/vault.id");
    fs::write(&vault_id_path, different_id.as_str()).expect("overwrite vault.id");

    let result = cairn_cli::identity::IdentityService::open(dir.path().to_path_buf()).await;
    let Err(err) = result else {
        panic!("open must fail when vault.id disagrees with db, but it succeeded");
    };

    assert!(
        matches!(err, IdentityServiceError::VaultIdConflict { .. }),
        "expected VaultIdConflict, got: {err:?}",
    );
}

/// `IdentityService::open` must return `VaultIdMissing` when `.cairn/vault.id`
/// is absent (vault was never bootstrapped).
#[tokio::test]
async fn open_fails_when_vault_id_file_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cairn_dir = dir.path().join(".cairn");
    fs::create_dir_all(&cairn_dir).expect("create .cairn dir");
    // No vault.id file written.

    let result = cairn_cli::identity::IdentityService::open(dir.path().to_path_buf()).await;
    let Err(err) = result else {
        panic!("open must fail when vault.id is absent, but it succeeded");
    };

    assert!(
        matches!(err, IdentityServiceError::VaultIdMissing),
        "expected VaultIdMissing, got: {err:?}",
    );
}

/// `IdentityService::open` succeeds (returning an empty reconciliation report)
/// when `vault.id` and `vault_meta` agree and the keystore has no pending entries
/// to sweep.
#[tokio::test]
async fn open_succeeds_when_file_and_db_agree() {
    let dir = tempfile::tempdir().expect("tempdir");
    setup_vault_with_first_bind(&dir).await;

    // open() will sweep the keystore; the OS keystore may not have the pending
    // key, but NotFound is not vault-degrading — the call should succeed and
    // return an empty (non-degraded) report.
    let (svc, report) = cairn_cli::identity::IdentityService::open(dir.path().to_path_buf())
        .await
        .expect("open must succeed when file_id == db_id");

    // The vault_id on the service must match what we wrote.
    let written = fs::read_to_string(dir.path().join(".cairn/vault.id")).expect("read vault.id");
    assert_eq!(svc.vault_id.as_str(), written.trim());

    // Reconciliation report must not be degraded (no key-material to mismatch
    // at this point — orphan pending rows are not vault-degrading).
    assert!(
        !report.vault_degraded,
        "vault must not be degraded for a freshly bound registry with no keystore keys",
    );
}
