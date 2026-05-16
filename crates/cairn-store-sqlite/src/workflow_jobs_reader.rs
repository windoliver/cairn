//! `SQLite`-backed [`WorkflowJobsReader`] for the lint `workflow_health`
//! check (issue #92, spec §4.8, §4.10).
//!
//! Small read-only queries against `workflow_jobs`. The reader owns its
//! own `rusqlite::Connection` wrapped in a `std::sync::Mutex` — the
//! `lint` verb is sync, so the lock never spans an `.await`. Adapters
//! that already hold a pooled handle can construct a fresh read-only
//! connection against the same DB file and hand it here.

use std::str::FromStr as _;
use std::sync::Mutex;

use cairn_core::contract::job_store::{FailureClass, JobId, JobKind};
use cairn_core::contract::workflow_jobs::{DeadLetterRow, WorkflowJobsReader};
use rusqlite::Connection;

/// Columns added by migration 0062 that every `WorkflowJobsReader`
/// query reads. A DB stuck at 0020 has none of these columns; without
/// a construction-time probe the trait methods silently `unwrap_or`
/// past the missing-column error, returning empty results that look
/// identical to a healthy quiet vault. Probe by name through
/// `PRAGMA table_info(workflow_jobs)` and fail loud.
const REQUIRED_0062_COLUMNS: &[&str] = &["failure_class", "dead_letter_at_ms", "completed_at_ms"];

/// Errors raised by [`SqliteWorkflowJobsReader::new`] when the wrapped
/// connection's `workflow_jobs` schema does not have migration 0062
/// applied.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SqliteWorkflowJobsReaderError {
    /// The `workflow_jobs` table is missing a column the reader queries
    /// on every call (`failure_class`, `dead_letter_at_ms`, or
    /// `completed_at_ms`). Surfaced separately from
    /// [`Self::Backend`] so the operator can see at a glance that the
    /// vault is stuck at migration 0020.
    #[error(
        "workflow_jobs missing column `{name}` (migration 0062 not applied) — \
         run migrations via cairn-store-sqlite first"
    )]
    ColumnMissing {
        /// Column name that the probe expected to find.
        name: &'static str,
    },
    /// Backend error during the column probe.
    #[error("sqlite probe failed: {0}")]
    Backend(String),
}

/// `WorkflowJobsReader` backed by a single `rusqlite::Connection`.
pub struct SqliteWorkflowJobsReader {
    conn: Mutex<Connection>,
}

impl SqliteWorkflowJobsReader {
    /// Wrap an opened connection. Caller is responsible for opening
    /// against the vault DB that has migrations through 0062 applied.
    ///
    /// # Errors
    ///
    /// Returns [`SqliteWorkflowJobsReaderError::ColumnMissing`] when
    /// any of the migration-0062 columns the reader queries are
    /// absent (`failure_class`, `dead_letter_at_ms`,
    /// `completed_at_ms`). The trait methods do not return `Result`,
    /// so the contract is: "if construction succeeds, queries will
    /// not silently mask schema gaps". Callers (e.g. `cairn-cli`
    /// lint) should treat a construction failure the same as "DB
    /// unavailable" — pass `None` for `workflow_jobs` and the
    /// `workflow_health` check no-ops, matching the missing-DB path.
    ///
    /// Returns [`SqliteWorkflowJobsReaderError::Backend`] for any
    /// other `SQLite` failure (e.g. unreadable schema, lock error
    /// during the probe).
    pub fn new(conn: Connection) -> Result<Self, SqliteWorkflowJobsReaderError> {
        // Issue #92 round-6 finding 6.2: probe migration-0062 columns
        // up front. Without this, `dead_letter_rows`, `last_success_ms`,
        // and `dead_letter_count` all swallow the `no such column`
        // error via `.ok().flatten()` / `.unwrap_or_default()` and
        // return empty results. Lint then reports "no workflow issues"
        // on a vault that is silently broken. Fail at construction so
        // the lint verb sees a `None` reader and degrades gracefully
        // with an informative log message.
        let columns = read_column_set(&conn)?;
        for &needed in REQUIRED_0062_COLUMNS {
            if !columns.iter().any(|c| c == needed) {
                return Err(SqliteWorkflowJobsReaderError::ColumnMissing { name: needed });
            }
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

/// Walk `PRAGMA table_info(workflow_jobs)` and collect every column
/// name. Returned unordered — callers scan linearly for required
/// names. Mirrors the same probe shape as `SqliteJobStore::new`.
fn read_column_set(conn: &Connection) -> Result<Vec<String>, SqliteWorkflowJobsReaderError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(workflow_jobs)")
        .map_err(|e| SqliteWorkflowJobsReaderError::Backend(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| SqliteWorkflowJobsReaderError::Backend(e.to_string()))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| SqliteWorkflowJobsReaderError::Backend(e.to_string()))?);
    }
    Ok(out)
}

impl WorkflowJobsReader for SqliteWorkflowJobsReader {
    fn dead_letter_count(&self, kind: Option<&JobKind>) -> usize {
        let Ok(conn) = self.conn.lock() else {
            return 0;
        };
        let count: i64 = match kind {
            Some(k) => conn
                .query_row(
                    "SELECT count(*) FROM workflow_jobs \
                     WHERE dead_letter_at_ms IS NOT NULL AND kind = ?1",
                    rusqlite::params![k.as_str()],
                    |r| r.get(0),
                )
                .unwrap_or(0),
            None => conn
                .query_row(
                    "SELECT count(*) FROM workflow_jobs WHERE dead_letter_at_ms IS NOT NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0),
        };
        usize::try_from(count).unwrap_or(0)
    }

    fn oldest_queued_age_ms(&self, kind: Option<&JobKind>, now_ms: i64) -> Option<i64> {
        let conn = self.conn.lock().ok()?;
        let oldest: Option<i64> = match kind {
            Some(k) => conn
                .query_row(
                    "SELECT min(next_run_at) FROM workflow_jobs \
                     WHERE state = 'queued' AND kind = ?1",
                    rusqlite::params![k.as_str()],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .ok()
                .flatten(),
            None => conn
                .query_row(
                    "SELECT min(next_run_at) FROM workflow_jobs WHERE state = 'queued'",
                    [],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .ok()
                .flatten(),
        };
        oldest.map(|t| now_ms - t)
    }

    fn longest_held_lease_ms(&self, now_ms: i64) -> Option<i64> {
        let conn = self.conn.lock().ok()?;
        let oldest: Option<i64> = conn
            .query_row(
                "SELECT min(lease_expires_at) FROM workflow_jobs WHERE state = 'leased'",
                [],
                |r| r.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten();
        oldest.map(|t| now_ms - t)
    }

    fn last_success_ms(&self, kind: &JobKind) -> Option<i64> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT max(completed_at_ms) FROM workflow_jobs \
             WHERE kind = ?1 AND state = 'done'",
            rusqlite::params![kind.as_str()],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
    }

    fn dead_letter_rows(&self, limit: usize) -> Vec<DeadLetterRow> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT job_id, kind, attempts, failure_class, last_error, dead_letter_at_ms
               FROM workflow_jobs
              WHERE dead_letter_at_ms IS NOT NULL
              ORDER BY dead_letter_at_ms DESC
              LIMIT ?1",
        ) else {
            return Vec::new();
        };
        let limit_bind = i64::try_from(limit).unwrap_or(i64::MAX);
        let Ok(rows) = stmt.query_map(rusqlite::params![limit_bind], |r| {
            let job_id: String = r.get(0)?;
            let kind: String = r.get(1)?;
            let attempts: i64 = r.get(2)?;
            let class: Option<String> = r.get(3)?;
            let last_error: Option<String> = r.get(4)?;
            let dl_at: i64 = r.get(5)?;
            let failure_class = class
                .as_deref()
                .and_then(|s| FailureClass::from_str(s).ok())
                .unwrap_or(FailureClass::Transient);
            Ok(DeadLetterRow {
                job_id: JobId::new(job_id),
                kind: JobKind::new(kind),
                attempts: u32::try_from(attempts).unwrap_or(0),
                failure_class,
                last_error: last_error.unwrap_or_default(),
                dead_letter_at_ms: dl_at,
            })
        }) else {
            return Vec::new();
        };
        rows.flatten().collect()
    }
}
