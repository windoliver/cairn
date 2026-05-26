//! `SnapshotArtifactProducer` impl that writes an uncompressed tarball
//! containing `manifest.json`, `cairn.db`, and any `raw/`+`wiki/` vault tree.
//!
//! File format: `.cairn-snap.tar` (plain uncompressed tar). Compression
//! (zstd) is deferred to a follow-up — `zstd` is not a current workspace dep.
//!
//! Integrity:
//!
//! - `db_sha256`   = sha256(bytes written as the `cairn.db` tar member).
//! - `tree_sha256` = sha256(`name_bytes` || `size_le64` bytes for every tar member,
//!   accumulated in tar-write order.

use cairn_core::contract::memory_store::StoreError;
use cairn_core::contract::snapshot_artifact::{MaterializedArtifact, SnapshotArtifactProducer};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Produces snapshot tarballs for a SQLite-backed vault.
pub struct SqliteSnapshotProducer {
    vault_root: PathBuf,
    db_path: PathBuf,
}

impl SqliteSnapshotProducer {
    /// Construct the producer.
    ///
    /// `vault_root` is the root of the vault directory (parent of `raw/`,
    /// `wiki/`, `.cairn/`). `db_path` is the path to the live `cairn.db`.
    #[must_use]
    pub fn new(vault_root: PathBuf, db_path: PathBuf) -> Self {
        Self {
            vault_root,
            db_path,
        }
    }
}

impl SnapshotArtifactProducer for SqliteSnapshotProducer {
    fn materialize(
        &self,
        out_dir: &Path,
        backup_id: &str,
        manifest_bytes: &[u8],
        label: Option<&str>,
    ) -> Result<MaterializedArtifact, StoreError> {
        let file_label = label.unwrap_or("snap");
        let artifact_name = format!("{file_label}-{backup_id}.cairn-snap.tar");
        let artifact_path = out_dir.join(&artifact_name);

        std::fs::create_dir_all(out_dir).map_err(|e| Box::new(e) as StoreError)?;

        let file = std::fs::File::create(&artifact_path).map_err(|e| Box::new(e) as StoreError)?;
        let mut tar = tar::Builder::new(file);

        let mut db_hasher = Sha256::new();
        let mut tree_hasher = Sha256::new();

        // ── 1. manifest.json (first member, required by spec §6.2) ──────────
        let manifest_len = manifest_bytes.len() as u64;
        let mut header = tar::Header::new_gnu();
        header.set_size(manifest_len);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "manifest.json", manifest_bytes)
            .map_err(|e| Box::new(e) as StoreError)?;
        // tree hash: accumulate name + size for every member in tar order.
        tree_hasher.update(b"manifest.json");
        tree_hasher.update(manifest_len.to_le_bytes());

        // ── 2. cairn.db ──────────────────────────────────────────────────────
        // Read once into memory — vaults are small enough for v0.2.
        // TODO(perf): stream-while-hashing for large vaults (follow-up).
        let db_bytes = std::fs::read(&self.db_path).map_err(|e| Box::new(e) as StoreError)?;
        db_hasher.update(&db_bytes);
        let db_len = db_bytes.len() as u64;
        let mut db_header = tar::Header::new_gnu();
        db_header.set_size(db_len);
        db_header.set_mode(0o644);
        db_header.set_cksum();
        tar.append_data(&mut db_header, "cairn.db", &db_bytes[..])
            .map_err(|e| Box::new(e) as StoreError)?;
        tree_hasher.update(b"cairn.db");
        tree_hasher.update(db_len.to_le_bytes());

        // ── 3. Optional vault markdown tree (raw/ and wiki/) ─────────────────
        for sub in ["raw", "wiki"] {
            let dir = self.vault_root.join(sub);
            if dir.exists() && dir.is_dir() {
                tar.append_dir_all(sub, &dir)
                    .map_err(|e| Box::new(e) as StoreError)?;
                // Walk deterministically (sorted) to hash name + size.
                walk_for_tree_hash(&dir, sub, &mut tree_hasher)
                    .map_err(|e| Box::new(e) as StoreError)?;
            }
        }

        tar.finish().map_err(|e| Box::new(e) as StoreError)?;

        Ok(MaterializedArtifact {
            path: artifact_path,
            db_sha256: hex_encode(db_hasher.finalize().as_slice()),
            tree_sha256: hex_encode(tree_hasher.finalize().as_slice()),
        })
    }
}

/// Recursively walk `dir` in deterministic (sorted) order and update
/// `hasher` with `"<prefix>/<filename>"` bytes + file size as `u64`
/// little-endian for every regular file.
fn walk_for_tree_hash(dir: &Path, prefix: &str, hasher: &mut Sha256) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(Result::ok).collect();
    // Sort by path for determinism across platforms.
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        let rel = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        if path.is_dir() {
            walk_for_tree_hash(&path, &rel, hasher)?;
        } else {
            let meta = entry.metadata()?;
            hasher.update(rel.as_bytes());
            hasher.update(meta.len().to_le_bytes());
        }
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::reader::SqliteSnapshotReader;
    use cairn_core::contract::snapshot_artifact::SnapshotArtifactReader;
    use cairn_core::verbs::admin::manifest::{MANIFEST_SCHEMA_VERSION, SnapshotManifest};

    /// Build a minimal but structurally valid `SnapshotManifest` for use in
    /// unit tests.  All counts are zero and the timestamps are epoch.
    fn test_manifest(backup_id: &str) -> Vec<u8> {
        let m = SnapshotManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            backup_id: backup_id.to_owned(),
            created_at: chrono::DateTime::UNIX_EPOCH,
            source_machine_id: "test-machine".to_owned(),
            source_vault_id: "test-vault".to_owned(),
            frontier_step: "step:0".to_owned(),
            record_count: 0,
            tombstone_count: 0,
            schema_versions: std::collections::BTreeMap::new(),
            label: None,
        };
        m.to_canonical_json().expect("canonical JSON")
    }

    /// Smoke-test: materialize a tiny artifact, then `read_manifest` and
    /// `read_envelope` back — envelope `manifest_sha` must equal sha256 of
    /// the canonical manifest bytes.
    #[test]
    fn producer_reader_roundtrip() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let vault_root = dir.path().join("vault");
        std::fs::create_dir_all(&vault_root).expect("vault_root");

        // Create a minimal cairn.db (a few bytes — not a real DB for this test).
        let db_dir = vault_root.join(".cairn");
        std::fs::create_dir_all(&db_dir).expect("db_dir");
        let db_path = db_dir.join("cairn.db");
        std::fs::write(&db_path, b"fake-db-bytes").expect("write db");

        // Build a valid canonical-JSON manifest payload.
        let manifest_bytes = test_manifest("test");

        let producer = SqliteSnapshotProducer::new(vault_root.clone(), db_path.clone());
        let out_dir = dir.path().join("snapshots");
        let artifact = producer
            .materialize(&out_dir, "test-id-001", &manifest_bytes, Some("test"))
            .expect("materialize");

        assert!(
            artifact.path.exists(),
            "artifact tarball must exist at {:?}",
            artifact.path
        );

        // Read back the manifest.
        let reader = SqliteSnapshotReader;
        let raw_manifest = reader.read_manifest(&artifact.path).expect("read_manifest");
        assert_eq!(raw_manifest.backup_id, "test");

        // Recompute envelope and verify manifest_sha matches sha256(manifest_bytes).
        let envelope = reader.read_envelope(&artifact.path).expect("read_envelope");
        let expected_manifest_sha = {
            use sha2::Digest;
            let mut h = sha2::Sha256::new();
            h.update(&manifest_bytes);
            hex_encode(h.finalize().as_slice())
        };
        assert_eq!(
            envelope.manifest_sha256, expected_manifest_sha,
            "envelope.manifest_sha256 must match sha256(manifest_bytes)"
        );

        // db_sha256 from the producer must match db_sha256 from the reader.
        assert_eq!(
            artifact.db_sha256, envelope.db_sha256,
            "producer and reader must agree on db_sha256"
        );

        // tree_sha256: producer and reader must agree (both walk in tar order).
        assert_eq!(
            artifact.tree_sha256, envelope.tree_sha256,
            "producer and reader must agree on tree_sha256"
        );
    }
}
