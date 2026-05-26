//! `SnapshotArtifactReader` impl that reads `manifest.json` and recomputes
//! the three-part integrity envelope by walking the tarball.
//!
//! The tree hash accumulates `name_bytes || size_le64` for every tar entry
//! in the order they appear in the archive — matching the producer's write
//! order — so a post-transfer integrity check is O(1 read pass).

use cairn_core::contract::memory_store::StoreError;
use cairn_core::contract::snapshot_artifact::SnapshotArtifactReader;
use cairn_core::verbs::admin::manifest::{IntegrityEnvelope, SnapshotManifest};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Reads and verifies snapshot tarballs produced by [`SqliteSnapshotProducer`].
///
/// [`SqliteSnapshotProducer`]: super::producer::SqliteSnapshotProducer
pub struct SqliteSnapshotReader;

impl SnapshotArtifactReader for SqliteSnapshotReader {
    /// Read and parse `manifest.json` from the first position in the tarball.
    ///
    /// # Errors
    /// Returns `StoreError` if the file cannot be opened, the tarball is
    /// malformed, or `manifest.json` is not present.
    fn read_manifest(&self, artifact_path: &Path) -> Result<SnapshotManifest, StoreError> {
        let file = std::fs::File::open(artifact_path).map_err(|e| Box::new(e) as StoreError)?;
        let mut archive = tar::Archive::new(file);
        for entry in archive.entries().map_err(|e| Box::new(e) as StoreError)? {
            let mut e = entry.map_err(|e| Box::new(e) as StoreError)?;
            let path = e
                .path()
                .map_err(|e| Box::new(e) as StoreError)?
                .into_owned();
            if path.to_string_lossy() == "manifest.json" {
                let mut buf = Vec::new();
                e.read_to_end(&mut buf)
                    .map_err(|e| Box::new(e) as StoreError)?;
                return SnapshotManifest::from_canonical_json(&buf)
                    .map_err(|e| Box::new(e) as StoreError);
            }
        }
        Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "manifest.json not found in tarball",
        )) as StoreError)
    }

    /// Recompute the three-part integrity envelope by making one pass through
    /// the tarball:
    ///   - `manifest_sha256` = sha256(raw bytes of `manifest.json` member).
    ///   - `db_sha256` = sha256(raw bytes of `cairn.db` member).
    ///   - `tree_sha256` = sha256(name || `size_le64` for every entry in
    ///     tar order, including `manifest.json` and `cairn.db`).
    ///
    /// This mirrors the accumulation order used in
    /// [`SqliteSnapshotProducer::materialize`], so the hashes are
    /// byte-identical to what the producer recorded.
    ///
    /// # Errors
    /// Returns `StoreError` if the file cannot be opened, the tarball is
    /// malformed, or `manifest.json` is absent.
    ///
    /// [`SqliteSnapshotProducer::materialize`]: super::producer::SqliteSnapshotProducer::materialize
    fn read_envelope(&self, artifact_path: &Path) -> Result<IntegrityEnvelope, StoreError> {
        let file = std::fs::File::open(artifact_path).map_err(|e| Box::new(e) as StoreError)?;
        let mut archive = tar::Archive::new(file);

        let mut manifest_sha = String::new();
        let mut db_hasher = Sha256::new();
        let mut tree_hasher = Sha256::new();

        for entry in archive.entries().map_err(|e| Box::new(e) as StoreError)? {
            let mut e = entry.map_err(|e| Box::new(e) as StoreError)?;
            let path = e
                .path()
                .map_err(|e| Box::new(e) as StoreError)?
                .into_owned();
            let path_str = path.to_string_lossy().into_owned();
            let size = e.header().size().unwrap_or(0);

            // Tree hash: name + size for every entry in the archive, in tar order.
            // Must match the accumulation order in the producer.
            tree_hasher.update(path_str.as_bytes());
            tree_hasher.update(size.to_le_bytes());

            if path_str == "manifest.json" {
                let mut buf = Vec::new();
                e.read_to_end(&mut buf)
                    .map_err(|e| Box::new(e) as StoreError)?;
                manifest_sha = hex_encode(Sha256::digest(&buf).as_slice());
            } else if path_str == "cairn.db" {
                let mut buf = Vec::new();
                e.read_to_end(&mut buf)
                    .map_err(|e| Box::new(e) as StoreError)?;
                db_hasher.update(&buf);
            }
            // Sub-tree entries (raw/, wiki/) contribute to tree_sha but not db_sha.
        }

        if manifest_sha.is_empty() {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "manifest.json not found in tarball",
            )) as StoreError);
        }

        Ok(IntegrityEnvelope {
            manifest_sha256: manifest_sha,
            db_sha256: hex_encode(db_hasher.finalize().as_slice()),
            tree_sha256: hex_encode(tree_hasher.finalize().as_slice()),
        })
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}
