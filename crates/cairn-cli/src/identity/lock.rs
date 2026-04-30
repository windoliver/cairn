//! Advisory vault-binding lock using OS file locks (issue #50).
//!
//! [`VaultBindingLock`] wraps an exclusive `flock` on
//! `.cairn/vault.binding.lock`.  The lock is released automatically when the
//! guard is dropped.  Callers that need the 30-second retry window use
//! [`VaultBindingLock::acquire`].

use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt as _;

/// Duration after which [`VaultBindingLock::acquire`] gives up and returns
/// [`LockError::TimedOut`].
//
// Used by `acquire` once D3 wires it in.
#[allow(dead_code, reason = "consumed by acquire, which is invoked from D3+")]
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to sleep between lock-acquisition retries.
//
// Used by `acquire` once D3 wires it in.
#[allow(dead_code, reason = "consumed by acquire, which is invoked from D3+")]
const RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// Errors returned by [`VaultBindingLock::acquire`].
#[allow(dead_code, reason = "returned to D3+ callers of acquire")]
#[derive(Debug, thiserror::Error)]
pub(super) enum LockError {
    /// The lock could not be acquired within [`LOCK_TIMEOUT`].
    #[error("vault binding lock timed out after 30 s")]
    TimedOut,
    /// An OS-level I/O error prevented opening or locking the lock file.
    #[error("vault binding lock I/O error: {0}")]
    Io(#[from] io::Error),
}

/// RAII guard that holds an exclusive advisory lock on the vault binding lock
/// file for the duration of a first-bind or mutating-maintenance operation.
///
/// The lock file is **not** deleted on drop — it is left in place so that
/// subsequent acquirers can `flock` it again without a TOCTOU window.
#[allow(dead_code, reason = "constructed by D3+ first_bind callers")]
pub(super) struct VaultBindingLock {
    // Named `file` (not `_file`) so Drop can call unlock() on it.
    // The #[allow(dead_code)] on the struct covers the field too.
    #[allow(clippy::used_underscore_binding)]
    file: File,
}

impl VaultBindingLock {
    /// Open `.cairn/vault.binding.lock` (creating it if absent) and acquire an
    /// exclusive flock, retrying for up to 30 seconds before giving up.
    ///
    /// # Errors
    /// Returns [`LockError::TimedOut`] if the lock is held by another process
    /// for more than [`LOCK_TIMEOUT`], or [`LockError::Io`] on any OS error.
    #[allow(dead_code, reason = "invoked from D3+ first_bind callers")]
    pub(super) fn acquire(cairn_dir: &Path) -> Result<Self, LockError> {
        let lock_path = cairn_dir.join("vault.binding.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { file }),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(LockError::TimedOut);
                    }
                    thread::sleep(RETRY_INTERVAL);
                }
                Err(e) => return Err(LockError::Io(e)),
            }
        }
    }
}

impl Drop for VaultBindingLock {
    fn drop(&mut self) {
        // fs2's `FileExt::unlock()` releases the flock; ignore errors on drop.
        let _ = self.file.unlock();
    }
}
