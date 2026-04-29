//! SQLite-backed [`JobStore`] for the default tokio orchestrator.
//!
//! Owns a single `rusqlite::Connection` opened against `.cairn/cairn.db`.
//! Migration 0011 (in `cairn-store-sqlite`) provisions the
//! `workflow_jobs` table. We only read/write that table — schema
//! ownership stays with the store crate, runtime ownership stays here.
//!
//! `rusqlite::Connection` is `Send` but `!Sync`, so we wrap it in
//! `std::sync::Mutex` and execute every call inside
//! `tokio::task::spawn_blocking` to keep the async runtime unblocked.
//! Mutex guards never span an `.await`.

use std::sync::{Arc, Mutex};

use cairn_core::contract::{
    EnqueueRequest, FailDisposition, JobId, JobKind, JobStore, JobStoreError, LeaseToken,
    LeasedJob, RetryPolicy,
};
use rusqlite::{Connection, OptionalExtension, params};

/// A [`JobStore`] backed by a single `SQLite` connection.
pub struct SqliteJobStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteJobStore {
    /// Wrap an opened connection. The caller is responsible for ensuring
    /// migration 0011 has run.
    #[must_use]
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
        }
    }
}

#[async_trait::async_trait]
impl JobStore for SqliteJobStore {
    async fn enqueue(&self, req: EnqueueRequest) -> Result<(), JobStoreError> {
        let conn = Arc::clone(&self.conn);
        // Move all owned data into the blocking task; rusqlite is sync.
        tokio::task::spawn_blocking(move || {
            let mut guard = match conn.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            insert_job(&mut guard, &req)
        })
        .await
        .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))?
    }

    async fn lease(
        &self,
        owner: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<Option<LeasedJob>, JobStoreError> {
        let conn = Arc::clone(&self.conn);
        let owner = owner.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut guard = match conn.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            atomic_lease(&mut guard, &owner, now_ms, lease_duration_ms)
        })
        .await
        .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))?
    }

    async fn heartbeat(
        &self,
        job_id: &JobId,
        lease: &LeaseToken,
        new_expires_at_ms: i64,
    ) -> Result<(), JobStoreError> {
        let conn = Arc::clone(&self.conn);
        let job_id = job_id.clone();
        let lease = lease.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = match conn.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            cas_heartbeat(&mut guard, &job_id, &lease, new_expires_at_ms)
        })
        .await
        .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))?
    }

    async fn complete(&self, job_id: &JobId, lease: &LeaseToken) -> Result<(), JobStoreError> {
        let conn = Arc::clone(&self.conn);
        let job_id = job_id.clone();
        let lease = lease.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = match conn.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            cas_complete(&mut guard, &job_id, &lease)
        })
        .await
        .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))?
    }

    async fn fail(
        &self,
        job_id: &JobId,
        lease: &LeaseToken,
        disposition: FailDisposition,
        last_error: &str,
        now_ms: i64,
    ) -> Result<(), JobStoreError> {
        let conn = Arc::clone(&self.conn);
        let job_id = job_id.clone();
        let lease = lease.clone();
        let last_error = last_error.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut guard = match conn.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            cas_fail(
                &mut guard,
                &job_id,
                &lease,
                disposition,
                &last_error,
                now_ms,
            )
        })
        .await
        .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))?
    }

    async fn reap_expired(&self, now_ms: i64) -> Result<usize, JobStoreError> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = match conn.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            reap(&mut guard, now_ms)
        })
        .await
        .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))?
    }
}

// ---- pure SQL helpers (sync, exercised both directly in tests and via the trait) ----

fn insert_job(conn: &mut Connection, req: &EnqueueRequest) -> Result<(), JobStoreError> {
    let now = req.not_before_ms;
    let res = conn.execute(
        "INSERT INTO workflow_jobs \
            (job_id, kind, payload, state, attempts, max_attempts, queue_key, dedupe_key, \
             next_run_at, lease_owner, lease_expires_at, last_error, enqueued_at, updated_at) \
         VALUES (?, ?, ?, 'queued', 0, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?)",
        params![
            req.job_id.as_str(),
            req.kind.as_str(),
            req.payload,
            i64::from(req.retry.max_attempts),
            req.queue_key,
            req.dedupe_key,
            req.not_before_ms,
            now,
            now,
        ],
    );
    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(translate_enqueue_err(&e, req)),
    }
}

fn translate_enqueue_err(e: &rusqlite::Error, req: &EnqueueRequest) -> JobStoreError {
    let msg = e.to_string();
    let lower = msg.to_lowercase();
    // SQLite UNIQUE-constraint errors name the columns ("workflow_jobs.kind,
    // workflow_jobs.dedupe_key"), not the index. Detect each conflict by
    // the column set.
    let unique_failed = lower.contains("unique constraint failed");
    if unique_failed && lower.contains("dedupe_key")
        && let Some(d) = &req.dedupe_key
    {
        return JobStoreError::DuplicateDedupeKey {
            kind: req.kind.clone(),
            dedupe_key: d.clone(),
        };
    }
    if unique_failed && lower.contains("queue_key")
        && let Some(q) = &req.queue_key
    {
        return JobStoreError::QueueKeyBusy {
            queue_key: q.clone(),
        };
    }
    JobStoreError::Backend(msg)
}

fn atomic_lease(
    conn: &mut Connection,
    owner: &str,
    now_ms: i64,
    lease_duration_ms: i64,
) -> Result<Option<LeasedJob>, JobStoreError> {
    let new_expires = now_ms.saturating_add(lease_duration_ms);
    let tx = conn
        .transaction()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;

    // Pick the oldest queued, ready row.
    let candidate: Option<(String, String, Vec<u8>, i64, i64)> = tx
        .query_row(
            "SELECT job_id, kind, payload, attempts, max_attempts \
               FROM workflow_jobs \
              WHERE state = 'queued' AND next_run_at <= ? \
              ORDER BY next_run_at, enqueued_at \
              LIMIT 1",
            params![now_ms],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;

    let Some((job_id, kind, payload, attempts, max_attempts)) = candidate else {
        return Ok(None);
    };

    // CAS on (job_id, state='queued') so concurrent leasers can't both win.
    let updated = tx
        .execute(
            "UPDATE workflow_jobs \
                SET state = 'leased', lease_owner = ?, lease_expires_at = ?, \
                    attempts = attempts + 1, updated_at = ? \
              WHERE job_id = ? AND state = 'queued'",
            params![owner, new_expires, now_ms, job_id],
        )
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;

    if updated == 0 {
        // Lost the race; surface as "no work right now" — caller will retry.
        tx.rollback()
            .map_err(|e| JobStoreError::Backend(e.to_string()))?;
        return Ok(None);
    }

    tx.commit()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;

    // attempts column post-update.
    let new_attempts = u32::try_from(attempts.saturating_add(1)).unwrap_or(u32::MAX);
    let max_attempts_u32 = u32::try_from(max_attempts).unwrap_or(u32::MAX);

    Ok(Some(LeasedJob {
        job_id: JobId::new(job_id),
        kind: JobKind::new(kind),
        payload,
        attempts: new_attempts,
        retry: RetryPolicy {
            max_attempts: max_attempts_u32,
            ..RetryPolicy::DEFAULT
        },
        lease: LeaseToken {
            owner: owner.to_owned(),
            expires_at_ms: new_expires,
        },
    }))
}

fn cas_heartbeat(
    conn: &mut Connection,
    job_id: &JobId,
    lease: &LeaseToken,
    new_expires_at_ms: i64,
) -> Result<(), JobStoreError> {
    let updated = conn
        .execute(
            "UPDATE workflow_jobs \
                SET lease_expires_at = ?, updated_at = ? \
              WHERE job_id = ? AND state = 'leased' \
                AND lease_owner = ? AND lease_expires_at = ?",
            params![
                new_expires_at_ms,
                new_expires_at_ms,
                job_id.as_str(),
                lease.owner,
                lease.expires_at_ms,
            ],
        )
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    if updated == 0 {
        return Err(JobStoreError::LeaseLost {
            job_id: job_id.clone(),
        });
    }
    Ok(())
}

fn cas_complete(
    conn: &mut Connection,
    job_id: &JobId,
    lease: &LeaseToken,
) -> Result<(), JobStoreError> {
    let updated = conn
        .execute(
            "UPDATE workflow_jobs \
                SET state = 'done', lease_owner = NULL, lease_expires_at = NULL, updated_at = ? \
              WHERE job_id = ? AND state = 'leased' \
                AND lease_owner = ? AND lease_expires_at = ?",
            params![
                lease.expires_at_ms,
                job_id.as_str(),
                lease.owner,
                lease.expires_at_ms,
            ],
        )
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    if updated == 0 {
        return Err(JobStoreError::LeaseLost {
            job_id: job_id.clone(),
        });
    }
    Ok(())
}

fn cas_fail(
    conn: &mut Connection,
    job_id: &JobId,
    lease: &LeaseToken,
    disposition: FailDisposition,
    last_error: &str,
    now_ms: i64,
) -> Result<(), JobStoreError> {
    let tx = conn
        .transaction()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    let row: Option<(i64, i64)> = tx
        .query_row(
            "SELECT attempts, max_attempts FROM workflow_jobs \
              WHERE job_id = ? AND state = 'leased' \
                AND lease_owner = ? AND lease_expires_at = ?",
            params![job_id.as_str(), lease.owner, lease.expires_at_ms],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;

    let Some((attempts, max_attempts)) = row else {
        tx.rollback()
            .map_err(|e| JobStoreError::Backend(e.to_string()))?;
        return Err(JobStoreError::LeaseLost {
            job_id: job_id.clone(),
        });
    };

    let exhausted = attempts >= max_attempts;
    let terminal = matches!(disposition, FailDisposition::Permanent) || exhausted;

    let res = if terminal {
        tx.execute(
            "UPDATE workflow_jobs \
                SET state = 'failed', lease_owner = NULL, lease_expires_at = NULL, \
                    last_error = ?, updated_at = ? \
              WHERE job_id = ? AND state = 'leased' \
                AND lease_owner = ? AND lease_expires_at = ?",
            params![
                last_error,
                now_ms,
                job_id.as_str(),
                lease.owner,
                lease.expires_at_ms,
            ],
        )
    } else {
        // Compute next_run_at using RetryPolicy::DEFAULT — caller of
        // enqueue passed a policy; we persisted only max_attempts in
        // schema 0011, so retry cadence is currently fixed. The
        // scheduler can override by passing a different `now_ms` if a
        // custom backoff is desired in P1.
        let attempt_for_delay = u32::try_from(attempts).unwrap_or(u32::MAX);
        let delay = u64::from(RetryPolicy::DEFAULT.delay_for_attempt(attempt_for_delay));
        let next_run = now_ms.saturating_add(i64::try_from(delay).unwrap_or(i64::MAX));
        tx.execute(
            "UPDATE workflow_jobs \
                SET state = 'queued', lease_owner = NULL, lease_expires_at = NULL, \
                    last_error = ?, next_run_at = ?, updated_at = ? \
              WHERE job_id = ? AND state = 'leased' \
                AND lease_owner = ? AND lease_expires_at = ?",
            params![
                last_error,
                next_run,
                now_ms,
                job_id.as_str(),
                lease.owner,
                lease.expires_at_ms,
            ],
        )
    };
    let updated = res.map_err(|e| JobStoreError::Backend(e.to_string()))?;
    if updated == 0 {
        tx.rollback()
            .map_err(|e| JobStoreError::Backend(e.to_string()))?;
        return Err(JobStoreError::LeaseLost {
            job_id: job_id.clone(),
        });
    }
    tx.commit()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    Ok(())
}

fn reap(conn: &mut Connection, now_ms: i64) -> Result<usize, JobStoreError> {
    let updated = conn
        .execute(
            "UPDATE workflow_jobs \
                SET state = 'queued', lease_owner = NULL, lease_expires_at = NULL, \
                    next_run_at = ?, updated_at = ? \
              WHERE state = 'leased' AND lease_expires_at <= ?",
            params![now_ms, now_ms, now_ms],
        )
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    Ok(updated)
}
