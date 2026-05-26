//! Adapter boundary for snapshot artifact production and integrity probing.
//!
//! Lets `cairn-core::verbs::admin::snapshot` stay I/O-free: the verb computes
//! the manifest + integrity envelope, then asks an adapter (typically
//! `cairn-cli` or a future `cairn-store-sqlite` module) to produce the tarball.

use crate::contract::memory_store::StoreError;
use std::path::{Path, PathBuf};

/// Provides the per-vault numbers that go into the manifest. Read-only.
pub trait SnapshotMetadataProvider: Send + Sync {
    /// Number of live records in the vault at snapshot time.
    fn record_count(&self) -> Result<u64, StoreError>;
    /// Number of tombstone records in the vault at snapshot time.
    fn tombstone_count(&self) -> Result<u64, StoreError>;
    /// WAL step marker at snapshot time (opaque string).
    fn frontier_step(&self) -> Result<String, StoreError>;
    /// Stable vault identifier — distinguishes vault A from vault B
    /// on the same machine.
    fn vault_id(&self) -> Result<String, StoreError>;
    /// Per-component migration heads at snapshot time, e.g.
    /// `{ "store": 4, "wal": 1 }`. Used by the restore-time `SchemaTooNew`
    /// precondition check.
    fn migration_heads(&self) -> Result<std::collections::BTreeMap<String, u32>, StoreError>;
}

/// Produces a snapshot tarball at `out_dir/<artifact_name>` and returns the
/// path, plus the inputs the verb needs to compute the integrity envelope.
pub trait SnapshotArtifactProducer: Send + Sync {
    /// Writes the tarball and returns its path. Implementations must include
    /// `manifest.json` (canonical-JSON-serialized by the caller, passed via
    /// `manifest_bytes`) as the first tar member; `cairn.db`; and any
    /// adjacent vault tree the adapter chooses to capture.
    fn materialize(
        &self,
        out_dir: &Path,
        backup_id: &str,
        manifest_bytes: &[u8],
        label: Option<&str>,
    ) -> Result<MaterializedArtifact, StoreError>;
}

/// Output of `SnapshotArtifactProducer::materialize`: the tarball path plus
/// the two sha256 components the verb needs to fill in the
/// `IntegrityEnvelope` (`manifest_sha` is computed by the verb itself).
pub struct MaterializedArtifact {
    /// Filesystem path of the written tarball artifact.
    pub path: PathBuf,
    /// sha256 of the bytes that were written as the `cairn.db` tar member.
    pub db_sha256: String,
    /// sha256 of the tar member tree (filenames + sizes in tar order),
    /// computed by the adapter while writing.
    pub tree_sha256: String,
}

/// Registers a snapshot entry in the backup registry.
pub trait BackupRegistry: Send + Sync {
    /// Persist a new backup registry entry. Idempotent by `backup_id`.
    fn register(
        &self,
        entry: &crate::domain::backup::BackupRegistryEntry,
    ) -> Result<(), StoreError>;
}
