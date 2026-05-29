//! `BackupRegistry` impl that writes one JSON file per entry under
//! `<vault>/.cairn/backups/<backup_id>.json`.
//!
//! The directory is created on first `register` call. Entries are
//! idempotent by `backup_id`: re-registering the same id overwrites the
//! existing file with the same content (the `BackupRegistryEntry` is
//! immutable once sealed by the caller).

use cairn_core::contract::memory_store::StoreError;
use cairn_core::contract::snapshot_artifact::BackupRegistry;
use cairn_core::domain::backup::BackupRegistryEntry;
use std::path::PathBuf;

/// `BackupRegistry` that persists each `BackupRegistryEntry` as a
/// pretty-printed JSON file under `<vault>/.cairn/backups/`.
pub struct FileBackupRegistry {
    vault_root: PathBuf,
}

impl FileBackupRegistry {
    /// Construct the registry. `vault_root` is the top-level vault directory.
    #[must_use]
    pub fn new(vault_root: PathBuf) -> Self {
        Self { vault_root }
    }

    fn dir(&self) -> PathBuf {
        self.vault_root.join(".cairn").join("backups")
    }
}

impl BackupRegistry for FileBackupRegistry {
    /// Persist `entry` as `<vault>/.cairn/backups/<backup_id>.json`.
    ///
    /// The parent directory is created if absent. Writing is atomic from
    /// the perspective of the caller: `serde_json::to_vec_pretty` is
    /// infallible for well-formed domain types, and `fs::write` is a
    /// single syscall on POSIX.
    fn register(&self, entry: &BackupRegistryEntry) -> Result<(), StoreError> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir).map_err(|e| Box::new(e) as StoreError)?;
        let path = dir.join(format!("{}.json", entry.backup_id));
        let bytes = serde_json::to_vec_pretty(entry).map_err(|e| Box::new(e) as StoreError)?;
        std::fs::write(&path, bytes).map_err(|e| Box::new(e) as StoreError)?;
        Ok(())
    }

    /// Read `<vault>/.cairn/backups/<backup_id>.json` and parse it.
    ///
    /// Returns `Ok(None)` when the file is absent (no entry for that id). A
    /// missing-file error is the only error mapped to `None`; malformed JSON
    /// or other I/O errors propagate so a corrupt registry fails loudly rather
    /// than silently degrading the restore-time integrity check.
    fn lookup(&self, backup_id: &str) -> Result<Option<BackupRegistryEntry>, StoreError> {
        let path = self.dir().join(format!("{backup_id}.json"));
        match std::fs::read(&path) {
            Ok(bytes) => {
                let entry: BackupRegistryEntry =
                    serde_json::from_slice(&bytes).map_err(|e| Box::new(e) as StoreError)?;
                Ok(Some(entry))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Box::new(e) as StoreError),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::domain::Rfc3339Timestamp;

    fn sample_entry(id: &str) -> BackupRegistryEntry {
        BackupRegistryEntry {
            backup_id: id.to_owned(),
            created_at: Rfc3339Timestamp::from_unix_secs(1_700_000_000).expect("valid ts"),
            artifact_path: format!("/tmp/snaps/{id}.cairn-snap.tar"),
            file_digest: format!("sha256:{:064x}", 0xdead_beef_u64),
            backup_kind: "snapshot".to_owned(),
            target_ids_included: vec![],
        }
    }

    #[test]
    fn register_creates_json_file() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let registry = FileBackupRegistry::new(dir.path().to_owned());
        let entry = sample_entry("reg-test-001");

        registry.register(&entry).expect("register");

        let expected_path = dir
            .path()
            .join(".cairn")
            .join("backups")
            .join("reg-test-001.json");
        assert!(expected_path.exists(), "registry json must exist");

        let content = std::fs::read_to_string(&expected_path).expect("read json");
        let parsed: BackupRegistryEntry = serde_json::from_str(&content).expect("parse json");
        assert_eq!(parsed.backup_id, "reg-test-001");
        assert_eq!(parsed.backup_kind, "snapshot");
    }

    #[test]
    fn register_is_idempotent() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let registry = FileBackupRegistry::new(dir.path().to_owned());
        let entry = sample_entry("reg-idem-001");

        registry.register(&entry).expect("first register");
        registry
            .register(&entry)
            .expect("second register (idempotent)");

        let expected_path = dir
            .path()
            .join(".cairn")
            .join("backups")
            .join("reg-idem-001.json");
        assert!(expected_path.exists());
    }
}
