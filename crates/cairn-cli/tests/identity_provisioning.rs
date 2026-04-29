//! Integration tests for [`IdentityService::open`],
//! [`IdentityService::open_for_maintenance`] (issue #50, D2),
//! `commit_first_identity` (D3), and `init_defaults` (D5).

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
use cairn_core::{
    contract::keystore::Keystore as _,
    domain::identity::provision::{ProvisionInput, build_provisioning_plan},
};
use cairn_store_sqlite::SqliteIdentityRegistry;
use cairn_test_fixtures::MemoryKeystore;

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

// ── D3 helpers ────────────────────────────────────────────────────────────────

/// Build a minimal [`ProvisioningPlan`] for a human identity with slug `slug`.
fn make_plan_for(
    slug: &str,
) -> (
    VaultId,
    cairn_core::domain::identity::provision::ProvisioningPlan,
) {
    let vault_id = VaultId::mint();
    let id = Identity::parse(format!("hmn:{slug}:v1")).expect("valid identity");
    let input = ProvisionInput {
        vault_id: vault_id.clone(),
        id,
        kind: IdentityKind::Human,
        revision: IdentityRevision::FIRST,
    };
    let plan = build_provisioning_plan(input, &mut rand_core::OsRng, chrono::Utc::now());
    (vault_id, plan)
}

// ── D3 tests ──────────────────────────────────────────────────────────────────

/// `commit_first_identity` must refuse when the witness slot in the keystore is
/// already populated for the given vault id (namespace already claimed by
/// another process or node).
///
/// Verifies the namespace-ownership probe in step 0 of spec §3.7.
#[tokio::test]
async fn first_bind_refuses_when_namespace_already_has_witness() {
    let dir = tempfile::tempdir().expect("tempdir");
    let keystore = MemoryKeystore::new();
    let vault_id = VaultId::mint();

    // Pre-populate the witness slot so the namespace appears claimed.
    keystore
        .store_secret(
            &cairn_core::domain::identity::keys::SecretHandle::for_witness(vault_id.clone()),
            &[0u8; 32],
        )
        .await
        .expect("store witness");

    // Build a plan (the plan's vault_id must match the pre-populated one).
    let id = Identity::parse("hmn:taken:v1").expect("valid identity");
    let input = ProvisionInput {
        vault_id: vault_id.clone(),
        id,
        kind: IdentityKind::Human,
        revision: IdentityRevision::FIRST,
    };
    let plan = build_provisioning_plan(input, &mut rand_core::OsRng, chrono::Utc::now());

    let registry = SqliteIdentityRegistry::open_in_memory().expect("open registry");

    let err = cairn_cli::identity::commit_first_identity(
        dir.path(),
        vault_id,
        plan,
        &registry,
        &keystore,
    )
    .await
    .expect_err("must fail when namespace is already claimed");

    assert!(
        matches!(err, IdentityServiceError::VaultNamespaceClaimed { .. }),
        "expected VaultNamespaceClaimed, got: {err:?}",
    );
}

/// Happy path: `commit_first_identity` completes all six steps and leaves the
/// vault in a consistent, fully activated state.
///
/// Postconditions verified (spec §3.7):
/// - `.cairn/vault.binding` exists and contains exactly 32 bytes (the witness hash).
/// - `.cairn/vault.binding.pending` has been removed.
/// - `registry.read_vault_meta()` returns `Some` (first-bind committed).
/// - `registry.get_identity(id, Operational)` returns `Some` with `state = Active`.
/// - `keystore.load_signing_key(secret_handle)` matches `plan`'s public key.
#[tokio::test]
async fn first_bind_full_path_lands_active_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let keystore = MemoryKeystore::new();

    let (vault_id, plan) = make_plan_for("alice");
    let identity_id = plan.identity.id.clone();
    let secret_handle = plan.secret_handle.clone();
    let expected_pubkey = plan.signing_key.verifying_key().to_bytes();

    let registry = SqliteIdentityRegistry::open_in_memory().expect("open registry");

    cairn_cli::identity::commit_first_identity(
        dir.path(),
        vault_id.clone(),
        plan,
        &registry,
        &keystore,
    )
    .await
    .expect("commit_first_identity must succeed on the happy path");

    // ── File-system postconditions ────────────────────────────────────────────

    let binding_path = dir.path().join(".cairn/vault.binding");
    let binding_bytes = fs::read(&binding_path).expect(".cairn/vault.binding must exist");
    assert_eq!(
        binding_bytes.len(),
        32,
        "vault.binding must contain exactly 32 bytes (witness hash)",
    );

    let pending_path = dir.path().join(".cairn/vault.binding.pending");
    assert!(
        !pending_path.exists(),
        "vault.binding.pending must have been removed after successful commit",
    );

    // ── Registry postconditions ───────────────────────────────────────────────

    let vault_meta = registry
        .read_vault_meta()
        .await
        .expect("read_vault_meta")
        .expect("vault_meta must be Some after commit");
    assert_eq!(
        vault_meta.0.as_str(),
        vault_id.as_str(),
        "vault_meta vault_id must match the one we committed",
    );

    let record = registry
        .get_identity(
            &identity_id,
            cairn_core::contract::identity_registry::IdentityVisibility::Operational,
        )
        .await
        .expect("get_identity")
        .expect("identity must be visible at Operational level after activation");
    assert_eq!(
        record.provisioning_state,
        ProvisioningState::Active,
        "identity must be Active after commit_first_identity",
    );

    // ── Keystore postcondition ────────────────────────────────────────────────

    let loaded_key = keystore
        .load_signing_key(&secret_handle)
        .await
        .expect("signing key must be stored after commit");
    assert_eq!(
        loaded_key.verifying_key().to_bytes(),
        expected_pubkey,
        "stored public key must match the plan's signing key",
    );
}

// Note: a "lock-busy" test is omitted intentionally — it would block for up to
// 30 seconds waiting for VaultBindingLock::acquire to time out, making the test
// suite unacceptably slow. The lock machinery is unit-tested in lock.rs instead.

// ── D4 helpers ────────────────────────────────────────────────────────────────

/// Seed a vault with first-bind already committed, then return an
/// `IdentityService` wired to the same registry + a fresh `MemoryKeystore`.
///
/// The first-bind identity is `hmn:system:v1` so it does not collide with
/// the target identity under test.  The `vault_id` is written to
/// `<dir>/.cairn/vault.id` so that `commit_first_identity` can write the
/// binding sentinel file if needed by any code path.
async fn setup_service_after_first_bind(
    dir: &tempfile::TempDir,
) -> (cairn_cli::identity::IdentityService, VaultId) {
    use cairn_test_fixtures::MemoryKeystore;
    use std::sync::Arc;

    let cairn_dir = dir.path().join(".cairn");
    fs::create_dir_all(&cairn_dir).expect("create .cairn dir");

    let registry = SqliteIdentityRegistry::open_in_memory().expect("open in-memory registry");

    // Run first-bind for a "system" seed identity so vault_meta is present.
    let vault_id = VaultId::mint();
    let sys_id = Identity::parse("hmn:system:v1").expect("valid");
    let sys_input = ProvisionInput {
        vault_id: vault_id.clone(),
        id: sys_id,
        kind: IdentityKind::Human,
        revision: IdentityRevision::FIRST,
    };
    let keystore = MemoryKeystore::new();
    let sys_plan = build_provisioning_plan(sys_input, &mut rand_core::OsRng, chrono::Utc::now());

    cairn_cli::identity::commit_first_identity(
        dir.path(),
        vault_id.clone(),
        sys_plan,
        &registry,
        &keystore,
    )
    .await
    .expect("first bind for system identity");

    // Write vault.id so any file-based checks pass.
    fs::write(cairn_dir.join("vault.id"), vault_id.as_str()).expect("write vault.id");

    let svc = cairn_cli::identity::IdentityService::new_for_test(
        dir.path().to_path_buf(),
        vault_id.clone(),
        Arc::new(registry),
        Arc::new(keystore),
    );
    (svc, vault_id)
}

// ── D4 tests ──────────────────────────────────────────────────────────────────

/// `provision` must self-heal an orphan pending row (pending row exists but
/// the keypair was never stored in the keystore) and land the identity in
/// `Active` state.
///
/// Simulates a crash between `reserve_identity` and `store_keypair`.
#[tokio::test]
async fn provision_self_heals_pending_orphan() {
    use cairn_test_fixtures::MemoryKeystore;
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("tempdir");
    let cairn_dir = dir.path().join(".cairn");
    fs::create_dir_all(&cairn_dir).expect("create .cairn dir");

    let registry = SqliteIdentityRegistry::open_in_memory().expect("open in-memory registry");
    let vault_id = VaultId::mint();
    let keystore = MemoryKeystore::new();

    // Seed vault_meta with first-bind for a system identity.
    let sys_id = Identity::parse("hmn:system:v1").expect("valid");
    let sys_input = ProvisionInput {
        vault_id: vault_id.clone(),
        id: sys_id,
        kind: IdentityKind::Human,
        revision: IdentityRevision::FIRST,
    };
    let sys_plan = build_provisioning_plan(sys_input, &mut rand_core::OsRng, chrono::Utc::now());
    cairn_cli::identity::commit_first_identity(
        dir.path(),
        vault_id.clone(),
        sys_plan,
        &registry,
        &keystore,
    )
    .await
    .expect("first bind for system identity");
    fs::write(cairn_dir.join("vault.id"), vault_id.as_str()).expect("write vault.id");

    // Manually insert an orphan pending row for the target identity — no
    // keypair is stored in the keystore, simulating a mid-provisioning crash.
    let target_id = Identity::parse("hmn:bob:v1").expect("valid");
    let (orphan_record, orphan_key) = make_record_and_key(&target_id);
    registry
        .reserve_identity(&orphan_record, &orphan_key)
        .await
        .expect("reserve orphan pending row");
    // Deliberately do NOT store the keypair → keystore has no entry for
    // `hmn:bob:v1`, so the pending row is an orphan.

    // Now call provision — it must detect the orphan, delete it, and
    // provision fresh, leaving the identity in Active state.
    let svc = cairn_cli::identity::IdentityService::new_for_test(
        dir.path().to_path_buf(),
        vault_id,
        Arc::new(registry),
        Arc::new(keystore),
    );
    let bob_input = ProvisionInput {
        vault_id: svc.vault_id.clone(),
        id: target_id.clone(),
        kind: IdentityKind::Human,
        revision: IdentityRevision::FIRST,
    };
    svc.provision(IdentityKind::Human, bob_input, &mut rand_core::OsRng)
        .await
        .expect("provision must succeed after self-healing the orphan");

    let row = svc
        .registry
        .get_identity(
            &target_id,
            cairn_core::contract::identity_registry::IdentityVisibility::Operational,
        )
        .await
        .expect("get_identity")
        .expect("identity must be visible after provision");
    assert_eq!(
        row.provisioning_state,
        ProvisioningState::Active,
        "identity must be Active after self-healing provision",
    );
}

/// `provision` on a fully clean vault (first-bind already done) must land a
/// fresh human identity in `Active` state.
#[tokio::test]
async fn provision_fresh_human_identity_lands_active() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (svc, vault_id) = setup_service_after_first_bind(&dir).await;

    let alice_id = Identity::parse("hmn:alice:v1").expect("valid");
    let alice_input = ProvisionInput {
        vault_id,
        id: alice_id.clone(),
        kind: IdentityKind::Human,
        revision: IdentityRevision::FIRST,
    };

    svc.provision(IdentityKind::Human, alice_input, &mut rand_core::OsRng)
        .await
        .expect("provision must succeed for a fresh human identity");

    let row = svc
        .registry
        .get_identity(
            &alice_id,
            cairn_core::contract::identity_registry::IdentityVisibility::Operational,
        )
        .await
        .expect("get_identity")
        .expect("alice must be visible after provision");
    assert_eq!(
        row.provisioning_state,
        ProvisioningState::Active,
        "fresh provisioned identity must be Active",
    );
}

// ── D5 helpers ────────────────────────────────────────────────────────────────

/// Create a service after first-bind, provision a human identity at v1, then
/// immediately purge it (Active → `PurgePending` → Purged), and return the service
/// together with the v1 identity id and the slug used.
///
/// Uses `cairn_core::contract::identity_registry::PurgeAcknowledgement::for_test`
/// which is available under `cfg(test)`.
async fn setup_service_with_purged_default_human(
    dir: &tempfile::TempDir,
) -> (cairn_cli::identity::IdentityService, String) {
    use cairn_core::{
        contract::identity_registry::{PurgeAcknowledgement, PurgeReason},
        domain::identity::normalize_human_slug,
    };

    let (svc, vault_id) = setup_service_after_first_bind(dir).await;

    // Determine the slug that `init_defaults` will use.
    let slug = normalize_human_slug(&whoami::username());

    // Provision the v1 human.
    let v1_id = Identity::parse(format!("hmn:{slug}:v1")).expect("valid v1 id");
    let v1_input = ProvisionInput {
        vault_id,
        id: v1_id.clone(),
        kind: IdentityKind::Human,
        revision: IdentityRevision::FIRST,
    };
    svc.provision(IdentityKind::Human, v1_input, &mut rand_core::OsRng)
        .await
        .expect("provision v1 human");

    // Purge it directly: Active → PurgePending → Purged.
    let ack = PurgeAcknowledgement::for_test();
    let reason = PurgeReason("test purge".to_owned());
    svc.registry
        .mark_purge_pending(&v1_id, &ack, reason)
        .await
        .expect("mark_purge_pending");
    svc.registry
        .finalise_purge(&v1_id)
        .await
        .expect("finalise_purge");

    // Return svc with a fresh Arc-wrapped keystore (same underlying data).
    // We need to reconstruct from parts because setup_service_after_first_bind
    // returns owned IdentityService which we already used.
    // The svc still holds the same registry and keystore — just return it.
    (svc, slug)
}

// ── D5 tests ──────────────────────────────────────────────────────────────────

/// `init_defaults` on a fresh vault (no prior defaults) must provision v1 for
/// both the default human and the default agent, both in `Active` state.
#[tokio::test]
async fn init_defaults_creates_v1_when_none_exist() {
    use cairn_core::{
        contract::identity_registry::IdentityVisibility,
        domain::identity::{DefaultsState, normalize_human_slug},
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let (svc, _) = setup_service_after_first_bind(&dir).await;

    let result = svc
        .init_defaults(&mut rand_core::OsRng)
        .await
        .expect("init_defaults must succeed on a fresh vault");

    let DefaultsState::Active { human, agent } = result else {
        panic!("expected DefaultsState::Active");
    };

    // Human must be v1.
    let human_row = svc
        .registry
        .get_identity(&human, IdentityVisibility::Operational)
        .await
        .expect("get human identity")
        .expect("human must exist");
    assert_eq!(
        human_row.provisioning_state,
        ProvisioningState::Active,
        "default human at v1 must be Active",
    );
    let expected_slug = normalize_human_slug(&whoami::username());
    assert!(
        human.as_str().starts_with(&format!("hmn:{expected_slug}:")),
        "human id must embed the OS username slug, got: {}",
        human.as_str(),
    );

    // Agent must be v1.
    let agent_row = svc
        .registry
        .get_identity(&agent, IdentityVisibility::Operational)
        .await
        .expect("get agent identity")
        .expect("agent must exist");
    assert_eq!(
        agent_row.provisioning_state,
        ProvisioningState::Active,
        "default agent at v1 must be Active",
    );
    assert_eq!(
        agent.as_str(),
        "agt:claude-code:opus-4-7:main:v1",
        "default agent id must match the hardcoded triple",
    );
}

/// `init_defaults` must be idempotent: calling it a second time when both
/// defaults are already `Active` must return the same ids without touching
/// the registry.
#[tokio::test]
async fn init_defaults_is_idempotent_when_already_active() {
    use cairn_core::domain::identity::DefaultsState;

    let dir = tempfile::tempdir().expect("tempdir");
    let (svc, _) = setup_service_after_first_bind(&dir).await;

    let first = svc
        .init_defaults(&mut rand_core::OsRng)
        .await
        .expect("first call must succeed");
    let second = svc
        .init_defaults(&mut rand_core::OsRng)
        .await
        .expect("second call must succeed");

    let DefaultsState::Active {
        human: h1,
        agent: a1,
    } = first
    else {
        panic!("expected Active after first call");
    };
    let DefaultsState::Active {
        human: h2,
        agent: a2,
    } = second
    else {
        panic!("expected Active after second call");
    };
    assert_eq!(h1, h2, "human id must be stable across calls");
    assert_eq!(a1, a2, "agent id must be stable across calls");
}

/// `init_defaults` must mint v2 when the v1 default human has been purged.
///
/// Spec §3.5 revision-bump rule: if the highest-revision row is in a
/// terminal state (`Purged`), the next default identity is provisioned at
/// `current_rev + 1`.
#[tokio::test]
async fn init_defaults_mints_v2_after_v1_purged() {
    use cairn_core::{
        contract::identity_registry::IdentityVisibility, domain::identity::DefaultsState,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let (svc, slug) = setup_service_with_purged_default_human(&dir).await;

    let result = svc
        .init_defaults(&mut rand_core::OsRng)
        .await
        .expect("init_defaults must succeed after purge");

    // The returned human id must be at v2.
    let DefaultsState::Active { human, .. } = result else {
        panic!("expected DefaultsState::Active");
    };

    let human_row = svc
        .registry
        .get_identity(&human, IdentityVisibility::Operational)
        .await
        .expect("get_identity")
        .expect("v2 human must exist");

    assert_eq!(
        human_row.provisioning_state,
        ProvisioningState::Active,
        "v2 human must be Active",
    );

    // The identity string must embed the correct slug at v2.
    // (The DB does not store revision separately; it is encoded in the
    // identity string itself — derive it from the `:v<N>` suffix.)
    let expected_v2 = format!("hmn:{slug}:v2");
    assert_eq!(
        human.as_str(),
        expected_v2,
        "v2 human id must be hmn:{slug}:v2",
    );
}
