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

/// `WorkflowJobsReader` backed by a single `rusqlite::Connection`.
pub struct SqliteWorkflowJobsReader {
    conn: Mutex<Connection>,
}

impl SqliteWorkflowJobsReader {
    /// Wrap an opened connection. Caller is responsible for opening
    /// against the vault DB that has migrations through 0062 applied.
    #[must_use]
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }
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
