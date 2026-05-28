//! `cairn.admin.v1` restore verb (outer-layer orchestrator with precondition gate).

use crate::contract::admin_state::AdminStateStore;
use crate::contract::memory_store::StoreError;
use crate::contract::snapshot_artifact::{
    ConsentLog, SnapshotApplier, SnapshotArtifactReader, SnapshotMetadataProvider,
};
use crate::domain::admin::{AdminContext, AdminError, AdminRole};
use crate::verbs::admin::manifest::{MANIFEST_SCHEMA_VERSION, SnapshotManifest, sha256_hex};
use std::path::PathBuf;

/// Request to restore a snapshot artifact into the local vault.
#[derive(Debug, Clone)]
pub struct RestoreRequest {
    /// Filesystem path to the `.cairn-snap.tar` artifact.
    pub artifact_path: PathBuf,
    /// When `true`, validate all preconditions and return stats without
    /// actually applying the restore.
    pub dry_run: bool,
    /// Local machine id — the caller passes this in; the verb has no I/O.
    pub local_machine_id: String,
}

/// Result produced by a successful restore run.
#[derive(Debug, Clone)]
pub struct RestoreResponse {
    /// Number of records in the restored artifact.
    pub restored_records: u64,
    /// Number of forget tombstones replayed post-restore.
    pub tombstones_replayed: u64,
    /// WAL frontier step from the live vault after restore.
    pub frontier_step: String,
    /// Backup identifier embedded in the manifest.
    pub backup_id: String,
}

/// Run the restore verb. Pure orchestration; all I/O behind trait deps.
///
/// # Precondition gate (ordered, fail-closed per §6.4)
/// 1. `manifest.schema_version ∈ {1}` else [`AdminError::SchemaTooNew`].
/// 2. `manifest.source_machine_id == req.local_machine_id` else [`AdminError::CrossMachineRestore`].
/// 3. `manifest.source_vault_id == local_vault_id` else [`AdminError::VaultIdMismatch`].
/// 4. Each `manifest.schema_versions[k] ≤ local_heads[k]` else [`AdminError::SchemaTooNew`].
/// 5. Integrity envelope verified else [`AdminError::IntegrityMismatch`].
///
/// # Errors
/// - [`AdminError::NotAuthorized`] — caller lacks operator role.
/// - [`AdminError::SchemaTooNew`] — manifest format too new OR component head too new.
/// - [`AdminError::CrossMachineRestore`] — machine-id mismatch.
/// - [`AdminError::VaultIdMismatch`] — vault-id mismatch.
/// - [`AdminError::IntegrityMismatch`] — recomputed envelope disagrees with the canonical manifest sha.
/// - [`AdminError::Store`] — adapter failure.
pub fn run(
    ctx: &AdminContext,
    req: &RestoreRequest,
    admin: &dyn AdminStateStore,
    meta: &dyn SnapshotMetadataProvider,
    reader: &dyn SnapshotArtifactReader,
    applier: &dyn SnapshotApplier,
    consent: &dyn ConsentLog,
) -> Result<RestoreResponse, AdminError> {
    super::guard::require_role(ctx, admin, AdminRole::Operator)?;

    let manifest = reader
        .read_manifest(&req.artifact_path)
        .map_err(AdminError::Store)?;

    precondition_gate(&manifest, req, meta, reader, &req.artifact_path)?;

    if req.dry_run {
        return Ok(RestoreResponse {
            restored_records: manifest.record_count,
            tombstones_replayed: 0,
            frontier_step: manifest.frontier_step.clone(),
            backup_id: manifest.backup_id,
        });
    }

    // Pre-capture forget hashes from the LIVE DB before swap_in() overwrites
    // the live DB path with the restored DB (finding #161-R2-4 fix). The
    // ConsentLog adapter caches these hashes so apply_post_restore_purge()
    // can use them even when live_db == restored_db (MCP path).
    consent
        .forgotten_record_target_hashes()
        .map_err(AdminError::Store)?;

    let staging = applier
        .stage(&req.artifact_path, &manifest.backup_id)
        .map_err(AdminError::Store)?;
    applier.swap_in(&staging).map_err(AdminError::Store)?;
    let tombstones = consent
        .apply_post_restore_purge()
        .map_err(AdminError::Store)?;
    let frontier = meta.frontier_step().map_err(AdminError::Store)?;

    Ok(RestoreResponse {
        restored_records: manifest.record_count,
        tombstones_replayed: tombstones,
        frontier_step: frontier,
        backup_id: manifest.backup_id,
    })
}

fn precondition_gate(
    m: &SnapshotManifest,
    req: &RestoreRequest,
    meta: &dyn SnapshotMetadataProvider,
    reader: &dyn SnapshotArtifactReader,
    artifact_path: &std::path::Path,
) -> Result<(), AdminError> {
    // Gate 1: manifest schema version.
    if m.schema_version > MANIFEST_SCHEMA_VERSION {
        return Err(AdminError::SchemaTooNew {
            snapshot: m.schema_version,
            local: MANIFEST_SCHEMA_VERSION,
        });
    }

    // Gate 2: machine-id guard.
    if m.source_machine_id != req.local_machine_id {
        return Err(AdminError::CrossMachineRestore {
            snapshot: m.source_machine_id.clone(),
            local: req.local_machine_id.clone(),
        });
    }

    // Gate 3: vault-id guard.
    let local_vault = meta.vault_id().map_err(AdminError::Store)?;
    if m.source_vault_id != local_vault {
        return Err(AdminError::VaultIdMismatch {
            snapshot: m.source_vault_id.clone(),
            local: local_vault,
        });
    }

    // Gate 4: per-component schema version guard.
    let local_heads = meta.migration_heads().map_err(AdminError::Store)?;
    for (component, ver) in &m.schema_versions {
        if let Some(local_ver) = local_heads.get(component)
            && ver > local_ver
        {
            return Err(AdminError::SchemaTooNew {
                snapshot: *ver,
                local: *local_ver,
            });
        }
    }

    // Gate 5: integrity envelope.
    let envelope = reader
        .read_envelope(artifact_path)
        .map_err(AdminError::Store)?;
    let manifest_bytes = m
        .to_canonical_json()
        .map_err(|e| AdminError::Store(Box::new(e) as StoreError))?;
    let expected_manifest_sha = sha256_hex(&manifest_bytes);
    if envelope.manifest_sha256 != expected_manifest_sha {
        return Err(AdminError::IntegrityMismatch {
            expected: expected_manifest_sha,
            actual: envelope.manifest_sha256,
        });
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::contract::admin_state::ConnectorStateRow;
    use crate::contract::snapshot_artifact::{
        ConsentLog as ConsentLogTrait, SnapshotApplier as ApplierT,
        SnapshotArtifactReader as ReaderT, SnapshotMetadataProvider as MetaT,
    };
    use crate::domain::Identity;
    use crate::verbs::admin::manifest::{
        IntegrityEnvelope, MANIFEST_SCHEMA_VERSION, SnapshotManifest,
    };
    use std::collections::{BTreeMap, HashSet};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    fn fixture_manifest(machine_id: &str, vault_id: &str) -> SnapshotManifest {
        SnapshotManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            backup_id: "snap-test".into(),
            created_at: chrono::Utc::now(),
            source_machine_id: machine_id.into(),
            source_vault_id: vault_id.into(),
            frontier_step: "step:1".into(),
            record_count: 10,
            tombstone_count: 0,
            schema_versions: [("store".to_string(), 4u32)].into_iter().collect(),
            label: None,
        }
    }

    struct StubAdmin {
        is_op: bool,
    }

    impl crate::contract::admin_state::AdminStateStore for StubAdmin {
        fn has_role(&self, _: &Identity, _: AdminRole) -> Result<bool, StoreError> {
            Ok(self.is_op)
        }

        fn has_any_operator(&self) -> Result<bool, StoreError> {
            Ok(self.is_op)
        }

        fn grant_role(&self, _: &Identity, _: AdminRole, _: &Identity) -> Result<(), StoreError> {
            Ok(())
        }

        fn revoke_role(&self, _: &Identity, _: AdminRole) -> Result<(), StoreError> {
            Ok(())
        }

        fn set_connector_enabled(
            &self,
            name: &str,
            en: bool,
            by: &Identity,
            reason: Option<&str>,
        ) -> Result<ConnectorStateRow, StoreError> {
            Ok(ConnectorStateRow::new(
                name.into(),
                en,
                chrono::Utc::now(),
                by.clone(),
                reason.map(str::to_string),
            ))
        }

        fn get_connector_state(&self, _: &str) -> Result<Option<ConnectorStateRow>, StoreError> {
            Ok(None)
        }

        fn list_connector_state(&self) -> Result<Vec<ConnectorStateRow>, StoreError> {
            Ok(Vec::new())
        }
    }

    struct StubMeta {
        vault: String,
        heads: BTreeMap<String, u32>,
        frontier: String,
    }

    impl MetaT for StubMeta {
        fn record_count(&self) -> Result<u64, StoreError> {
            Ok(0)
        }

        fn tombstone_count(&self) -> Result<u64, StoreError> {
            Ok(0)
        }

        fn frontier_step(&self) -> Result<String, StoreError> {
            Ok(self.frontier.clone())
        }

        fn vault_id(&self) -> Result<String, StoreError> {
            Ok(self.vault.clone())
        }

        fn migration_heads(&self) -> Result<BTreeMap<String, u32>, StoreError> {
            Ok(self.heads.clone())
        }
    }

    struct StubReader {
        manifest: SnapshotManifest,
        envelope: IntegrityEnvelope,
    }

    impl ReaderT for StubReader {
        fn read_manifest(&self, _: &Path) -> Result<SnapshotManifest, StoreError> {
            Ok(self.manifest.clone())
        }

        fn read_envelope(&self, _: &Path) -> Result<IntegrityEnvelope, StoreError> {
            Ok(self.envelope.clone())
        }
    }

    struct StubApplier {
        swapped: Arc<Mutex<bool>>,
        staged: Arc<Mutex<bool>>,
    }

    impl ApplierT for StubApplier {
        fn stage(&self, _: &Path, _: &str) -> Result<PathBuf, StoreError> {
            *self.staged.lock().expect("test mutex: staged") = true;
            Ok(PathBuf::from("/tmp/stage"))
        }

        fn swap_in(&self, _: &Path) -> Result<(), StoreError> {
            *self.swapped.lock().expect("test mutex: swapped") = true;
            Ok(())
        }
    }

    struct StubConsent {
        purged: u64,
    }

    impl ConsentLogTrait for StubConsent {
        fn forgotten_record_target_hashes(&self) -> Result<HashSet<String>, StoreError> {
            Ok(HashSet::new())
        }

        fn apply_post_restore_purge(&self) -> Result<u64, StoreError> {
            Ok(self.purged)
        }
    }

    fn op_ctx() -> AdminContext {
        AdminContext::new(
            Identity::parse("hmn:op").expect("test fixture: known-valid identity"),
            AdminRole::Operator,
        )
    }

    fn good_envelope_for(m: &SnapshotManifest) -> IntegrityEnvelope {
        let bytes = m
            .to_canonical_json()
            .expect("test fixture: canonical json must succeed");
        IntegrityEnvelope {
            manifest_sha256: sha256_hex(&bytes),
            db_sha256: "deadbeef".into(),
            tree_sha256: "cafebabe".into(),
        }
    }

    fn default_applier() -> StubApplier {
        StubApplier {
            swapped: Arc::new(Mutex::new(false)),
            staged: Arc::new(Mutex::new(false)),
        }
    }

    #[test]
    fn restore_rejects_non_operator() {
        let m = fixture_manifest("M", "V");
        let env = good_envelope_for(&m);
        let req = RestoreRequest {
            artifact_path: PathBuf::from("/tmp/x"),
            dry_run: false,
            local_machine_id: "M".into(),
        };
        let admin = StubAdmin { is_op: false };
        let meta = StubMeta {
            vault: "V".into(),
            heads: m.schema_versions.clone(),
            frontier: "step:9".into(),
        };
        let reader = StubReader {
            manifest: m,
            envelope: env,
        };
        let applier = default_applier();
        let consent = StubConsent { purged: 0 };

        let err = run(&op_ctx(), &req, &admin, &meta, &reader, &applier, &consent).unwrap_err();
        assert!(
            matches!(err, AdminError::NotAuthorized { .. }),
            "expected NotAuthorized, got {err:?}",
        );
        assert!(
            !*applier.swapped.lock().expect("test mutex"),
            "no swap on auth failure",
        );
    }

    #[test]
    fn cross_machine_refused() {
        let m = fixture_manifest("MACHINE-A", "V");
        let env = good_envelope_for(&m);
        let req = RestoreRequest {
            artifact_path: PathBuf::from("/tmp/x"),
            dry_run: false,
            local_machine_id: "MACHINE-B".into(),
        };
        let admin = StubAdmin { is_op: true };
        let meta = StubMeta {
            vault: "V".into(),
            heads: m.schema_versions.clone(),
            frontier: "step:9".into(),
        };
        let reader = StubReader {
            manifest: m,
            envelope: env,
        };
        let applier = default_applier();
        let consent = StubConsent { purged: 0 };

        let err = run(&op_ctx(), &req, &admin, &meta, &reader, &applier, &consent).unwrap_err();
        assert!(
            matches!(
                err,
                AdminError::CrossMachineRestore { ref snapshot, ref local }
                    if snapshot == "MACHINE-A" && local == "MACHINE-B"
            ),
            "expected CrossMachineRestore, got {err:?}",
        );
        assert!(
            !*applier.swapped.lock().expect("test mutex"),
            "no swap on gate failure",
        );
    }

    #[test]
    fn vault_mismatch_refused() {
        let m = fixture_manifest("M", "VAULT-A");
        let env = good_envelope_for(&m);
        let req = RestoreRequest {
            artifact_path: PathBuf::from("/tmp/x"),
            dry_run: false,
            local_machine_id: "M".into(),
        };
        let admin = StubAdmin { is_op: true };
        let meta = StubMeta {
            vault: "VAULT-B".into(),
            heads: m.schema_versions.clone(),
            frontier: "step:9".into(),
        };
        let reader = StubReader {
            manifest: m,
            envelope: env,
        };
        let applier = default_applier();
        let consent = StubConsent { purged: 0 };

        let err = run(&op_ctx(), &req, &admin, &meta, &reader, &applier, &consent).unwrap_err();
        assert!(
            matches!(
                err,
                AdminError::VaultIdMismatch { ref snapshot, ref local }
                    if snapshot == "VAULT-A" && local == "VAULT-B"
            ),
            "expected VaultIdMismatch, got {err:?}",
        );
    }

    #[test]
    fn schema_too_new_refused() {
        let mut m = fixture_manifest("M", "V");
        m.schema_versions.insert("store".into(), 99); // local is 4
        // Re-compute envelope with updated manifest.
        let env = good_envelope_for(&m);
        let req = RestoreRequest {
            artifact_path: PathBuf::from("/tmp/x"),
            dry_run: false,
            local_machine_id: "M".into(),
        };
        let admin = StubAdmin { is_op: true };
        let meta = StubMeta {
            vault: "V".into(),
            heads: [("store".to_string(), 4u32)].into_iter().collect(),
            frontier: "step:9".into(),
        };
        let reader = StubReader {
            manifest: m,
            envelope: env,
        };
        let applier = default_applier();
        let consent = StubConsent { purged: 0 };

        let err = run(&op_ctx(), &req, &admin, &meta, &reader, &applier, &consent).unwrap_err();
        assert!(
            matches!(
                err,
                AdminError::SchemaTooNew {
                    snapshot: 99,
                    local: 4
                }
            ),
            "expected SchemaTooNew(99, 4), got {err:?}",
        );
    }

    #[test]
    fn integrity_mismatch_refused() {
        let m = fixture_manifest("M", "V");
        let env = IntegrityEnvelope {
            manifest_sha256: "wrong-hash".into(),
            db_sha256: "d".into(),
            tree_sha256: "t".into(),
        };
        let req = RestoreRequest {
            artifact_path: PathBuf::from("/tmp/x"),
            dry_run: false,
            local_machine_id: "M".into(),
        };
        let admin = StubAdmin { is_op: true };
        let meta = StubMeta {
            vault: "V".into(),
            heads: m.schema_versions.clone(),
            frontier: "step:9".into(),
        };
        let reader = StubReader {
            manifest: m,
            envelope: env,
        };
        let applier = default_applier();
        let consent = StubConsent { purged: 0 };

        let err = run(&op_ctx(), &req, &admin, &meta, &reader, &applier, &consent).unwrap_err();
        assert!(
            matches!(err, AdminError::IntegrityMismatch { .. }),
            "expected IntegrityMismatch, got {err:?}",
        );
    }

    #[test]
    fn dry_run_skips_swap() {
        let m = fixture_manifest("M", "V");
        let env = good_envelope_for(&m);
        let req = RestoreRequest {
            artifact_path: PathBuf::from("/tmp/x"),
            dry_run: true,
            local_machine_id: "M".into(),
        };
        let admin = StubAdmin { is_op: true };
        let meta = StubMeta {
            vault: "V".into(),
            heads: m.schema_versions.clone(),
            frontier: "step:9".into(),
        };
        let reader = StubReader {
            manifest: m.clone(),
            envelope: env,
        };
        let applier = default_applier();
        let consent = StubConsent { purged: 0 };

        let resp = run(&op_ctx(), &req, &admin, &meta, &reader, &applier, &consent).unwrap();
        assert!(
            !*applier.swapped.lock().expect("test mutex"),
            "dry-run never swaps",
        );
        assert_eq!(resp.tombstones_replayed, 0);
        assert_eq!(resp.restored_records, m.record_count);
    }

    #[test]
    fn happy_path_swaps_and_purges() {
        let m = fixture_manifest("M", "V");
        let env = good_envelope_for(&m);
        let req = RestoreRequest {
            artifact_path: PathBuf::from("/tmp/x"),
            dry_run: false,
            local_machine_id: "M".into(),
        };
        let admin = StubAdmin { is_op: true };
        let meta = StubMeta {
            vault: "V".into(),
            heads: m.schema_versions.clone(),
            frontier: "step:99".into(),
        };
        let reader = StubReader {
            manifest: m,
            envelope: env,
        };
        let applier = default_applier();
        let consent = StubConsent { purged: 3 };

        let resp = run(&op_ctx(), &req, &admin, &meta, &reader, &applier, &consent).unwrap();
        assert!(
            *applier.staged.lock().expect("test mutex"),
            "stage must have been called",
        );
        assert!(
            *applier.swapped.lock().expect("test mutex"),
            "swap_in must have been called",
        );
        assert_eq!(resp.tombstones_replayed, 3);
        assert_eq!(resp.frontier_step, "step:99");
    }

    #[test]
    fn manifest_schema_too_new_refused() {
        let mut m = fixture_manifest("M", "V");
        m.schema_version = MANIFEST_SCHEMA_VERSION + 1;
        // Envelope doesn't matter — gate fires before integrity check.
        let env = IntegrityEnvelope {
            manifest_sha256: "any".into(),
            db_sha256: "any".into(),
            tree_sha256: "any".into(),
        };
        let req = RestoreRequest {
            artifact_path: PathBuf::from("/tmp/x"),
            dry_run: false,
            local_machine_id: "M".into(),
        };
        let admin = StubAdmin { is_op: true };
        let meta = StubMeta {
            vault: "V".into(),
            heads: m.schema_versions.clone(),
            frontier: "step:9".into(),
        };
        let reader = StubReader {
            manifest: m,
            envelope: env,
        };
        let applier = default_applier();
        let consent = StubConsent { purged: 0 };

        let err = run(&op_ctx(), &req, &admin, &meta, &reader, &applier, &consent).unwrap_err();
        assert!(
            matches!(
                err,
                AdminError::SchemaTooNew {
                    snapshot,
                    local
                } if snapshot == MANIFEST_SCHEMA_VERSION + 1 && local == MANIFEST_SCHEMA_VERSION
            ),
            "expected SchemaTooNew for manifest version, got {err:?}",
        );
    }
}
