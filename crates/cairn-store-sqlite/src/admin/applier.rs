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

    /// Swap the staged contents into the live vault **transactionally**.
    ///
    /// The live `cairn.db` and (when staged) `raw/`+`wiki/` are replaced as a
    /// unit. Each live target is first moved ASIDE to a sibling
    /// `*.restore-bak` (an atomic same-directory rename) before the staged
    /// copy is renamed into place — so no live data is *deleted* before its
    /// replacement is committed. If any rename fails partway through, every
    /// completed step is rolled back (staged copy removed, original restored
    /// from its aside backup), leaving the vault in its pre-restore state. On
    /// full success the aside backups and the staging directory are removed.
    ///
    /// This closes the data-loss window where a failed `raw/`/`wiki/` rename
    /// could leave a swapped DB next to a deleted or half-replaced tree
    /// (round-4 review #3).
    ///
    /// # Errors
    /// Returns `StoreError` if `cairn.db` is absent from the staging root, a
    /// stale aside backup cannot be cleared, or any rename fails (after
    /// rolling back).
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

        // Replacement plan: (staged src, live dst, aside backup). The backup is
        // a sibling of dst (same directory → same device → atomic rename).
        // cairn.db is always present; raw/ and wiki/ only when staged.
        let mut plan: Vec<(PathBuf, PathBuf, PathBuf)> = vec![(
            staged_db,
            live_db.clone(),
            live_cairn_dir.join("cairn.db.restore-bak"),
        )];
        for sub in ["raw", "wiki"] {
            let src = staging_root.join(sub);
            if src.exists() {
                plan.push((
                    src,
                    self.vault_root.join(sub),
                    self.vault_root.join(format!("{sub}.restore-bak")),
                ));
            }
        }

        // Clear any stale aside backups left by a previously-interrupted swap.
        for (_, _, bak) in &plan {
            if bak.exists() {
                remove_path(bak).map_err(|e| Box::new(e) as StoreError)?;
            }
        }

        // Apply with rollback. `done` records (dst, bak, had_live) for undo.
        let mut done: Vec<(&PathBuf, &PathBuf, bool)> = Vec::new();
        for (src, dst, bak) in &plan {
            let had_live = dst.exists();
            if had_live && let Err(e) = std::fs::rename(dst, bak) {
                rollback(&done);
                return Err(Box::new(e) as StoreError);
            }
            if let Err(e) = std::fs::rename(src, dst) {
                // Undo this step's aside move, then roll back prior steps.
                if had_live {
                    let _ = std::fs::rename(bak, dst);
                }
                rollback(&done);
                return Err(Box::new(e) as StoreError);
            }
            done.push((dst, bak, had_live));
        }

        // Success: discard the aside backups and the staging dir (best-effort).
        for (_, bak, had_live) in &done {
            if *had_live {
                let _ = remove_path(bak);
            }
        }
        let _ = std::fs::remove_dir_all(staging_root);

        Ok(())
    }
}

/// Remove a path whether it is a file or a directory. Used for aside backups,
/// which are a file (`cairn.db.restore-bak`) or a directory
/// (`raw.restore-bak`, `wiki.restore-bak`).
fn remove_path(p: &Path) -> std::io::Result<()> {
    if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

/// Roll back a partially-applied swap. For each completed `(dst, bak,
/// had_live)` in REVERSE: remove the staged copy now sitting at `dst`, and if
/// an original was moved aside, restore it from `bak`. Best-effort — already on
/// an error path; the goal is to restore the operator's ORIGINAL live data.
fn rollback(done: &[(&PathBuf, &PathBuf, bool)]) {
    for (dst, bak, had_live) in done.iter().rev() {
        let _ = remove_path(dst);
        if *had_live {
            let _ = std::fs::rename(bak, dst);
        }
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

        // Create a REAL cairn.db at schema head: the producer captures it via
        // `VACUUM INTO`, which needs a valid SQLite source (a fake byte string
        // is not a database).
        let db_path = cairn_dir.join("cairn.db");
        {
            let conn = crate::open::open_sync(&db_path).expect("create real cairn.db");
            conn.close().expect("close db before snapshot");
        }

        let manifest_bytes = b"{\"schema_version\":1,\"backup_id\":\"apply-test\"}";
        let producer = SqliteSnapshotProducer::new(vault_root.clone(), db_path.clone());
        let out_dir = dir.path().join("snaps");
        let artifact = producer
            .materialize(&out_dir, "apply-test-001", manifest_bytes, None)
            .expect("materialize");

        // Overwrite the live DB to simulate post-snapshot mutation/corruption.
        std::fs::write(&db_path, b"mutated-db").expect("mutate db");

        let applier = SqliteSnapshotApplier::new(vault_root.clone());
        let staging = applier
            .stage(&artifact.path, "apply-test-001")
            .expect("stage");
        assert!(staging.exists(), "staging dir must exist after stage");

        applier.swap_in(&staging).expect("swap_in");

        // After swap, the live DB is the snapshot's cairn.db: the post-snapshot
        // mutation is gone, and the restored file is a valid SQLite database at
        // schema head (proving VACUUM INTO captured a real, restorable db).
        let restored = std::fs::read(&db_path).expect("read restored db");
        assert_ne!(
            restored, b"mutated-db" as &[u8],
            "swap must overwrite the post-snapshot mutation"
        );
        crate::open::open_sync(&db_path)
            .expect("restored db must open at schema head")
            .close()
            .expect("close reopened db");

        // Staging dir should be cleaned up.
        assert!(
            !staging.exists(),
            "staging dir must be removed after swap_in"
        );
    }

    /// Build a vault with live `cairn.db` + `raw/` + `wiki/` originals and a
    /// staging dir with new content; `swap_in` must replace all three and leave
    /// no aside backups (round-4 review #3 — multi-target swap, happy path).
    #[test]
    fn swap_in_replaces_db_and_vault_trees() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let vault_root = dir.path().join("vault");
        let cairn_dir = vault_root.join(".cairn");
        std::fs::create_dir_all(&cairn_dir).expect("cairn_dir");
        std::fs::write(cairn_dir.join("cairn.db"), b"ORIG-DB").expect("orig db");
        std::fs::create_dir_all(vault_root.join("raw")).expect("raw");
        std::fs::create_dir_all(vault_root.join("wiki")).expect("wiki");
        std::fs::write(vault_root.join("raw/a.md"), b"ORIG-RAW").expect("orig raw");
        std::fs::write(vault_root.join("wiki/b.md"), b"ORIG-WIKI").expect("orig wiki");

        // swap_in only renames files — fake db bytes are fine here (it never
        // opens the database; that is the producer's job).
        let staging = cairn_dir.join("restore-multi");
        std::fs::create_dir_all(staging.join("raw")).expect("st raw");
        std::fs::create_dir_all(staging.join("wiki")).expect("st wiki");
        std::fs::write(staging.join("cairn.db"), b"NEW-DB").expect("new db");
        std::fs::write(staging.join("raw/a.md"), b"NEW-RAW").expect("new raw");
        std::fs::write(staging.join("wiki/b.md"), b"NEW-WIKI").expect("new wiki");

        let applier = SqliteSnapshotApplier::new(vault_root.clone());
        applier.swap_in(&staging).expect("swap_in");

        assert_eq!(
            std::fs::read(cairn_dir.join("cairn.db")).unwrap(),
            b"NEW-DB"
        );
        assert_eq!(
            std::fs::read(vault_root.join("raw/a.md")).unwrap(),
            b"NEW-RAW"
        );
        assert_eq!(
            std::fs::read(vault_root.join("wiki/b.md")).unwrap(),
            b"NEW-WIKI"
        );
        assert!(!cairn_dir.join("cairn.db.restore-bak").exists());
        assert!(!vault_root.join("raw.restore-bak").exists());
        assert!(!vault_root.join("wiki.restore-bak").exists());
        assert!(!staging.exists());
    }

    /// Inject a mid-swap failure: with `vault_root` made read-only, the
    /// `cairn.db` swap (inside the still-writable `.cairn/`) succeeds but the
    /// `raw/` rename (which needs write on `vault_root`) is denied — so the
    /// `cairn.db` swap must be ROLLED BACK to the original, leaving the vault
    /// untouched (round-4 review #3 — rollback path).
    #[cfg(unix)]
    #[test]
    fn swap_in_rolls_back_on_tree_rename_failure() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tmpdir");
        let vault_root = dir.path().join("vault");
        let cairn_dir = vault_root.join(".cairn");
        std::fs::create_dir_all(&cairn_dir).expect("cairn_dir");
        std::fs::write(cairn_dir.join("cairn.db"), b"ORIG-DB").expect("orig db");
        std::fs::create_dir_all(vault_root.join("raw")).expect("raw");
        std::fs::write(vault_root.join("raw/a.md"), b"ORIG-RAW").expect("orig raw");

        let staging = cairn_dir.join("restore-fail");
        std::fs::create_dir_all(staging.join("raw")).expect("st raw");
        std::fs::write(staging.join("cairn.db"), b"NEW-DB").expect("new db");
        std::fs::write(staging.join("raw/a.md"), b"NEW-RAW").expect("new raw");

        // Read-only vault_root denies renaming raw/ (parent = vault_root) while
        // .cairn (a subdir) stays writable, so the cairn.db swap commits first.
        let orig_perms = std::fs::metadata(&vault_root).unwrap().permissions();
        std::fs::set_permissions(&vault_root, std::fs::Permissions::from_mode(0o555)).unwrap();

        let applier = SqliteSnapshotApplier::new(vault_root.clone());
        let result = applier.swap_in(&staging);

        // Restore writability before asserting / tempdir cleanup.
        std::fs::set_permissions(&vault_root, orig_perms).unwrap();

        assert!(
            result.is_err(),
            "swap must fail when the raw/ rename is denied"
        );
        assert_eq!(
            std::fs::read(cairn_dir.join("cairn.db")).unwrap(),
            b"ORIG-DB",
            "cairn.db must be rolled back to the original after a later-step failure"
        );
        assert_eq!(
            std::fs::read(vault_root.join("raw/a.md")).unwrap(),
            b"ORIG-RAW",
            "raw/ must be untouched"
        );
        assert!(
            !cairn_dir.join("cairn.db.restore-bak").exists(),
            "no aside backup may be left behind after rollback"
        );
    }
}
