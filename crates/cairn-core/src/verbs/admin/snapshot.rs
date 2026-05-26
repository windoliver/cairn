//! `cairn.admin.v1` snapshot verb (outer-layer orchestrator).
//!
//! Orchestrates snapshot creation without performing any I/O itself. All
//! adapters are injected via traits so `cairn-core` stays I/O-free.

use crate::contract::admin_state::AdminStateStore;
use crate::contract::memory_store::StoreError;
use crate::contract::snapshot_artifact::{
    BackupRegistry, SnapshotArtifactProducer, SnapshotMetadataProvider,
};
use crate::domain::admin::{AdminContext, AdminError, AdminRole};
use crate::domain::backup::BackupRegistryEntry;
use crate::domain::{Rfc3339Timestamp, TargetId};
use crate::verbs::admin::manifest::{
    sha256_hex, IntegrityEnvelope, SnapshotManifest, MANIFEST_SCHEMA_VERSION,
};
use std::path::PathBuf;

/// Request to produce a snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotRequest {
    /// Directory into which the tarball will be written.
    pub out_dir: PathBuf,
    /// Optional human-readable label for the snapshot artifact.
    pub label: Option<String>,
    /// Locally-derived machine id (caller passes in — verb has no I/O).
    pub local_machine_id: String,
    /// `BackupRegistryEntry::backup_kind` value — typically `"snapshot"`.
    pub backup_kind: String,
}

/// Result produced by a successful snapshot run.
#[derive(Debug, Clone)]
pub struct SnapshotResponse {
    /// Unique backup identifier embedded in both the manifest and registry.
    pub backup_id: String,
    /// Filesystem path of the written tarball.
    pub artifact_path: PathBuf,
    /// Artifact-level sha256 digest (hex, 64 chars).
    pub sha256: String,
    /// WAL frontier step at snapshot time.
    pub frontier_step: String,
    /// The manifest that was sealed into the tarball.
    pub manifest: SnapshotManifest,
}

/// Run the snapshot verb. Pure orchestration over the trait dependencies.
///
/// # Errors
/// - [`AdminError::NotAuthorized`] if the caller lacks the [`AdminRole::Operator`] role.
/// - [`AdminError::Store`] if any adapter call fails.
pub fn run(
    ctx: &AdminContext,
    req: &SnapshotRequest,
    admin: &dyn AdminStateStore,
    meta: &dyn SnapshotMetadataProvider,
    producer: &dyn SnapshotArtifactProducer,
    registry: &dyn BackupRegistry,
) -> Result<SnapshotResponse, AdminError> {
    require_role(ctx, admin, AdminRole::Operator)?;

    let backup_id = generate_backup_id();
    let now = chrono::Utc::now();

    let manifest = SnapshotManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        backup_id: backup_id.clone(),
        created_at: now,
        source_machine_id: req.local_machine_id.clone(),
        source_vault_id: meta.vault_id().map_err(AdminError::Store)?,
        frontier_step: meta.frontier_step().map_err(AdminError::Store)?,
        record_count: meta.record_count().map_err(AdminError::Store)?,
        tombstone_count: meta.tombstone_count().map_err(AdminError::Store)?,
        schema_versions: meta.migration_heads().map_err(AdminError::Store)?,
        label: req.label.clone(),
    };

    let manifest_bytes = manifest
        .to_canonical_json()
        .map_err(|e| AdminError::Store(Box::new(e)))?;

    let materialized = producer
        .materialize(&req.out_dir, &backup_id, &manifest_bytes, req.label.as_deref())
        .map_err(AdminError::Store)?;

    let envelope = IntegrityEnvelope {
        manifest_sha256: sha256_hex(&manifest_bytes),
        db_sha256: materialized.db_sha256,
        tree_sha256: materialized.tree_sha256,
    };
    let sha = envelope.artifact_sha256();

    let entry = BackupRegistryEntry {
        backup_id: backup_id.clone(),
        created_at: rfc3339_from_chrono(now)?,
        artifact_path: materialized.path.display().to_string(),
        file_digest: sha.clone(),
        backup_kind: req.backup_kind.clone(),
        // Populated by callers that enumerate targets; out of scope for v0.2.
        target_ids_included: Vec::<TargetId>::new(),
    };
    registry.register(&entry).map_err(AdminError::Store)?;

    Ok(SnapshotResponse {
        backup_id,
        artifact_path: materialized.path,
        sha256: sha,
        frontier_step: manifest.frontier_step.clone(),
        manifest,
    })
}

fn require_role(
    ctx: &AdminContext,
    admin: &dyn AdminStateStore,
    needed: AdminRole,
) -> Result<(), AdminError> {
    if !admin.has_role(&ctx.actor, needed).map_err(AdminError::Store)? {
        return Err(AdminError::NotAuthorized {
            actor: ctx.actor.clone(),
            needed,
        });
    }
    Ok(())
}

/// Generate a unique backup id using a ULID, prefixed with `"snap-"`.
///
/// Two consecutive calls are guaranteed to produce different ids — ULID
/// encodes the millisecond timestamp in the high bits so monotonic time
/// alone separates them, with a random 80-bit suffix for sub-millisecond
/// uniqueness.
fn generate_backup_id() -> String {
    format!("snap-{}", ulid::Ulid::new())
}

/// Convert a `chrono::DateTime<Utc>` to the domain `Rfc3339Timestamp` wire type.
///
/// `chrono::DateTime::to_rfc3339` always produces a valid RFC3339 string, so
/// the `parse` call below cannot fail for any value produced at runtime —
/// only pre-1970 or out-of-range datetimes could fail, and those cannot come
/// from `Utc::now()`.
fn rfc3339_from_chrono(dt: chrono::DateTime<chrono::Utc>) -> Result<Rfc3339Timestamp, AdminError> {
    Rfc3339Timestamp::parse(dt.to_rfc3339())
        .map_err(|e| AdminError::Store(Box::new(e) as StoreError))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::contract::admin_state::{AdminStateStore, ConnectorStateRow};
    use crate::contract::snapshot_artifact::MaterializedArtifact;
    use crate::domain::Identity;
    use std::path::Path;
    use std::sync::Mutex;

    struct FakeAdmin {
        is_op: bool,
    }

    impl AdminStateStore for FakeAdmin {
        fn has_role(&self, _: &Identity, _: AdminRole) -> Result<bool, StoreError> {
            Ok(self.is_op)
        }

        fn has_any_operator(&self) -> Result<bool, StoreError> {
            Ok(self.is_op)
        }

        fn grant_role(
            &self,
            _: &Identity,
            _: AdminRole,
            _: &Identity,
        ) -> Result<(), StoreError> {
            Ok(())
        }

        fn revoke_role(&self, _: &Identity, _: AdminRole) -> Result<(), StoreError> {
            Ok(())
        }

        fn set_connector_enabled(
            &self,
            name: &str,
            enabled: bool,
            by: &Identity,
            reason: Option<&str>,
        ) -> Result<ConnectorStateRow, StoreError> {
            Ok(ConnectorStateRow::new(
                name.into(),
                enabled,
                chrono::Utc::now(),
                by.clone(),
                reason.map(str::to_string),
            ))
        }

        fn get_connector_state(
            &self,
            _: &str,
        ) -> Result<Option<ConnectorStateRow>, StoreError> {
            Ok(None)
        }

        fn list_connector_state(&self) -> Result<Vec<ConnectorStateRow>, StoreError> {
            Ok(Vec::new())
        }
    }

    struct FakeMeta;

    impl SnapshotMetadataProvider for FakeMeta {
        fn record_count(&self) -> Result<u64, StoreError> {
            Ok(42)
        }

        fn tombstone_count(&self) -> Result<u64, StoreError> {
            Ok(3)
        }

        fn frontier_step(&self) -> Result<String, StoreError> {
            Ok("step:7".into())
        }

        fn vault_id(&self) -> Result<String, StoreError> {
            Ok("vault-x".into())
        }

        fn migration_heads(
            &self,
        ) -> Result<std::collections::BTreeMap<String, u32>, StoreError> {
            Ok([("store".to_string(), 4u32)].into_iter().collect())
        }
    }

    struct FakeProducer;

    impl SnapshotArtifactProducer for FakeProducer {
        fn materialize(
            &self,
            out_dir: &Path,
            backup_id: &str,
            _manifest_bytes: &[u8],
            _label: Option<&str>,
        ) -> Result<MaterializedArtifact, StoreError> {
            // Fake: don't actually write — just return a synthetic path.
            let path = out_dir.join(format!("{backup_id}.cairn-snap.tar"));
            Ok(MaterializedArtifact {
                path,
                db_sha256: "deadbeef".into(),
                tree_sha256: "cafebabe".into(),
            })
        }
    }

    struct FakeRegistry {
        entries: Mutex<Vec<BackupRegistryEntry>>,
    }

    impl BackupRegistry for FakeRegistry {
        fn register(&self, entry: &BackupRegistryEntry) -> Result<(), StoreError> {
            self.entries
                .lock()
                .expect("test mutex poisoned")
                .push(entry.clone());
            Ok(())
        }
    }

    fn op_ctx() -> AdminContext {
        AdminContext::new(
            Identity::parse("hmn:op").expect("test fixture: known-valid identity"),
            AdminRole::Operator,
        )
    }

    fn default_req() -> SnapshotRequest {
        SnapshotRequest {
            out_dir: std::env::temp_dir(),
            label: None,
            local_machine_id: "MACHINE-A".into(),
            backup_kind: "snapshot".into(),
        }
    }

    fn fake_registry() -> FakeRegistry {
        FakeRegistry { entries: Mutex::new(Vec::new()) }
    }

    #[test]
    fn snapshot_happy_path_returns_envelope_and_registers() {
        let req = SnapshotRequest {
            label: Some("test".into()),
            ..default_req()
        };
        let admin = FakeAdmin { is_op: true };
        let meta = FakeMeta;
        let producer = FakeProducer;
        let registry = fake_registry();

        let resp = run(&op_ctx(), &req, &admin, &meta, &producer, &registry).expect("ok");

        assert_eq!(resp.manifest.record_count, 42);
        assert_eq!(resp.manifest.tombstone_count, 3);
        assert_eq!(resp.manifest.source_machine_id, "MACHINE-A");
        assert_eq!(resp.frontier_step, "step:7");
        assert_eq!(registry.entries.lock().expect("test mutex").len(), 1);
        // Artifact sha256 is a hex string of 64 characters.
        assert_eq!(resp.sha256.len(), 64);
    }

    #[test]
    fn snapshot_rejects_non_operator() {
        let req = default_req();
        let admin = FakeAdmin { is_op: false };
        let meta = FakeMeta;
        let producer = FakeProducer;
        let registry = fake_registry();

        let err =
            run(&op_ctx(), &req, &admin, &meta, &producer, &registry).unwrap_err();

        assert!(
            matches!(err, AdminError::NotAuthorized { .. }),
            "expected NotAuthorized, got {err:?}",
        );
        assert!(
            registry.entries.lock().expect("test mutex").is_empty(),
            "no registry write on auth failure",
        );
    }

    #[test]
    fn two_snapshots_get_different_backup_ids() {
        let req = default_req();
        let admin = FakeAdmin { is_op: true };
        let meta = FakeMeta;
        let producer = FakeProducer;
        let registry = fake_registry();

        let a = run(&op_ctx(), &req, &admin, &meta, &producer, &registry).expect("first");
        let b = run(&op_ctx(), &req, &admin, &meta, &producer, &registry).expect("second");

        assert_ne!(a.backup_id, b.backup_id, "backup ids must be unique");
    }
}
