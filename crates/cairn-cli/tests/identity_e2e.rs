//! §7 acceptance-matrix subset — 8 cross-component cases not already covered
//! by the Phase A-D unit/integration tests in `identity_provisioning.rs`.
//!
//! All tests use `MemoryKeystore` + in-memory `SqliteIdentityRegistry` +
//! `IdentityService::new_for_test`.  No real OS keychain is required.
//!
//! Cases covered:
//!   1. `bootstrap_init_defaults_happy_path`
//!   2. `provision_agent_identity_lands_active`
//!   3. `rotation_predecessor_key_superseded_at_is_set`
//!   4. `rotation_both_key_versions_accessible_in_keystore`
//!   5. `purge_after_rotation_evicts_all_key_versions`
//!   6. `revoke_by_third_party_signer`
//!   7. `key_material_desynchronized_raised_on_active_without_keystore_entry`
//!   8. `vault_no_plaintext_signing_key_bytes_on_disk`

use std::{fs, sync::Arc};

use cairn_core::{
    contract::{
        identity_registry::IdentityVisibility,
        keystore::KeystoreError,
    },
    domain::identity::{
        Identity, IdentityKind,
        keys::{IdentityRevision, KeyVersion, SecretHandle, VaultId},
        provision::{ProvisionInput, build_provisioning_plan},
        records::ProvisioningState,
    },
};
use cairn_store_sqlite::SqliteIdentityRegistry;
use cairn_test_fixtures::MemoryKeystore;

// ── shared helper ─────────────────────────────────────────────────────────────

/// Bootstrap a vault with `hmn:system:v1` as the first-bind identity and
/// return an `IdentityService` wired to the same in-memory registry + keystore.
async fn bootstrap_service(
    dir: &tempfile::TempDir,
) -> (cairn_cli::identity::IdentityService, VaultId) {
    let cairn_dir = dir.path().join(".cairn");
    fs::create_dir_all(&cairn_dir).expect("create .cairn");

    let registry = SqliteIdentityRegistry::open_in_memory().expect("in-memory registry");
    let vault_id = VaultId::mint();
    let keystore = MemoryKeystore::new();

    let sys_id = Identity::parse("hmn:system:v1").expect("valid");
    let plan = build_provisioning_plan(
        ProvisionInput {
            vault_id: vault_id.clone(),
            id: sys_id,
            kind: IdentityKind::Human,
            revision: IdentityRevision::FIRST,
        },
        &mut rand_core::OsRng,
        chrono::Utc::now(),
    );
    cairn_cli::identity::commit_first_identity(
        dir.path(),
        vault_id.clone(),
        plan,
        &registry,
        &keystore,
    )
    .await
    .expect("first-bind");

    fs::write(cairn_dir.join("vault.id"), vault_id.as_str()).expect("vault.id");

    let svc = cairn_cli::identity::IdentityService::new_for_test(
        dir.path().to_path_buf(),
        vault_id.clone(),
        Arc::new(registry),
        Arc::new(keystore),
    );
    (svc, vault_id)
}

/// Provision `id` through the service and return the `Identity`.
async fn provision(
    svc: &cairn_cli::identity::IdentityService,
    id_str: &str,
    kind: IdentityKind,
    vault_id: VaultId,
) -> Identity {
    let id = Identity::parse(id_str).expect("valid identity");
    svc.provision(
        kind,
        ProvisionInput {
            vault_id,
            id: id.clone(),
            kind,
            revision: IdentityRevision::FIRST,
        },
        &mut rand_core::OsRng,
    )
    .await
    .expect("provision");
    id
}

// ── Case 1 ────────────────────────────────────────────────────────────────────

/// Bootstrap → `init_defaults` happy path.
///
/// `init_defaults` on a freshly bootstrapped vault must provision a default
/// human and a default agent, both active, and return `DefaultsState::Active`.
///
/// Cross-component: service orchestrates `provision` for both identity kinds
/// and queries the registry to verify the result.
#[tokio::test]
async fn bootstrap_init_defaults_happy_path() {
    use cairn_core::domain::identity::DefaultsState;

    let dir = tempfile::tempdir().expect("tempdir");
    let (svc, _) = bootstrap_service(&dir).await;

    let result = svc
        .init_defaults(&mut rand_core::OsRng)
        .await
        .expect("init_defaults must succeed");

    let DefaultsState::Active { human, agent } = result else {
        panic!("expected DefaultsState::Active, got NotInitialised");
    };

    // Both must be Active in the registry.
    let human_row = svc
        .registry
        .get_identity(&human, IdentityVisibility::Operational)
        .await
        .expect("get human")
        .expect("human must exist");
    assert_eq!(
        human_row.provisioning_state,
        ProvisioningState::Active,
        "default human must be Active",
    );

    let agent_row = svc
        .registry
        .get_identity(&agent, IdentityVisibility::Operational)
        .await
        .expect("get agent")
        .expect("agent must exist");
    assert_eq!(
        agent_row.provisioning_state,
        ProvisioningState::Active,
        "default agent must be Active",
    );

    // Agent must have agt: prefix.
    assert!(
        agent.as_str().starts_with("agt:"),
        "default agent id must have agt: prefix, got: {}",
        agent.as_str(),
    );

    // Signing keys for both defaults must be stored in the keystore.
    let human_handle = SecretHandle::for_identity(
        svc.vault_id.clone(),
        human.clone(),
        human_row.current_key_version,
    );
    svc.keystore
        .load_signing_key(&human_handle)
        .await
        .expect("default human signing key must be in keystore");

    let agent_handle = SecretHandle::for_identity(
        svc.vault_id.clone(),
        agent.clone(),
        agent_row.current_key_version,
    );
    svc.keystore
        .load_signing_key(&agent_handle)
        .await
        .expect("default agent signing key must be in keystore");
}

// ── Case 2 ────────────────────────────────────────────────────────────────────

/// `provision` with `IdentityKind::Agent` must land the identity in `Active`
/// state with a stored signing key, just like `IdentityKind::Human`.
///
/// Agent identity strings use the `agt:harness:model:role:version` schema;
/// this case verifies that the provisioning path is not inadvertently
/// restricted to human identities only.
#[tokio::test]
async fn provision_agent_identity_lands_active() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (svc, vault_id) = bootstrap_service(&dir).await;

    let agent_id = provision(
        &svc,
        "agt:claude-code:opus-4-7:reviewer:v1",
        IdentityKind::Agent,
        vault_id.clone(),
    )
    .await;

    let row = svc
        .registry
        .get_identity(&agent_id, IdentityVisibility::Operational)
        .await
        .expect("get_identity")
        .expect("agent must exist after provision");
    assert_eq!(
        row.provisioning_state,
        ProvisioningState::Active,
        "provisioned agent must be Active",
    );

    // Signing key must be stored.
    let handle = SecretHandle::for_identity(vault_id, agent_id, row.current_key_version);
    svc.keystore
        .load_signing_key(&handle)
        .await
        .expect("agent signing key must be in keystore after provision");
}

// ── Case 3 ────────────────────────────────────────────────────────────────────

/// After one rotation, `list_keys` must show two rows for the identity:
/// - v1: `superseded_at` is `Some` (predecessor was superseded).
/// - v2: `superseded_at` is `None` (current, live key).
///
/// Verifies the two-phase rotation schema invariant from spec §3.6: the old
/// key row is retained for audit/history and marked superseded, not deleted.
#[tokio::test]
async fn rotation_predecessor_key_superseded_at_is_set() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (svc, vault_id) = bootstrap_service(&dir).await;

    let alice_id = provision(&svc, "hmn:alice:v1", IdentityKind::Human, vault_id).await;

    svc.rotate(&alice_id)
        .await
        .expect("rotate must succeed for active identity");

    let keys = svc
        .registry
        .list_keys(&alice_id)
        .await
        .expect("list_keys");

    assert_eq!(
        keys.len(),
        2,
        "exactly two key rows expected after one rotation; got: {keys:?}",
    );

    let v1 = keys
        .iter()
        .find(|k| k.key_version == KeyVersion::FIRST)
        .expect("v1 row must exist");
    let v2 = keys
        .iter()
        .find(|k| k.key_version == KeyVersion::FIRST.next().expect("v2"))
        .expect("v2 row must exist");

    assert!(
        v1.superseded_at.is_some(),
        "v1 key row must have superseded_at set after rotation, got: {v1:?}",
    );
    assert!(
        v2.superseded_at.is_none(),
        "v2 (current) key row must NOT have superseded_at set, got: {v2:?}",
    );
}

// ── Case 4 ────────────────────────────────────────────────────────────────────

/// After one rotation, both v1 and v2 signing keys must be loadable from the
/// keystore (no eviction occurs below `MAX_KEY_HISTORY = 3`).
///
/// The first rotation produces 2 key rows — below the eviction watermark —
/// so the keystore must still hold the v1 material for backward-compat
/// verification scenarios.
#[tokio::test]
async fn rotation_both_key_versions_accessible_in_keystore() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (svc, vault_id) = bootstrap_service(&dir).await;

    let bob_id = provision(&svc, "hmn:bob:v1", IdentityKind::Human, vault_id.clone()).await;

    // Capture v1 key version before rotation.
    let row_before = svc
        .registry
        .get_identity(&bob_id, IdentityVisibility::Operational)
        .await
        .expect("get before")
        .expect("bob must exist");
    assert_eq!(row_before.current_key_version, KeyVersion::FIRST);

    // Rotate.
    svc.rotate(&bob_id)
        .await
        .expect("rotate must succeed");

    // After rotation current key version is 2.
    let row_after = svc
        .registry
        .get_identity(&bob_id, IdentityVisibility::Operational)
        .await
        .expect("get after")
        .expect("bob must exist after rotation");
    let v2 = row_after.current_key_version;
    assert_eq!(v2.as_u32(), 2, "current key version must be 2 after rotation");

    // v1 is still accessible.
    let v1_handle = SecretHandle::for_identity(vault_id.clone(), bob_id.clone(), KeyVersion::FIRST);
    svc.keystore
        .load_signing_key(&v1_handle)
        .await
        .expect("v1 signing key must still be in keystore after first rotation (below eviction threshold)");

    // v2 is accessible.
    let v2_handle = SecretHandle::for_identity(vault_id, bob_id, v2);
    svc.keystore
        .load_signing_key(&v2_handle)
        .await
        .expect("v2 signing key must be in keystore after rotation");
}

// ── Case 5 ────────────────────────────────────────────────────────────────────

/// `purge` after a rotation must evict ALL key versions (v1 + v2) from the
/// keystore, not just the current one.
///
/// Verifies the full-sweep behaviour of `purge` — the spec §3.10 tombstone
/// must not leave orphan predecessor key material behind in the keystore.
#[tokio::test]
async fn purge_after_rotation_evicts_all_key_versions() {
    use cairn_core::contract::identity_registry::PurgeReason;

    let dir = tempfile::tempdir().expect("tempdir");
    let (svc, vault_id) = bootstrap_service(&dir).await;

    let carol_id = provision(&svc, "hmn:carol:v1", IdentityKind::Human, vault_id.clone()).await;

    // Rotate once so carol has two key versions.
    svc.rotate(&carol_id)
        .await
        .expect("rotate");

    let row = svc
        .registry
        .get_identity(&carol_id, IdentityVisibility::Operational)
        .await
        .expect("get")
        .expect("carol must exist");
    let v2 = row.current_key_version;

    // Write the purge-ack file (required by spec §3.10).
    let maintenance_dir = dir.path().join(".cairn/maintenance");
    fs::create_dir_all(&maintenance_dir).expect("create maintenance dir");
    fs::write(maintenance_dir.join("purge-ack"), carol_id.as_str())
        .expect("write purge-ack");

    // Purge carol.
    svc.purge(&carol_id, PurgeReason("GDPR erasure".to_owned()))
        .await
        .expect("purge must succeed");

    // State must be Purged (visible at Audit).
    let row_after = svc
        .registry
        .get_identity(&carol_id, IdentityVisibility::Audit)
        .await
        .expect("get after purge")
        .expect("purged row must still be visible at Audit");
    assert_eq!(
        row_after.provisioning_state,
        ProvisioningState::Purged,
        "carol must be Purged after purge()",
    );

    // v1 key must be gone from keystore.
    let v1_handle = SecretHandle::for_identity(vault_id.clone(), carol_id.clone(), KeyVersion::FIRST);
    assert!(
        matches!(
            svc.keystore.load_signing_key(&v1_handle).await,
            Err(KeystoreError::NotFound)
        ),
        "v1 signing key must be evicted from keystore after purge",
    );

    // v2 key must also be gone.
    let v2_handle = SecretHandle::for_identity(vault_id, carol_id, v2);
    assert!(
        matches!(
            svc.keystore.load_signing_key(&v2_handle).await,
            Err(KeystoreError::NotFound)
        ),
        "v2 signing key must also be evicted from keystore after purge",
    );
}

// ── Case 6 ────────────────────────────────────────────────────────────────────

/// `revoke` with `signer != target` (third-party revocation) must succeed and
/// leave the target in `Revoked` state with its signing key evicted.
///
/// The two existing revoke tests only exercise self-revocation (`signer == target`).
/// This case verifies the code path where a separate signer (here `moderator`)
/// triggers revocation of another active identity.
#[tokio::test]
async fn revoke_by_third_party_signer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (svc, vault_id) = bootstrap_service(&dir).await;

    // Provision both identities.
    let alice_id = provision(&svc, "hmn:alice:v1", IdentityKind::Human, vault_id.clone()).await;
    let moderator_id =
        provision(&svc, "hmn:moderator:v1", IdentityKind::Human, vault_id).await;

    // Third-party revocation: moderator revokes alice.
    svc.revoke(&alice_id, &moderator_id)
        .await
        .expect("third-party revoke must succeed");

    let row = svc
        .registry
        .get_identity(&alice_id, IdentityVisibility::Operational)
        .await
        .expect("get_identity")
        .expect("alice must still be visible at Operational after revocation");

    assert_eq!(
        row.provisioning_state,
        ProvisioningState::Revoked,
        "alice must be Revoked after third-party revocation",
    );

    // Alice's signing key must be evicted.
    let handle =
        SecretHandle::for_identity(svc.vault_id.clone(), alice_id, row.current_key_version);
    assert!(
        matches!(
            svc.keystore.load_signing_key(&handle).await,
            Err(KeystoreError::NotFound)
        ),
        "alice's signing key must be evicted after third-party revocation",
    );
}

// ── Case 7 ────────────────────────────────────────────────────────────────────

/// `status_report` must surface `desynchronized_active_ids` when an active
/// identity has a registry row but its keystore entry has been deleted.
///
/// This is the "active-desync" branch of spec §4.5: the identity is Active in
/// the registry, but `load_signing_key` returns `NotFound`.  The condition is
/// non-vault-degrading (no mismatch, just missing) but must appear in the
/// report so operators can detect and repair key material loss.
#[tokio::test]
async fn key_material_desynchronized_raised_on_active_without_keystore_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (svc, vault_id) = bootstrap_service(&dir).await;

    let target_id = provision(&svc, "hmn:desynced:v1", IdentityKind::Human, vault_id.clone()).await;

    // Verify the identity is active.
    let row = svc
        .registry
        .get_identity(&target_id, IdentityVisibility::Operational)
        .await
        .expect("get_identity")
        .expect("target must exist");
    assert_eq!(row.provisioning_state, ProvisioningState::Active);

    // Delete the keystore entry — simulates key-material loss without
    // touching the registry (desync, not mismatch).
    let handle = SecretHandle::for_identity(vault_id, target_id.clone(), row.current_key_version);
    svc.keystore
        .delete_keypair(&handle)
        .await
        .expect("delete_keypair to simulate desync");

    // status_report must surface the desync.
    let report = svc
        .status_report()
        .await
        .expect("status_report must not error");

    assert!(
        report.desynchronized_active_ids.contains(&target_id),
        "desynchronized_active_ids must contain the identity whose keystore entry was deleted; \
         got: {:?}",
        report.desynchronized_active_ids,
    );

    // A desync is NOT vault-degrading on its own (no pubkey mismatch).
    // The mismatched_ids list must not include the target.
    assert!(
        !report.mismatched_ids.contains(&target_id),
        "desync must NOT appear in mismatched_ids (no pubkey mismatch); \
         mismatched_ids: {:?}",
        report.mismatched_ids,
    );
}

// ── Case 8 ────────────────────────────────────────────────────────────────────

/// After bootstrapping + provisioning, the raw Ed25519 private key bytes must
/// NOT appear in any file under `.cairn/`.
///
/// Privacy by construction (brief §14): signing key material lives in the
/// keystore adapter, never in the markdown vault or in config files.
/// `MemoryKeystore` never writes to disk, so this check validates that
/// `commit_first_identity` doesn't accidentally spill key bytes through any
/// other code path (config writes, WAL, binding sentinels, etc.).
///
/// Method: capture the exact private key bytes from the plan before the commit,
/// then scan all files under `<vault>/.cairn/` and assert none contain those bytes.
#[tokio::test]
async fn vault_no_plaintext_signing_key_bytes_on_disk() {
    use cairn_core::domain::identity::{
        keys::{IdentityRevision, VaultId},
        provision::{ProvisionInput, build_provisioning_plan},
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let cairn_dir = dir.path().join(".cairn");
    fs::create_dir_all(&cairn_dir).expect("create .cairn");

    let registry = SqliteIdentityRegistry::open_in_memory().expect("in-memory registry");
    let vault_id = VaultId::mint();
    let keystore = MemoryKeystore::new();

    let id = Identity::parse("hmn:privacy-test:v1").expect("valid");
    let plan = build_provisioning_plan(
        ProvisionInput {
            vault_id: vault_id.clone(),
            id,
            kind: IdentityKind::Human,
            revision: IdentityRevision::FIRST,
        },
        &mut rand_core::OsRng,
        chrono::Utc::now(),
    );

    // Capture the raw private key bytes BEFORE committing so we know exactly
    // what bytes to look for on disk.
    let private_key_bytes: Vec<u8> = plan.signing_key.expose_secret_bytes().to_vec();
    assert_eq!(
        private_key_bytes.len(),
        32,
        "Ed25519 private key must be 32 bytes",
    );

    // Also capture the public key bytes — we don't expect those to be hidden,
    // but we need to distinguish them from the private key in the scan.
    let public_key_bytes: Vec<u8> = plan.signing_key.verifying_key().to_bytes().to_vec();

    cairn_cli::identity::commit_first_identity(
        dir.path(),
        vault_id.clone(),
        plan,
        &registry,
        &keystore,
    )
    .await
    .expect("commit_first_identity");

    // Walk .cairn/ and check that no file contains the private key bytes.
    let mut offenders: Vec<String> = Vec::new();
    for entry in walkdir(&cairn_dir) {
        if !entry.is_file() {
            continue;
        }
        let Ok(bytes) = fs::read(&entry) else {
            continue;
        };
        if bytes_contains_subsequence(&bytes, &private_key_bytes) {
            // Extra check: rule out an incidental collision with the public key
            // (which is derived and may be written to disk for audit).
            if !bytes_contains_subsequence(&bytes, &public_key_bytes) || bytes != private_key_bytes
            {
                offenders.push(format!("{}", entry.display()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "Private key bytes found on disk under .cairn/ — signing key must \
         stay in keystore only (brief §14):\n{}",
        offenders.join("\n"),
    );
}

/// Minimal recursive directory walker returning file paths under `root`.
fn walkdir(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return result;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            result.extend(walkdir(&p));
        } else {
            result.push(p);
        }
    }
    result
}

/// Returns `true` if `haystack` contains `needle` as a contiguous
/// byte subsequence.
fn bytes_contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}
