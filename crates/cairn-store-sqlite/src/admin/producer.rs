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
        // Capture a transactionally-consistent image of the live DB (WAL-safe)
        // rather than reading the main file in place. Read once into memory —
        // vaults are small enough for v0.2.
        // TODO(perf): stream-while-hashing for large vaults (follow-up).
        let db_bytes = consistent_db_bytes(&self.db_path)?;
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
        // Walk each tree in deterministic sorted order, appending every regular
        // file to the tar AND folding its name + size + CONTENT into the tree
        // hash in the SAME order. Hashing file *contents* (not just name+size)
        // means a same-length edit to a raw/wiki file changes `tree_sha256`, so
        // restore-time integrity rejects vault-tree tampering (round-4 review
        // #1). Writing in sorted order makes the tar member order equal the
        // hash order, so the reader (which walks tar order) recomputes an
        // identical digest even for multi-file trees.
        for sub in ["raw", "wiki"] {
            let dir = self.vault_root.join(sub);
            if dir.exists() && dir.is_dir() {
                append_tree_sorted(&mut tar, &dir, sub, &mut tree_hasher)
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

/// Produce a transactionally-consistent byte image of the live `SQLite`
/// database at `db_path`, safe under WAL mode.
///
/// The store runs `PRAGMA journal_mode=WAL`, so freshly-committed rows can
/// live in the `-wal` sidecar that a naive `std::fs::read` of the main DB
/// file would miss — yielding a snapshot that silently drops recent commits
/// or captures a torn page mid-checkpoint. Instead we open our own
/// connection and run `VACUUM INTO`, which takes a read transaction over the
/// current committed state (WAL frames included) and writes a fresh,
/// standalone, defragmented copy. We then read that copy's bytes
/// (round-3 adversarial review #3).
fn consistent_db_bytes(db_path: &Path) -> Result<Vec<u8>, StoreError> {
    // Register the sqlite-vec vec0 module first so VACUUM can reconstruct the
    // `record_vectors` virtual table in the destination database. Registration
    // is process-global and idempotent.
    crate::vec_ext::register_vec0();
    let conn = rusqlite::Connection::open(db_path).map_err(|e| Box::new(e) as StoreError)?;
    // `trusted_schema=ON` lets VACUUM process the vec0 virtual table; the
    // busy_timeout lets us wait out a concurrent writer's lock rather than
    // failing immediately with SQLITE_BUSY.
    conn.execute_batch("PRAGMA trusted_schema=ON; PRAGMA busy_timeout=5000;")
        .map_err(|e| Box::new(e) as StoreError)?;

    let tmp = tempfile::tempdir().map_err(|e| Box::new(e) as StoreError)?;
    let dest = tmp.path().join("snapshot.db");
    // VACUUM INTO requires a destination path that does NOT already exist; the
    // tempdir is empty so `snapshot.db` is fresh. Double any single-quote for
    // SQL string-literal escaping — tempdir paths normally contain none, but
    // escape defensively.
    let dest_sql = dest.display().to_string().replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{dest_sql}'"))
        .map_err(|e| Box::new(e) as StoreError)?;
    drop(conn);

    let bytes = std::fs::read(&dest).map_err(|e| Box::new(e) as StoreError)?;
    // `tmp` (holding `dest`) is removed when it drops at end of scope.
    Ok(bytes)
}

/// Recursively walk `dir` in deterministic (sorted) order, appending every
/// regular file to `tar` as `"<prefix>/<name>"` AND folding its
/// `"<prefix>/<name>"` bytes, size (`u64` LE), and full file CONTENTS into
/// `hasher` — in the same order they are written to the archive.
///
/// Hashing the contents (not just name + size) is what lets restore detect a
/// same-length edit to a vault-tree file. Writing to the tar and hashing in a
/// single sorted pass guarantees the tar member order equals the hash order,
/// so the reader (which recomputes the digest in tar order) agrees.
fn append_tree_sorted<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    prefix: &str,
    hasher: &mut Sha256,
) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(Result::ok).collect();
    // Sort by path for determinism across platforms.
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        let rel = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        // `file_type()` does NOT follow symlinks. Skip symlinks (and any other
        // non-regular node) WITHOUT descending or reading through them: a
        // symlink under raw/wiki pointing outside the vault would otherwise be
        // archived as regular snapshot content, leaking out-of-vault data
        // (round-6 review #3). `is_dir()`/`read()` on `path` below are only
        // reached for genuine directories/regular files.
        let ftype = entry.file_type()?;
        if ftype.is_symlink() {
            tracing::warn!(
                entry = %rel,
                "skipping symlink during snapshot: symlinks are not archived to \
                 avoid following links outside the vault tree",
            );
            continue;
        }
        if ftype.is_dir() {
            append_tree_sorted(tar, &path, &rel, hasher)?;
        } else if ftype.is_file() {
            let bytes = std::fs::read(&path)?;
            hasher.update(rel.as_bytes());
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);

            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, &rel, &bytes[..])?;
        } else {
            // FIFO, socket, device, etc. — not snapshot content. Skip.
            tracing::warn!(entry = %rel, "skipping non-regular filesystem node during snapshot");
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

        // Create a REAL minimal cairn.db at schema head: the producer now
        // captures the DB via `VACUUM INTO`, which needs a valid SQLite
        // source (the old `b"fake-db-bytes"` placeholder is not a database).
        let db_dir = vault_root.join(".cairn");
        std::fs::create_dir_all(&db_dir).expect("db_dir");
        let db_path = db_dir.join("cairn.db");
        {
            let conn = crate::open::open_sync(&db_path).expect("create real cairn.db");
            conn.close().expect("close db before snapshot");
        }

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

    /// Round-4 review #1: the tree hash must cover vault-tree file *contents*
    /// (not just name + size), and the producer and reader must agree even for
    /// a multi-file, multi-directory tree. Asserts (a) producer == reader for a
    /// populated raw/wiki tree, and (b) a same-length content edit changes
    /// `tree_sha256`.
    #[test]
    fn tree_hash_covers_vault_file_contents_and_agrees() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let vault_root = dir.path().join("vault");
        let db_dir = vault_root.join(".cairn");
        std::fs::create_dir_all(&db_dir).expect("db_dir");
        let db_path = db_dir.join("cairn.db");
        {
            let conn = crate::open::open_sync(&db_path).expect("create real cairn.db");
            conn.close().expect("close db");
        }

        // Populate a multi-file, nested vault tree.
        std::fs::create_dir_all(vault_root.join("raw/sub")).expect("raw/sub");
        std::fs::create_dir_all(vault_root.join("wiki")).expect("wiki");
        std::fs::write(vault_root.join("raw/a.md"), b"AAAA").expect("a.md");
        std::fs::write(vault_root.join("raw/sub/b.md"), b"BBBB").expect("b.md");
        std::fs::write(vault_root.join("wiki/c.md"), b"CCCC").expect("c.md");

        let manifest_bytes = test_manifest("tree");
        let producer = SqliteSnapshotProducer::new(vault_root.clone(), db_path.clone());
        let out_dir = dir.path().join("snaps1");
        let artifact = producer
            .materialize(&out_dir, "tree-001", &manifest_bytes, None)
            .expect("materialize");

        // (a) producer and reader agree on tree_sha for a populated tree.
        let reader = SqliteSnapshotReader;
        let envelope = reader.read_envelope(&artifact.path).expect("read_envelope");
        assert_eq!(
            artifact.tree_sha256, envelope.tree_sha256,
            "producer and reader must agree on tree_sha256 for a multi-file tree"
        );

        // (b) a SAME-LENGTH content edit must change tree_sha256.
        std::fs::write(vault_root.join("raw/a.md"), b"ZZZZ").expect("rewrite a.md");
        let out_dir2 = dir.path().join("snaps2");
        let artifact2 = producer
            .materialize(&out_dir2, "tree-002", &manifest_bytes, None)
            .expect("materialize 2");
        assert_ne!(
            artifact.tree_sha256, artifact2.tree_sha256,
            "a same-length vault-file content edit must change tree_sha256"
        );
    }

    /// Round-6 review #3: snapshot production must NOT follow symlinks in the
    /// vault tree — a symlink under raw/ pointing outside the vault must be
    /// skipped, never archived as regular content (no out-of-vault data leak).
    #[cfg(unix)]
    #[test]
    fn snapshot_skips_symlinks_in_vault_tree() {
        use std::io::Read as _;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tmpdir");
        let vault_root = dir.path().join("vault");
        let db_dir = vault_root.join(".cairn");
        std::fs::create_dir_all(&db_dir).expect("db_dir");
        let db_path = db_dir.join("cairn.db");
        {
            let conn = crate::open::open_sync(&db_path).expect("create real cairn.db");
            conn.close().expect("close db");
        }

        // A secret OUTSIDE the vault tree.
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, b"TOP-SECRET-OUTSIDE-VAULT").expect("secret");

        // raw/ holds a real file plus a symlink to the out-of-vault secret.
        std::fs::create_dir_all(vault_root.join("raw")).expect("raw");
        std::fs::write(vault_root.join("raw/real.md"), b"real content").expect("real");
        symlink(&secret, vault_root.join("raw/leak.md")).expect("symlink");

        let manifest_bytes = test_manifest("sym");
        let producer = SqliteSnapshotProducer::new(vault_root.clone(), db_path.clone());
        let out_dir = dir.path().join("snaps");
        let artifact = producer
            .materialize(&out_dir, "sym-001", &manifest_bytes, None)
            .expect("materialize");

        // The secret's content must NOT appear anywhere in the artifact.
        let raw = std::fs::read(&artifact.path).expect("read artifact");
        assert!(
            !String::from_utf8_lossy(&raw).contains("TOP-SECRET-OUTSIDE-VAULT"),
            "symlink target content must never be archived"
        );

        // The real file is archived; the symlink member is absent.
        let file = std::fs::File::open(&artifact.path).expect("open artifact");
        let mut archive = tar::Archive::new(file);
        let mut names = Vec::new();
        for entry in archive.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let name = entry.path().expect("path").to_string_lossy().into_owned();
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).expect("read");
            names.push(name);
        }
        assert!(
            names.iter().any(|n| n == "raw/real.md"),
            "the real file must be archived; got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "raw/leak.md"),
            "the symlink must be skipped, not archived; got {names:?}"
        );
    }
}
