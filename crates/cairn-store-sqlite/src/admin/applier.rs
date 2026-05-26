//! `SnapshotApplier` impl: untar to a staging directory, then atomically
//! rename `cairn.db` (and optionally `raw/`+`wiki/`) into the live vault.
//!
//! Two-phase contract:
//!   1. `stage(artifact_path, backup_id)` → unpack tarball to
//!      `<vault>/.cairn/restore-<backup_id>/`.
//!   2. `swap_in(staging_root)` → atomic `rename(staged/cairn.db, live/cairn.db)`,
//!      then rename `wiki/` and `raw/` if present in the staging root.

use cairn_core::contract::memory_store::StoreError;
use cairn_core::contract::snapshot_artifact::SnapshotApplier;
use std::path::{Path, PathBuf};

/// `SnapshotApplier` for a SQLite-backed vault stored under `vault_root`.
pub struct SqliteSnapshotApplier {
    vault_root: PathBuf,
}

impl SqliteSnapshotApplier {
    /// Construct the applier. `vault_root` is the top-level vault directory
    /// (parent of `raw/`, `wiki/`, `.cairn/cairn.db`).
    #[must_use]
    pub fn new(vault_root: PathBuf) -> Self {
        Self { vault_root }
    }
}

impl SnapshotApplier for SqliteSnapshotApplier {
    /// Unpack the tarball into a transient staging directory under
    /// `<vault>/.cairn/restore-<backup_id>/`. Returns the staging root for
    /// audit logging.
    ///
    /// If a staging directory from a prior (failed) attempt exists it is
    /// removed before extraction.
    fn stage(&self, artifact_path: &Path, backup_id: &str) -> Result<PathBuf, StoreError> {
        let staging = self
            .vault_root
            .join(".cairn")
            .join(format!("restore-{backup_id}"));
        if staging.exists() {
            std::fs::remove_dir_all(&staging).map_err(|e| Box::new(e) as StoreError)?;
        }
        std::fs::create_dir_all(&staging).map_err(|e| Box::new(e) as StoreError)?;

        let file = std::fs::File::open(artifact_path).map_err(|e| Box::new(e) as StoreError)?;
        let mut archive = tar::Archive::new(file);
        archive
            .unpack(&staging)
            .map_err(|e| Box::new(e) as StoreError)?;

        Ok(staging)
    }

    /// Atomically swap the staged contents into the live vault.
    ///
    /// Steps:
    ///   1. Rename `staging_root/cairn.db` → `<vault>/.cairn/cairn.db`.
    ///   2. For each of `wiki/` and `raw/`: if present in staging, remove
    ///      the live copy and rename the staged one in.
    ///   3. Remove the (now-empty) staging directory.
    ///
    /// # Errors
    /// Returns `StoreError` if `cairn.db` is absent from the staging root
    /// or any rename fails.
    fn swap_in(&self, staging_root: &Path) -> Result<(), StoreError> {
        let staged_db = staging_root.join("cairn.db");
        let live_cairn_dir = self.vault_root.join(".cairn");
        let live_db = live_cairn_dir.join("cairn.db");

        if !staged_db.exists() {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("staged cairn.db missing at {}", staged_db.display()),
            )) as StoreError);
        }

        std::fs::create_dir_all(&live_cairn_dir).map_err(|e| Box::new(e) as StoreError)?;

        // Atomic rename of the DB. On most POSIX filesystems this is a
        // single syscall when source and destination are on the same device.
        std::fs::rename(&staged_db, &live_db).map_err(|e| Box::new(e) as StoreError)?;

        // Restore markdown vault trees if present.
        for sub in ["wiki", "raw"] {
            let staged = staging_root.join(sub);
            let live = self.vault_root.join(sub);
            if staged.exists() {
                if live.exists() {
                    std::fs::remove_dir_all(&live).map_err(|e| Box::new(e) as StoreError)?;
                }
                std::fs::rename(&staged, &live).map_err(|e| Box::new(e) as StoreError)?;
            }
        }

        // Best-effort cleanup of the (now-mostly-empty) staging dir.
        let _ = std::fs::remove_dir_all(staging_root);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::producer::SqliteSnapshotProducer;
    use cairn_core::contract::snapshot_artifact::SnapshotArtifactProducer;

    #[test]
    fn stage_and_swap_in_roundtrip() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let vault_root = dir.path().join("vault");
        let cairn_dir = vault_root.join(".cairn");
        std::fs::create_dir_all(&cairn_dir).expect("cairn_dir");

        // Write a fake cairn.db.
        let db_path = cairn_dir.join("cairn.db");
        std::fs::write(&db_path, b"original-db").expect("write original db");

        let manifest_bytes = b"{\"schema_version\":1,\"backup_id\":\"apply-test\"}";
        let producer = SqliteSnapshotProducer::new(vault_root.clone(), db_path.clone());
        let out_dir = dir.path().join("snaps");
        let artifact = producer
            .materialize(&out_dir, "apply-test-001", manifest_bytes, None)
            .expect("materialize");

        // Overwrite the live DB to simulate post-snapshot mutations.
        std::fs::write(&db_path, b"mutated-db").expect("mutate db");

        let applier = SqliteSnapshotApplier::new(vault_root.clone());
        let staging = applier
            .stage(&artifact.path, "apply-test-001")
            .expect("stage");
        assert!(staging.exists(), "staging dir must exist after stage");

        applier.swap_in(&staging).expect("swap_in");

        // After swap, the live DB should be the snapshot's cairn.db
        // (which was `b"original-db"` at snapshot time).
        let restored = std::fs::read(&db_path).expect("read restored db");
        assert_eq!(
            restored, b"original-db" as &[u8],
            "live db after swap must equal the snapshot's db"
        );

        // Staging dir should be cleaned up.
        assert!(
            !staging.exists(),
            "staging dir must be removed after swap_in"
        );
    }
}
