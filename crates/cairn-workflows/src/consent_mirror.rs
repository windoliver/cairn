//! Async-friendly consent log materializer (brief §14, issue #94).
//!
//! Tails the `SQLite` `consent_journal` table by `rowid` and appends each
//! event as a JSON line to `.cairn/consent.log`. The mirror is **never
//! authoritative** — the database is. The on-disk log is rebuildable
//! via [`ConsentLogMaterializer::rebuild_from_db`].
//!
//! Each line is a `{"rowid": N, "event": <ConsentEvent>}` envelope — the
//! `rowid` is the canonical cursor. On open we recover the cursor by
//! reading the **last well-formed envelope** in the log. If the file's
//! tail contains a torn (no-newline / partial) write we truncate at the
//! byte offset just after the last good envelope so future appends start
//! from a clean line boundary; otherwise repeated tick + reopen cycles
//! would prepend a partial JSON fragment to every new line and brick
//! deserialization. The cursor file at `.cairn/consent.cursor` is purely
//! an O(1) fast-path hint — when the log is empty the sidecar is
//! ignored, because a non-zero hint over an empty log is always a sign
//! of inconsistency (sidecar survived a log truncation).
//!
//! Per-row durability: every event is written, fsync'd to the log file,
//! and the parent directory is fsync'd to make the new bytes visible
//! across remount before the cursor is advanced. Without the parent
//! `fsync`, an `fsync` on the file alone does not guarantee the new
//! file size or the rename is durable on every filesystem (POSIX leaves
//! this to the implementation; ext4 + APFS need both).
//!
//! Concurrent-writer safety: every `open` / `tick` / `rebuild_from_db`
//! holds an exclusive advisory file lock on `.cairn/consent.lock`. Two
//! materializers that try to drive the same vault block each other on
//! the lock instead of racing into the log. The lock is held for the
//! duration of `tick` only, so background materializers do not starve
//! ad-hoc CLI rebuilds.
//!
//! Brief §14: "no duplicates, no gaps". This module enforces that under
//! the failure modes we can locally simulate: prefix-write crash, cursor
//! desync, log corruption, and concurrent writers.
//!
//! The file primitives here are blocking (`std::fs`). Callers that want
//! to run the materializer from a tokio runtime should drive it via
//! [`tokio::task::spawn_blocking`] or schedule it on a dedicated thread.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use cairn_core::domain::ConsentEvent;
use cairn_store_sqlite::consent::read_since_rowid;
use cairn_store_sqlite::error::StoreError;
use fs4::fs_std::FileExt;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors raised by the materializer.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MirrorError {
    /// I/O against `.cairn/consent.log` or its cursor / lock files.
    #[error("consent.log io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization of a `ConsentEvent` envelope.
    #[error("consent event encode: {0}")]
    Encode(#[from] serde_json::Error),
    /// Underlying store query failure.
    #[error("consent store: {0}")]
    Store(#[from] StoreError),
    /// The log is non-empty but the bounded recovery scan could not find
    /// a single well-formed envelope. Continuing to append would either
    /// duplicate rows (cursor 0) or skip rows (stale sidecar). The caller
    /// must repair the vault via [`ConsentLogMaterializer::rebuild_from_db`].
    #[error(
        "consent.log corrupt: non-empty file has no parseable envelope in the recovery window — \
         repair via rebuild_from_db"
    )]
    LogCorrupt,
}

/// On-disk envelope wrapping each `ConsentEvent`. The `rowid` field is
/// the canonical mirror cursor; downstream readers that only care about
/// the audit content read `event` and ignore `rowid`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LogLine {
    rowid: i64,
    event: ConsentEvent,
}

/// Stateful tail-and-append materializer for a single vault's consent log.
#[derive(Debug)]
pub struct ConsentLogMaterializer {
    log_path: PathBuf,
    cursor_path: PathBuf,
    lock_path: PathBuf,
    cursor: i64,
}

impl ConsentLogMaterializer {
    /// Open the materializer for the vault rooted at `vault_dir`. Creates
    /// the log, cursor, and lock files if missing. Acquires the vault
    /// lock briefly to recover the cursor authoritatively from the log.
    /// On a torn tail, the log is truncated to the last clean envelope.
    ///
    /// # Errors
    /// Returns [`MirrorError`] on I/O failure during create/probe.
    pub fn open(vault_dir: impl AsRef<Path>) -> Result<Self, MirrorError> {
        let dir = vault_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let log_path = dir.join("consent.log");
        let cursor_path = dir.join("consent.cursor");
        let lock_path = dir.join("consent.lock");

        // Ensure the log exists and is durable.
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        log_file.sync_all()?;
        drop(log_file);
        fsync_parent(&log_path)?;

        // Hold the vault lock while we recover the cursor — another
        // materializer could otherwise be mid-tick and our scan would
        // see a partially-written tail.
        let _guard = LockGuard::acquire(&lock_path)?;

        // Authoritative cursor: scan the log's tail. If the tail is
        // torn, truncate to the last clean envelope before continuing.
        // Fail closed if the log is non-empty but has no parseable
        // envelope in the recovery window — appending past unparseable
        // bytes (cursor 0 or stale sidecar) would either duplicate or
        // skip rows. Caller must repair via `rebuild_at`.
        let recovery = recover_cursor_from_log(&log_path)?;
        let cursor = match recovery.cursor {
            Some(rowid) => rowid,
            None => {
                if log_is_empty(&log_path)? {
                    // Empty log — never trust a stale sidecar; force 0
                    // so a subsequent `rebuild_from_db` (or normal tick)
                    // replays every row.
                    0
                } else {
                    return Err(MirrorError::LogCorrupt);
                }
            }
        };

        if recovery.truncated_to_byte_offset.is_some() {
            // We just rewrote the log tail. Refresh the sidecar so the
            // fast-path matches reality.
            let _ = write_cursor_hint(&cursor_path, cursor);
        } else {
            let _ = write_cursor_hint(&cursor_path, cursor);
        }

        Ok(Self {
            log_path,
            cursor_path,
            lock_path,
            cursor,
        })
    }

    /// Last rowid the materializer believes it has appended.
    #[must_use]
    pub const fn cursor(&self) -> i64 {
        self.cursor
    }

    /// Path to the human-readable log file.
    #[must_use]
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// Path to the cursor sidecar file (advisory fast-path; the log
    /// itself is the authoritative cursor source).
    #[must_use]
    pub fn cursor_path(&self) -> &Path {
        &self.cursor_path
    }

    /// Path to the vault lock file (`consent.lock`) used to serialize
    /// concurrent ticks across processes / threads.
    #[must_use]
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// Append every journal row with `rowid > self.cursor` to the log.
    /// Each row is written as a `{rowid, event}` envelope, the file is
    /// fsync'd, the parent directory is fsync'd, and only then is the
    /// in-memory cursor advanced. The cursor sidecar is updated as a
    /// best-effort hint after the log is durable. The vault lock is
    /// held for the entire call. Returns the number of rows mirrored.
    ///
    /// # Errors
    /// Returns [`MirrorError`] on store, I/O, or encoding failure. On a
    /// partial write the on-disk log is the authority for what was
    /// committed; the next call re-reads from the log's last envelope.
    pub fn tick(&mut self, conn: &Connection) -> Result<usize, MirrorError> {
        let _guard = LockGuard::acquire(&self.lock_path)?;
        // Re-read the authoritative cursor under the lock — a peer
        // process may have advanced past us since `open`. Fail closed
        // on the same conditions `open()` does:
        //
        // * Non-empty log with no parseable envelope → `LogCorrupt`.
        //   A stale in-memory cursor would otherwise append after
        //   garbage or skip already-mirrored rows.
        //
        // * Recovered cursor lower than the in-memory cursor →
        //   `LogCorrupt`. The disk regressed (truncation to a valid
        //   prefix, restoration from backup, …); honoring the lower
        //   value would skip rows between the new tail and our
        //   cursor, and honoring the higher in-memory value would
        //   append past the gap. The vault must rebuild.
        match recover_cursor_from_log(&self.log_path)?.cursor {
            Some(rowid) if rowid > self.cursor => self.cursor = rowid,
            Some(rowid) if rowid == self.cursor => {}
            Some(_) => return Err(MirrorError::LogCorrupt),
            None => {
                if log_is_empty(&self.log_path)? {
                    // Log went empty (e.g., truncate-by-operator) —
                    // reset the in-memory cursor so the next read
                    // replays from rowid 0.
                    self.cursor = 0;
                } else {
                    return Err(MirrorError::LogCorrupt);
                }
            }
        }

        let pending = read_since_rowid(conn, self.cursor)?;
        if pending.is_empty() {
            return Ok(0);
        }

        let mut log_file = OpenOptions::new().append(true).open(&self.log_path)?;
        let mut written = 0usize;
        for (rowid, event) in pending {
            let line = serde_json::to_string(&LogLine { rowid, event })?;
            writeln!(log_file, "{line}")?;
            log_file.flush()?;
            log_file.sync_all()?;
            fsync_parent(&self.log_path)?;
            self.cursor = rowid;
            written += 1;
        }

        // Refresh the cursor sidecar after the log is durable. Best
        // effort — the log is the authoritative recovery source.
        let _ = write_cursor_hint(&self.cursor_path, self.cursor);

        Ok(written)
    }

    /// Repair a vault whose log is corrupt by rebuilding from the
    /// database, then return a fresh materializer. Use this when
    /// [`open`](Self::open) returns [`MirrorError::LogCorrupt`]: the
    /// regular `open` path fails closed so the caller must opt in.
    ///
    /// This bypasses the open-time recovery scan, atomically replaces
    /// the live log with a freshly-replayed one, and then opens the
    /// materializer pointing at the rebuilt log.
    ///
    /// # Errors
    /// Returns [`MirrorError`] on store, I/O, or encoding failure.
    pub fn rebuild_at(vault_dir: impl AsRef<Path>, conn: &Connection) -> Result<Self, MirrorError> {
        let dir = vault_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let log_path = dir.join("consent.log");
        let cursor_path = dir.join("consent.cursor");
        let lock_path = dir.join("consent.lock");

        // Make sure the lock file exists so we can hold it during the
        // rebuild — without an existing file `LockGuard::acquire` would
        // create one but the order matters for clarity.
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?
            .sync_all()?;
        let _guard = LockGuard::acquire(&lock_path)?;

        let rebuild = rebuild_log_to(&log_path, conn)?;
        let cursor = rebuild.high_water;
        let _ = write_cursor_hint(&cursor_path, cursor);

        Ok(Self {
            log_path,
            cursor_path,
            lock_path,
            cursor,
        })
    }

    /// Truncate the log, reset the cursor to `0`, and replay every event
    /// in the journal. Returns the number of rows written. Holds the
    /// vault lock for the entire call so concurrent ticks block until
    /// the rebuild completes.
    ///
    /// The mirror is never the authority — this operation cannot lose data.
    /// When all goes well the resulting file is byte-identical to one
    /// produced by repeated [`tick`](Self::tick) calls under the same DB.
    ///
    /// # Errors
    /// Returns [`MirrorError`] on store, I/O, or encoding failure.
    pub fn rebuild_from_db(&mut self, conn: &Connection) -> Result<usize, MirrorError> {
        let _guard = LockGuard::acquire(&self.lock_path)?;
        let rebuild = rebuild_log_to(&self.log_path, conn)?;
        // Advance only to the rowid we proved was serialized — never to
        // `max_rowid(conn)`, which could include rows inserted after the
        // replay query and would create an audit gap.
        self.cursor = rebuild.high_water;
        let _ = write_cursor_hint(&self.cursor_path, self.cursor);
        Ok(rebuild.written)
    }

    /// Read the on-disk log line by line, returning the JSON envelope
    /// strings (the full `{"rowid":…,"event":…}` form). Useful for tests
    /// and tooling.
    ///
    /// # Errors
    /// Returns [`MirrorError`] on I/O failure.
    pub fn read_lines(&self) -> Result<Vec<String>, MirrorError> {
        let file = File::open(&self.log_path)?;
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if !line.is_empty() {
                out.push(line);
            }
        }
        Ok(out)
    }

    /// Read the on-disk log and return only the `ConsentEvent` payloads,
    /// dropping the `rowid` envelope wrapper. Convenience for tests and
    /// downstream consumers that don't care about cursor metadata.
    ///
    /// # Errors
    /// Returns [`MirrorError`] on I/O or parse failure.
    pub fn read_events(&self) -> Result<Vec<ConsentEvent>, MirrorError> {
        let mut out = Vec::new();
        for line in self.read_lines()? {
            let env: LogLine = serde_json::from_str(&line)?;
            out.push(env.event);
        }
        Ok(out)
    }
}

/// RAII wrapper around an exclusive `fs4` advisory file lock.
struct LockGuard {
    file: File,
}

impl LockGuard {
    fn acquire(path: &Path) -> Result<Self, MirrorError> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        FileExt::lock_exclusive(&file)?;
        Ok(Self { file })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn log_is_empty(path: &Path) -> std::io::Result<bool> {
    Ok(std::fs::metadata(path)?.len() == 0)
}

fn fsync_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        // Opening a directory read-only and calling sync_all on the
        // resulting handle is the POSIX way to force directory entry
        // durability after a rename or new-file creation.
        let dir = File::open(parent)?;
        dir.sync_all()?;
    }
    Ok(())
}

fn write_cursor_hint(path: &Path, rowid: i64) -> Result<(), MirrorError> {
    let tmp = path.with_extension("cursor.tmp");
    {
        let mut f = File::create(&tmp)?;
        writeln!(f, "{rowid}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    fsync_parent(path)?;
    Ok(())
}

/// Outcome of [`rebuild_log_to`]: the rows actually serialized and the
/// highest rowid among them. The cursor must be set to `high_water`,
/// not to `max_rowid(conn)` — concurrent writers can insert past the
/// replay snapshot, and advancing past unserialized rows would create
/// an append-only audit gap on the next tick.
struct RebuildOutcome {
    written: usize,
    high_water: i64,
}

/// Build the rebuilt log in a temp file, then atomically rename it over
/// the live log. A crash or store error between truncate and replay-
/// complete never leaves the vault with a half-empty audit mirror —
/// readers see either the old log or the fully-replayed new one,
/// nothing in between. Caller must hold the vault lock.
fn rebuild_log_to(log_path: &Path, conn: &Connection) -> Result<RebuildOutcome, MirrorError> {
    let tmp = log_path.with_extension("log.tmp");
    let mut written = 0usize;
    let mut high_water = 0i64;
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        let pending = read_since_rowid(conn, 0)?;
        for (rowid, event) in pending {
            let line = serde_json::to_string(&LogLine { rowid, event })?;
            writeln!(f, "{line}")?;
            high_water = rowid;
            written += 1;
        }
        f.flush()?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, log_path)?;
    fsync_parent(log_path)?;
    Ok(RebuildOutcome {
        written,
        high_water,
    })
}

/// Result of a tail scan: the recovered cursor (if any) and the byte
/// offset we truncated to (if a torn tail was found).
struct RecoveryOutcome {
    cursor: Option<i64>,
    truncated_to_byte_offset: Option<u64>,
}

/// Maximum bytes we will scan from the log tail when recovering the
/// cursor. The log grows unbounded over a vault's lifetime; reading the
/// whole thing on every tick would OOM. 1 MiB is enough to cover ~5 000
/// envelope lines (~200 bytes each), more than enough to find the last
/// well-formed envelope in any realistic crash scenario. A torn tail
/// longer than this is treated as catastrophic corruption — the caller
/// recovers via `rebuild_from_db`.
const RECOVERY_SCAN_BYTES: u64 = 1024 * 1024;

/// Authoritative cursor recovery: scan **the bounded tail** of the log
/// and parse each line until we find a well-formed envelope. Returns
/// the last rowid we successfully read. **Truncates** the file at the
/// byte offset just after the last good envelope's trailing newline if
/// the tail contains malformed bytes (e.g., a torn last line from a
/// crash). This keeps subsequent appends starting on a clean line
/// boundary; without truncation, a partial line would prefix the next
/// envelope and brick deserialization for every future row.
///
/// Memory bound: at most [`RECOVERY_SCAN_BYTES`] of the file tail is
/// loaded into RAM regardless of total log size.
fn recover_cursor_from_log(path: &Path) -> Result<RecoveryOutcome, MirrorError> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RecoveryOutcome {
                cursor: None,
                truncated_to_byte_offset: None,
            });
        }
        Err(e) => return Err(e.into()),
    };
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(RecoveryOutcome {
            cursor: None,
            truncated_to_byte_offset: None,
        });
    }

    // Read at most the last RECOVERY_SCAN_BYTES bytes. If the file is
    // longer, advance the start to a newline boundary so we never split
    // a valid envelope mid-line.
    let scan_len = len.min(RECOVERY_SCAN_BYTES);
    let raw_start = len - scan_len;
    let mut buf = Vec::with_capacity(usize::try_from(scan_len).unwrap_or(usize::MAX));
    file.seek(SeekFrom::Start(raw_start))?;
    file.read_to_end(&mut buf)?;

    let scan_offset = if raw_start > 0 {
        // Skip the partial first line (we sliced into the middle of an
        // envelope). The next byte after the first newline is a clean
        // line boundary.
        match buf.iter().position(|b| *b == b'\n') {
            Some(p) => raw_start + (p as u64) + 1,
            None => len, // no newline in the scan window — nothing parseable
        }
    } else {
        0
    };
    let scan_buf = if raw_start > 0 {
        match buf.iter().position(|b| *b == b'\n') {
            Some(p) => &buf[p + 1..],
            None => &[][..],
        }
    } else {
        &buf[..]
    };

    // Walk lines from `scan_offset`, recording the rowid + end-of-line
    // offset (in absolute file bytes) for the last valid envelope.
    let mut last_good_rowid: Option<i64> = None;
    let mut last_good_end: u64 = scan_offset;
    let mut cursor_byte: u64 = scan_offset;
    for slice in scan_buf.split_inclusive(|b| *b == b'\n') {
        let len_u64 = slice.len() as u64;
        let trimmed: &[u8] = if slice.last() == Some(&b'\n') {
            &slice[..slice.len() - 1]
        } else {
            slice
        };
        if !trimmed.is_empty()
            && let Ok(line) = std::str::from_utf8(trimmed)
            && let Ok(env) = serde_json::from_str::<LogLine>(line)
        {
            last_good_rowid = Some(env.rowid);
            last_good_end = cursor_byte + len_u64;
        }
        cursor_byte += len_u64;
    }

    // If the file ends in something other than a newline-terminated
    // valid envelope, we have a torn tail. Truncate to last_good_end.
    // Note: we only truncate within the scan window. If the entire scan
    // window failed to parse but a clean envelope sits before it, we
    // leave the file alone — `rebuild_from_db` is the correct repair.
    let truncated = if last_good_end < len && last_good_rowid.is_some() {
        let f = OpenOptions::new().write(true).open(path)?;
        f.set_len(last_good_end)?;
        f.sync_all()?;
        fsync_parent(path)?;
        Some(last_good_end)
    } else {
        None
    };

    Ok(RecoveryOutcome {
        cursor: last_good_rowid,
        truncated_to_byte_offset: truncated,
    })
}
