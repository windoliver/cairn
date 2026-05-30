//! Cross-process vault write gate (advisory `flock`).
//!
//! Restore takes this gate EXCLUSIVE for the whole purge→swap critical section;
//! record mutations that could be lost-then-resurrected by a swap (notably
//! `forget`) take it SHARED. Because `flock` is advisory and cross-process,
//! this serializes restore against writers in *other* processes (a CLI ingest,
//! a workflow), not just in-process tasks (round-7 review #2/#3).
//!
//! The lock file lives at `<vault_root>/.cairn/write-gate.lock` — alongside
//! `cairn.db` but NOT the database file itself, so `swap_in` (which renames
//! `cairn.db`) never replaces it and the gate state survives the swap.
//!
//! The lock is held for the lifetime of the returned [`WriteGateGuard`] and
//! released when it drops (the OS releases the `flock` when the fd closes).

use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// RAII handle for a held vault write-gate lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct WriteGateGuard {
    // Held only for its `Drop` (closing the fd releases the advisory lock).
    _file: File,
}

/// Path to a vault's write-gate lock file: `<vault_root>/.cairn/write-gate.lock`.
#[must_use]
pub fn gate_path(vault_root: &Path) -> PathBuf {
    vault_root.join(".cairn").join("write-gate.lock")
}

fn open_gate(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

/// Acquire the gate EXCLUSIVELY, blocking until no shared or exclusive holder
/// remains. Restore holds this across purge→swap so no writer can commit in
/// that window.
///
/// # Errors
/// Returns `io::Error` if the lock file cannot be created/opened or `flock`
/// fails.
pub fn lock_exclusive(path: &Path) -> std::io::Result<WriteGateGuard> {
    let file = open_gate(path)?;
    FileExt::lock_exclusive(&file)?;
    Ok(WriteGateGuard { _file: file })
}

/// Acquire the gate in SHARED mode, blocking only while an EXCLUSIVE holder
/// (an in-progress restore) is active. Concurrent shared holders coexist.
///
/// # Errors
/// Returns `io::Error` if the lock file cannot be created/opened or `flock`
/// fails.
pub fn lock_shared(path: &Path) -> std::io::Result<WriteGateGuard> {
    let file = open_gate(path)?;
    FileExt::lock_shared(&file)?;
    Ok(WriteGateGuard { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_locks_coexist_then_exclusive_excludes() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = gate_path(&dir.path().join("vault"));

        // Two shared holders coexist.
        let s1 = lock_shared(&path).expect("shared 1");
        let s2 = lock_shared(&path).expect("shared 2");
        // An exclusive attempt while shared holders live must NOT succeed.
        let f = open_gate(&path).expect("open");
        assert!(
            FileExt::try_lock_exclusive(&f).is_err(),
            "exclusive must be blocked while shared holders are live"
        );
        drop(s1);
        drop(s2);

        // With no holders, exclusive succeeds; then shared is blocked.
        let _ex = lock_exclusive(&path).expect("exclusive after shared released");
        let g = open_gate(&path).expect("open");
        assert!(
            FileExt::try_lock_shared(&g).is_err(),
            "shared must be blocked while an exclusive holder is live"
        );
    }
}
