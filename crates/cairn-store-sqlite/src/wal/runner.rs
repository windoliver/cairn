//! Generic WAL step runner — drives a [`StepGraph`] against `wal_steps`.
//!
//! The runner is responsible for:
//! - Persisting a `wal_steps` row per attempt (PENDING → DONE/FAILED).
//! - Idempotent re-entry: a step in `Done` state is skipped.
//! - Retry policy: up to [`MAX_STEP_ATTEMPTS`] attempts with exponential
//!   backoff (100 ms / 400 ms / 1600 ms per brief §5.6).
//!
//! Step bodies (the actual side-effects) are supplied by callers via the
//! [`StepBody`] trait. This crate ships no production bodies for `upsert`
//! / `forget_record` / `expire` — those land in #57 and #58.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::wal::{MAX_STEP_ATTEMPTS, OperationId, StepDef, StepGraph, StepState};
use rusqlite::Transaction;
use thiserror::Error;
use tokio_rusqlite::Connection;

/// Errors a step body can return.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StepBodyError {
    /// The body decided this attempt should fail; runner will retry up to
    /// the policy ceiling.
    #[error("step body failed: {0}")]
    Failed(String),
    /// Underlying storage / transaction error. Treated as a body failure
    /// for retry purposes.
    #[error("step body storage error")]
    Storage(#[source] rusqlite::Error),
}

/// A side-effect body invoked by the runner, once per step ord per attempt.
///
/// Implementations run inside the same `SQLite` transaction the runner
/// opened to record the `wal_steps` row update. Returning `Ok(())` causes
/// the runner to commit the transaction with the row marked `Done`.
/// Returning `Err` rolls back the body's writes (the runner re-opens a
/// fresh transaction to record `Failed`).
pub trait StepBody: Send + Sync {
    /// Run the side-effect for `step.ord`.
    ///
    /// # Errors
    /// Any error here causes the runner to mark the step `Failed` and
    /// schedule a retry (up to [`MAX_STEP_ATTEMPTS`]).
    fn run(
        &self,
        tx: &mut Transaction<'_>,
        op_id: &OperationId,
        step: &StepDef,
    ) -> Result<(), StepBodyError>;
}

/// Errors from the runner.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RunnerError {
    /// A step exhausted [`MAX_STEP_ATTEMPTS`] without succeeding. The
    /// `wal_steps` row is durably `Failed` with `attempts == MAX`. The
    /// caller (recovery) is responsible for the op-level transition to
    /// `Aborted`.
    #[error("step {step_ord} exhausted retries for op {op_id}")]
    Exhausted {
        /// Op id.
        op_id: OperationId,
        /// Step ord that exhausted.
        step_ord: u32,
    },
    /// `SQLite` / connection failure outside a step body.
    #[error("runner storage error")]
    Storage(#[source] tokio_rusqlite::Error),
}

impl From<tokio_rusqlite::Error> for RunnerError {
    fn from(e: tokio_rusqlite::Error) -> Self {
        Self::Storage(e)
    }
}

/// Drives steps `start_ord..graph.len()` against the given connection.
///
/// `body.run` is called inside a transaction the runner opens; on Ok the
/// transaction commits with the `wal_steps` row marked `Done`. On Err the
/// transaction is rolled back, then a separate transaction marks the row
/// `Failed` with the error message.
///
/// Idempotent re-entry: a step already in `Done` state is skipped — its
/// body is never re-invoked.
///
/// # Errors
/// - [`RunnerError::Exhausted`] when a step fails [`MAX_STEP_ATTEMPTS`] times.
/// - [`RunnerError::Storage`] on connection failure.
pub async fn run_from(
    conn: &Arc<Connection>,
    graph: &'static StepGraph,
    op_id: &OperationId,
    start_ord: u32,
    body: Arc<dyn StepBody>,
) -> Result<(), RunnerError> {
    // graph.len() <= 7 in the scaffold; cast is safe.
    #[allow(clippy::cast_possible_truncation)]
    let total = graph.len() as u32;
    for ord in start_ord..total {
        // `ord < total == graph.len()` by construction, so `graph.step(ord)`
        // always returns `Some`. Defensive `let-else` avoids `expect()`
        // (denied by `clippy::expect_used` outside tests); falling through
        // to `break` here would only fire on a future change to `StepGraph`.
        let Some(step_ref) = graph.step(ord) else {
            break;
        };
        let step = *step_ref;
        run_one_step(conn, op_id, &step, &body).await?;
    }
    Ok(())
}

async fn run_one_step(
    conn: &Arc<Connection>,
    op_id: &OperationId,
    step: &StepDef,
    body: &Arc<dyn StepBody>,
) -> Result<(), RunnerError> {
    // 1. Skip if already DONE.
    if read_step_state(conn, op_id, step.ord).await? == Some(StepState::Done) {
        return Ok(());
    }

    // 2. Retry until the durable, lifetime-cumulative attempts count reaches
    // MAX_STEP_ATTEMPTS. The loop counter is only for local backoff timing.
    for attempt in 1.. {
        match try_one_attempt(conn, op_id, step, body).await? {
            AttemptOutcome::Done => return Ok(()),
            AttemptOutcome::Failed { attempts } if attempts >= MAX_STEP_ATTEMPTS => {
                return Err(RunnerError::Exhausted {
                    op_id: op_id.clone(),
                    step_ord: step.ord,
                });
            }
            AttemptOutcome::Failed { .. } => {
                tokio::time::sleep(backoff_for(attempt)).await;
            }
        }
    }
    // Loop above always returns; this is unreachable.
    Err(RunnerError::Exhausted {
        op_id: op_id.clone(),
        step_ord: step.ord,
    })
}

#[derive(Debug, Clone, Copy)]
enum AttemptOutcome {
    Done,
    Failed { attempts: u32 },
}

/// Backoff durations per brief §5.6 retry policy: 100 ms / 400 ms / 1600 ms.
fn backoff_for(attempt: u32) -> Duration {
    match attempt {
        1 => Duration::from_millis(100),
        2 => Duration::from_millis(400),
        _ => Duration::from_millis(1600),
    }
}

async fn read_step_state(
    conn: &Arc<Connection>,
    op_id: &OperationId,
    ord: u32,
) -> Result<Option<StepState>, RunnerError> {
    let op = op_id.as_str().to_owned();
    let state_str: Option<String> = conn
        .call(move |c| {
            let row: rusqlite::Result<String> = c.query_row(
                "SELECT state FROM wal_steps WHERE operation_id = ?1 AND step_ord = ?2",
                rusqlite::params![op, ord],
                |r| r.get(0),
            );
            match row {
                Ok(s) => Ok(Some(s)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(tokio_rusqlite::Error::Rusqlite(e)),
            }
        })
        .await?;
    Ok(state_str.map(|s| match s.as_str() {
        "PENDING" => StepState::Pending,
        "DONE" => StepState::Done,
        "FAILED" => StepState::Failed,
        "COMPENSATED" => StepState::Compensated,
        // wal_steps.state is CHECK-constrained — any other value is a
        // schema invariant violation, not a runtime case.
        other => panic!("invariant: unknown wal_steps.state {other:?}"),
    }))
}

async fn try_one_attempt(
    conn: &Arc<Connection>,
    op_id: &OperationId,
    step: &StepDef,
    body: &Arc<dyn StepBody>,
) -> Result<AttemptOutcome, RunnerError> {
    let op = op_id.as_str().to_owned();
    let step_owned = *step;
    let body = Arc::clone(body);
    let now = now_ms();
    let op_id_for_body = op_id.clone();

    let outcome: AttemptOutcome = conn
        .call(move |c| {
            // Phase 1: open a transaction, upsert the wal_steps row to
            // PENDING, run the body. Commit on Ok; drop (rollback) on Err.
            let body_result: Result<(), StepBodyError> = {
                let mut tx = c.transaction()?;
                // `attempts` is lifetime-cumulative across runner re-entries.
                // For a brand-new row we INSERT with `1`; on conflict we
                // increment the existing value rather than overwriting with
                // the per-invocation loop counter (which only counts attempts
                // within this process). `started_at` is preserved across
                // attempts via COALESCE so diagnostics keep the first-start
                // timestamp. The per-invocation `attempt` parameter still
                // drives `backoff_for(attempt)` — only its use in the
                // `attempts` column changes here.
                tx.execute(
                    "INSERT INTO wal_steps \
                       (operation_id, step_ord, step_kind, state, attempts, started_at) \
                     VALUES (?1, ?2, ?3, 'PENDING', 1, ?4) \
                     ON CONFLICT(operation_id, step_ord) DO UPDATE SET \
                       state = 'PENDING', \
                       attempts = wal_steps.attempts + 1, \
                       started_at = COALESCE(wal_steps.started_at, ?4)",
                    rusqlite::params![op, step_owned.ord, step_owned.name, now],
                )?;
                let r = body.run(&mut tx, &op_id_for_body, &step_owned);
                if r.is_ok() {
                    let finished = now_ms();
                    tx.execute(
                        "UPDATE wal_steps SET state = 'DONE', finished_at = ?1 \
                         WHERE operation_id = ?2 AND step_ord = ?3",
                        rusqlite::params![finished, op, step_owned.ord],
                    )?;
                    tx.commit()?;
                } // else: tx is dropped here, rolling back any body writes
                // and (importantly) the PENDING upsert above. Phase 2
                // re-stamps the row in a fresh transaction.
                r
            };

            match body_result {
                Ok(()) => Ok::<_, tokio_rusqlite::Error>(AttemptOutcome::Done),
                Err(err) => {
                    // Phase 2: in a fresh txn, persist the FAILED state +
                    // attempt count. Use INSERT … ON CONFLICT because the
                    // earlier PENDING upsert was rolled back, so the row
                    // may not exist yet on the very first attempt.
                    let msg = match &err {
                        StepBodyError::Failed(m) => m.clone(),
                        StepBodyError::Storage(e) => format!("storage: {e}"),
                    };
                    let finished = now_ms();
                    // Phase-1's PENDING upsert was rolled back when the
                    // body errored, so the row may not exist yet (very first
                    // attempt) — INSERT with `attempts = 1`. On conflict,
                    // increment the existing value so the counter is
                    // lifetime-cumulative. No double-counting risk: Phase 1
                    // and Phase 2 never both commit per attempt (Phase 2
                    // only runs when Phase 1's tx was dropped). `started_at`
                    // is intentionally absent from the DO UPDATE list so the
                    // original first-start timestamp survives.
                    c.execute(
                        "INSERT INTO wal_steps \
                           (operation_id, step_ord, step_kind, state, attempts, last_error, started_at, finished_at) \
                         VALUES (?1, ?2, ?3, 'FAILED', 1, ?4, ?5, ?6) \
                         ON CONFLICT(operation_id, step_ord) DO UPDATE SET \
                           state = 'FAILED', \
                           attempts = wal_steps.attempts + 1, \
                           last_error = ?4, \
                           finished_at = ?6",
                        rusqlite::params![
                            op,
                            step_owned.ord,
                            step_owned.name,
                            msg,
                            now,
                            finished
                        ],
                    )?;
                    let attempts = c.query_row(
                        "SELECT attempts FROM wal_steps \
                         WHERE operation_id = ?1 AND step_ord = ?2",
                        rusqlite::params![op, step_owned.ord],
                        |row| row.get::<_, u32>(0),
                    )?;
                    Ok::<_, tokio_rusqlite::Error>(AttemptOutcome::Failed { attempts })
                }
            }
        })
        .await?;

    Ok(outcome)
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}
