//! Async-friendly consent log materializer (brief §14, issue #94).
//!
//! Tails the `SQLite` `consent_journal` table by `rowid` and appends each
//! event as a JSON line to `.cairn/consent.log`. The mirror is **never
//! authoritative** — the database is. The on-disk log is rebuildable
//! via [`ConsentLogMaterializer::rebuild_from_db`].
//!
//! Cursor recovery: a sibling file `<log>.cursor` holds the last rowid
//! we successfully appended. On crash mid-append, the next start replays
//! every row strictly greater than the cursor; `SQLite`'s `rowid` is
//! monotonic for `INSERT`-only tables (the journal's append-only triggers
//! enforce that), so replay yields no duplicates and no gaps.
//!
//! The file primitives here are blocking (`std::fs`). Callers that want
//! to run the materializer from a tokio runtime should drive it via
//! [`tokio::task::spawn_blocking`] or schedule it on a dedicated thread.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use cairn_store_sqlite::consent::{max_rowid, read_since_rowid};
use cairn_store_sqlite::error::StoreError;
use rusqlite::Connection;
use thiserror::Error;

/// Errors raised by the materializer.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MirrorError {
    /// I/O against `.cairn/consent.log` or its cursor file.
    #[error("consent.log io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization of a `ConsentEvent`.
    #[error("consent event encode: {0}")]
    Encode(#[from] serde_json::Error),
    /// Underlying store query failure.
    #[error("consent store: {0}")]
    Store(#[from] StoreError),
    /// Cursor file is corrupt — could not parse the persisted rowid.
    #[error("consent.cursor corrupt: {0}")]
    CorruptCursor(String),
}

/// Stateful tail-and-append materializer for a single vault's consent log.
pub struct ConsentLogMaterializer {
    log_path: PathBuf,
    cursor_path: PathBuf,
    cursor: i64,
}

impl ConsentLogMaterializer {
    /// Open the materializer for the vault rooted at `vault_dir`. Creates
    /// the log and cursor files if missing. Recovers the cursor from the
    /// sibling `consent.cursor` file when present.
    ///
    /// # Errors
    /// Returns [`MirrorError`] if the log/cursor parent cannot be created
    /// or the cursor file is unparseable.
    pub fn open(vault_dir: impl AsRef<Path>) -> Result<Self, MirrorError> {
        let dir = vault_dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let log_path = dir.join("consent.log");
        let cursor_path = dir.join("consent.cursor");

        // Ensure both files exist so subsequent appends never need a
        // create-or-open dance.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?
            .sync_all()?;

        let cursor = if cursor_path.exists() {
            let raw = std::fs::read_to_string(&cursor_path)?;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                0
            } else {
                trimmed
                    .parse::<i64>()
                    .map_err(|e| MirrorError::CorruptCursor(e.to_string()))?
            }
        } else {
            std::fs::write(&cursor_path, "0\n")?;
            0
        };

        Ok(Self {
            log_path,
            cursor_path,
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

    /// Path to the cursor sidecar file.
    #[must_use]
    pub fn cursor_path(&self) -> &Path {
        &self.cursor_path
    }

    /// Append every journal row with `rowid > self.cursor` to the log,
    /// `fsync` the log, then update and `fsync` the cursor file. Returns
    /// the number of rows mirrored. Safe to call repeatedly; idempotent
    /// when no new rows exist.
    ///
    /// # Errors
    /// Returns [`MirrorError`] on store, I/O, or encoding failure. On a
    /// partial write the cursor is left at the last successful rowid so
    /// the next call replays the remainder.
    pub fn tick(&mut self, conn: &Connection) -> Result<usize, MirrorError> {
        let pending = read_since_rowid(conn, self.cursor)?;
        if pending.is_empty() {
            return Ok(0);
        }

        let mut log_file = OpenOptions::new().append(true).open(&self.log_path)?;
        let mut written = 0usize;
        let mut high_water = self.cursor;
        for (rowid, event) in pending {
            let line = serde_json::to_string(&event)?;
            writeln!(log_file, "{line}")?;
            high_water = rowid;
            written += 1;
        }
        log_file.flush()?;
        log_file.sync_all()?;

        write_cursor(&self.cursor_path, high_water)?;
        self.cursor = high_water;
        Ok(written)
    }

    /// Truncate the log, reset the cursor to `0`, and replay every event
    /// in the journal. Returns the number of rows written.
    ///
    /// The mirror is never the authority — this operation cannot lose data.
    /// When all goes well the resulting file is byte-identical to one
    /// produced by repeated [`tick`](Self::tick) calls under the same DB.
    ///
    /// # Errors
    /// Returns [`MirrorError`] on store, I/O, or encoding failure.
    pub fn rebuild_from_db(&mut self, conn: &Connection) -> Result<usize, MirrorError> {
        File::create(&self.log_path)?.sync_all()?;
        write_cursor(&self.cursor_path, 0)?;
        self.cursor = 0;
        let written = self.tick(conn)?;
        debug_assert_eq!(self.cursor, max_rowid(conn).unwrap_or(0));
        Ok(written)
    }

    /// Read the on-disk log line by line, returning the JSON strings.
    /// Useful for tests and for tooling that wants to verify the mirror
    /// without re-parsing into typed events.
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
}

fn write_cursor(path: &Path, rowid: i64) -> Result<(), MirrorError> {
    let tmp = path.with_extension("cursor.tmp");
    {
        let mut f = File::create(&tmp)?;
        writeln!(f, "{rowid}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}
