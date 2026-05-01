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
use cairn_store_sqlite::consent::{max_rowid, read_since_rowid};
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
    /// Per-mirror sidecar tracking which `consent_mirror_resets`
    /// `migration_id`s this mirror has already replayed. Multiple mirrors
    /// (different `vault_dir`s) sharing one DB must each replay
    /// independently — DB-level consumption would let the first mirror
    /// silently steal the marker from its peers (Phase-B finding 2).
    resets_consumed_path: PathBuf,
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
        let resets_consumed_path = dir.join("consent.mirror_resets_consumed");

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
            resets_consumed_path,
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

        // CORRUPTION-FIRST INVARIANT (Phase-B finding 1):
        //
        // The log-corruption check runs BEFORE any 0021-style reset
        // replay. A pending `consent_mirror_resets` marker is *not* a
        // license to silently overwrite a corrupt log — operators must
        // see `LogCorrupt` and explicitly opt into recovery via
        // `rebuild_at`. Reset auto-replay is reserved for the
        // known-good-log path so the audit trail remains visible at
        // exactly the moment something went wrong.
        //
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
        // Cap any recovered cursor at the journal's current high-water
        // mark. A log restored from another vault (or tampered) could
        // contain an envelope whose rowid is greater than any row in
        // *this* DB; honoring that cursor would skip every real row up
        // to the bogus one. We treat any rowid > max_rowid(conn) as
        // corruption.
        let db_high = max_rowid(conn)?;
        match recover_cursor_from_log(&self.log_path)?.cursor {
            Some(rowid) if rowid > db_high => return Err(MirrorError::LogCorrupt),
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

        // Migration 0021 promoted legacy `kind IS NULL` rows in the
        // consent_journal to event-shape rows preserving their original
        // rowid. Existing vaults' cursor sidecar may already point ABOVE
        // those legacy rowids (the mirror tailed only event-kind rows
        // pre-0021 via the `kind IS NOT NULL` predicate in
        // `read_since_rowid`), so a plain `tick()` would silently skip
        // every newly-visible legacy row and brick brief §14 "no gaps".
        //
        // Migration 0021 inserts a `(migration_id, applied_at,
        // consumed=0, db_nonce)` row into `consent_mirror_resets`.
        // Sidecar consumption is bound to `db_nonce` (round-12 finding;
        // applied_at is second-resolution and DB copies preserve it).
        // Here we look
        // for any reset rows this mirror has not yet consumed (the
        // sidecar at `consent.mirror_resets_consumed` is the per-mirror
        // source of truth — see Phase-B finding 2); on finding one we
        // replay from rowid 0 via `rebuild_log_to` (the same atomic
        // truncate-and-replay path `rebuild_from_db` uses), update the
        // in-memory cursor to the rebuild's high-water mark, and append
        // the migration_id to the local sidecar. The lock guard above
        // serializes us against any peer mirror.
        let pending_resets = read_pending_mirror_resets(
            conn,
            &self.resets_consumed_path,
            self.cursor,
            &self.log_path,
        )?;
        if !pending_resets.is_empty() {
            let rebuild = rebuild_log_to(&self.log_path, conn)?;
            self.cursor = rebuild.high_water;
            let _ = write_cursor_hint(&self.cursor_path, self.cursor);
            // Watermark: the highest rowid the rebuild actually serialized.
            // Future ticks compare the live cursor against this value; a
            // cursor that regresses below the watermark (vault rollback,
            // backup restore without DB) forces a replay even though the
            // sidecar still claims "consumed" — see round-4 medium finding.
            // Line count: the live log's line count *after* the rebuild —
            // a tail-only truncation that leaves the watermark envelope
            // parseable would still satisfy `cursor >= watermark`, so we
            // also bind to line count (round-13 medium finding).
            let watermark = rebuild.high_water;
            let line_count = count_log_lines(&self.log_path)?;
            let consumed: Vec<(i64, String, i64, u64)> = pending_resets
                .into_iter()
                .map(|(id, nonce)| (id, nonce, watermark, line_count))
                .collect();
            mark_mirror_resets_consumed(&self.resets_consumed_path, &consumed)?;
            return Ok(rebuild.written);
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
        let resets_consumed_path = dir.join("consent.mirror_resets_consumed");

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

        // Consume any pending mirror-reset markers — `rebuild_at` is the
        // operator-driven recovery path and supersedes any pending
        // 0021-style instruction to replay from zero. Per-mirror
        // consumption (Phase-B finding 2): peers with their own
        // sidecars still see and replay the marker independently.
        let pending = read_pending_mirror_resets(conn, &resets_consumed_path, cursor, &log_path)?;
        if !pending.is_empty() {
            let watermark = rebuild.high_water;
            let line_count = count_log_lines(&log_path)?;
            let consumed: Vec<(i64, String, i64, u64)> = pending
                .into_iter()
                .map(|(id, nonce)| (id, nonce, watermark, line_count))
                .collect();
            mark_mirror_resets_consumed(&resets_consumed_path, &consumed)?;
        }

        Ok(Self {
            log_path,
            cursor_path,
            lock_path,
            resets_consumed_path,
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
        // Consume any pending mirror-reset markers — an explicit rebuild
        // supersedes any pending 0021-style replay instruction.
        // Consumption is per-mirror (Phase-B finding 2): the sidecar
        // holds the local truth, leaving peers free to replay.
        let pending = read_pending_mirror_resets(
            conn,
            &self.resets_consumed_path,
            self.cursor,
            &self.log_path,
        )?;
        if !pending.is_empty() {
            let watermark = rebuild.high_water;
            let line_count = count_log_lines(&self.log_path)?;
            let consumed: Vec<(i64, String, i64, u64)> = pending
                .into_iter()
                .map(|(id, nonce)| (id, nonce, watermark, line_count))
                .collect();
            mark_mirror_resets_consumed(&self.resets_consumed_path, &consumed)?;
        }
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

/// Read every mirror-reset marker that this mirror has not yet
/// replayed. Migration 0021 inserts a `consent_mirror_resets` row to
/// instruct any vault that already has a tail-cursor past pre-0021
/// legacy rowids to replay the journal from rowid 0 once after upgrade.
///
/// Per-mirror semantics (Phase-B finding 2): the DB row records "a
/// reset was needed at migration N"; the *consumption* sidecar at
/// `resets_consumed_path` records "this mirror has handled it." Two
/// mirrors at different `vault_dir`s sharing the same DB therefore each
/// replay independently. The `consumed` column on the DB row is now
/// reserved-for-no-use — schema 0021 still defines it (changing the
/// schema would break verifier fingerprints), but we ignore it.
///
/// DB-instance binding (round-12 finding): consumption is keyed by the
/// `(migration_id, db_nonce)` tuple. `db_nonce` is a 32-char hex string
/// minted by `lower(hex(randomblob(16)))` at migration apply time, so
/// two distinct DB schema instances (e.g., a backup restore vs. the
/// original, or two DBs migrated in the same wall-clock second) hold
/// different nonces for the same `migration_id`. `applied_at` (round-3)
/// proved insufficient: it is second-resolution and DB copies preserve
/// it verbatim. A stale sidecar that records the source DB's nonce
/// therefore cannot match the freshly-minted nonce of a separate DB.
///
/// Log-state binding (round-4 finding, strengthened round-13): each
/// sidecar entry carries the `watermark_rowid` (highest rowid the prior
/// replay serialized) AND the `line_count` (number of lines the log
/// held immediately after that replay). For consumption to count, the
/// current `cursor` must be `>= watermark` AND the live log's line
/// count must be `>= recorded line_count`. The watermark check alone
/// is not proof: `recover_cursor_from_log` only inspects the LAST
/// well-formed envelope in a bounded scan window, so a log that was
/// truncated or replaced between consumption and now — but still has
/// the watermark envelope at its tail — would falsely satisfy
/// `cursor >= watermark`. Line-count grows monotonically with the
/// cursor (each replayed envelope is one line), so a tick-time count
/// below the recorded count proves the log has been rolled back even
/// when the tail still parses. Sidecar lines that lack the
/// `line_count` field (round-4 three-field, round-3 two-field, round-2 bare
/// `migration_id`) or that carry the round-3 `applied_at`-bound
/// middle field instead of a hex `db_nonce` are all treated the same
/// way: unknown state → unsafe to honor → replay.
fn read_pending_mirror_resets(
    conn: &Connection,
    resets_consumed_path: &Path,
    cursor: i64,
    log_path: &Path,
) -> Result<Vec<(i64, String)>, MirrorError> {
    // Tolerate the table being absent (e.g., a legacy connection that
    // somehow never ran 0021): no resets to honor.
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema \
             WHERE type = 'table' AND name = 'consent_mirror_resets'",
            [],
            |r| r.get(0),
        )
        .map_err(StoreError::from)?;
    if exists == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT migration_id, db_nonce FROM consent_mirror_resets")
        .map_err(StoreError::from)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(StoreError::from)?;
    let mut all = Vec::new();
    for row in rows {
        all.push(row.map_err(StoreError::from)?);
    }
    let consumed = read_resets_consumed_sidecar(resets_consumed_path)?;
    let live_line_count = count_log_lines(log_path)?;
    all.retain(|tuple| {
        // A DB row is considered consumed only when there's a sidecar
        // entry for the matching (migration_id, db_nonce) AND the
        // recorded watermark is still <= the live cursor AND the
        // recorded line_count is still <= the live log's line count.
        // A cursor below the watermark or a line count below the
        // recorded count means the log was rolled back behind the
        // sidecar's claim — force replay. The line-count check guards
        // against a tail-only truncation that leaves the watermark
        // envelope parseable while erasing earlier rows (round-13
        // medium finding).
        !consumed.iter().any(|(id, nonce, watermark, line_count)| {
            (*id, nonce.as_str()) == (tuple.0, tuple.1.as_str())
                && cursor >= *watermark
                && live_line_count >= *line_count
        })
    });
    Ok(all)
}

/// Count newline-terminated lines in the log. Returns 0 if the file is
/// missing. Used to detect a log rollback that the watermark check
/// alone cannot catch — a truncate-to-tail that preserves the last
/// well-formed envelope leaves `recover_cursor_from_log` reporting the
/// recorded watermark, but the line count drops.
fn count_log_lines(path: &Path) -> std::io::Result<u64> {
    match File::open(path) {
        Ok(file) => {
            let reader = BufReader::new(file);
            let mut n: u64 = 0;
            for line in reader.lines() {
                let _ = line?;
                n = n.saturating_add(1);
            }
            Ok(n)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

/// Mark the given `(migration_id, db_nonce, watermark, line_count)`
/// tuples as consumed by this mirror. Persists to the per-mirror
/// sidecar (atomic tmp+rename, fsynced, parent dir fsynced) — the DB
/// row is intentionally untouched so peer mirrors can still observe
/// and replay it.
///
/// `watermark` is the highest rowid the replay that produced this
/// consumption actually serialized; `line_count` is the live log's
/// line count at the moment of consumption (must be measured AFTER
/// `rebuild_log_to`). Subsequent ticks compare the live cursor and
/// live line count against these values to detect post-consumption
/// rollbacks (round-4 medium finding + round-13 strengthening). A new
/// entry replaces any older entry for the same `(migration_id,
/// db_nonce)` so both fields always reflect the most recent successful
/// replay.
fn mark_mirror_resets_consumed(
    resets_consumed_path: &Path,
    entries: &[(i64, String, i64, u64)],
) -> Result<(), MirrorError> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut existing = read_resets_consumed_sidecar(resets_consumed_path)?;
    for entry in entries {
        // Replace any prior entry for the same (migration_id,
        // db_nonce) — keep one row per DB-bound marker, with the
        // newest watermark + line_count winning.
        existing.retain(|(id, nonce, _, _)| (*id, nonce.as_str()) != (entry.0, entry.1.as_str()));
        existing.push(entry.clone());
    }
    write_resets_consumed_sidecar(resets_consumed_path, &existing)
}

/// Read the per-mirror reset-consumption sidecar. Missing file → empty
/// vec (no resets consumed yet).
///
/// Format: each non-empty line is
/// `migration_id:db_nonce:watermark_rowid:line_count`, where
/// `migration_id`, `watermark_rowid`, and `line_count` parse as
/// integers (`line_count` as `u64`) and `db_nonce` is a 32-char lower
/// hex string (minted by migration 0021 via `randomblob(16)`).
/// `watermark_rowid` is the highest rowid the replay that produced
/// this entry serialized; `line_count` is the live log's line count
/// immediately after that replay. The caller compares both against
/// the live state to detect post-consumption log rollbacks (round-4
/// watermark + round-13 line-count strengthening: a tail-only
/// truncation that leaves the watermark envelope parseable would
/// satisfy the watermark check but drop the line count).
///
/// Tolerance: lines that don't match the expected four-field shape
/// — including the round-2 bare-`migration_id` format, the round-3
/// two-field `migration_id:applied_at`, the round-4 three-field
/// `migration_id:applied_at:watermark` (middle decimal-parses), and
/// the round-12 three-field `migration_id:db_nonce:watermark` (no
/// `line_count`) — are silently skipped. A skipped line cannot match
/// any DB tuple, which forces the next tick to replay (the safe
/// default; the DB is still the source of truth for "a reset was
/// needed"). I/O errors propagate; parse errors do not.
fn read_resets_consumed_sidecar(path: &Path) -> Result<Vec<(i64, String, i64, u64)>, MirrorError> {
    match File::open(path) {
        Ok(file) => {
            let reader = BufReader::new(file);
            let mut out = Vec::new();
            for line in reader.lines() {
                let line = line?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = trimmed.splitn(4, ':').collect();
                if parts.len() < 4 {
                    // Legacy 1/2/3-field formats lack a line_count
                    // (and possibly a watermark / nonce); treat as
                    // unknown log state and skip so the next tick
                    // force-replays.
                    continue;
                }
                let nonce = parts[1].trim();
                // Reject the round-4 `applied_at`-bound format: its
                // middle field is a decimal integer (millis-since-epoch),
                // never a hex nonce. A 32-char lowercase hex string
                // never parses cleanly as i64, so an i64::parse success
                // on the middle field is a reliable round-4 signal.
                if nonce.parse::<i64>().is_ok() {
                    continue;
                }
                if let (Ok(id), Ok(watermark), Ok(line_count)) = (
                    parts[0].trim().parse::<i64>(),
                    parts[2].trim().parse::<i64>(),
                    parts[3].trim().parse::<u64>(),
                ) {
                    out.push((id, nonce.to_owned(), watermark, line_count));
                }
            }
            Ok(out)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// Write the consumed sidecar atomically. Same pattern as
/// `write_cursor_hint`: write tmp, fsync, rename, fsync parent. Each
/// entry is serialized as
/// `migration_id:db_nonce:watermark_rowid:line_count` on its own line.
fn write_resets_consumed_sidecar(
    path: &Path,
    entries: &[(i64, String, i64, u64)],
) -> Result<(), MirrorError> {
    let tmp = path.with_extension("mirror_resets_consumed.tmp");
    {
        let mut f = File::create(&tmp)?;
        for (id, nonce, watermark, line_count) in entries {
            writeln!(f, "{id}:{nonce}:{watermark}:{line_count}")?;
        }
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
