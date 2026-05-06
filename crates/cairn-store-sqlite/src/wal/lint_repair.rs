//! WAL helper for the `lint_repair` op kind (§5.6, brief invariant #5).
//!
//! Opens a new `lint_repair` op in state ISSUED and drives it through the
//! §5.6 FSM: ISSUED → PREPARED → COMMITTED (or → ABORTED). Every mutation
//! to vault state during a `lint --fix-markdown` run must be wrapped in one
//! of these ops so the WAL state-machine invariant is upheld.
//!
//! # Concurrency note
//!
//! `issued_seq` is computed as `COALESCE(MAX(issued_seq), 0) + 1` inside the
//! same transaction as the INSERT. `tokio_rusqlite` serialises all
//! `conn.call(…)` invocations on a single `SQLite` thread, so two concurrent
//! `begin` calls on the *same* `Connection` are naturally ordered and the
//! strictly-advancing trigger will never fire. Callers sharing a single
//! `Arc<Connection>` get this for free.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::params;
use thiserror::Error;
use tokio_rusqlite::Connection;
use uuid::Uuid;

/// Errors from `lint_repair` WAL transitions.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LintRepairWalError {
    /// A database or worker-thread error.
    #[error("wal db error")]
    Db(#[source] tokio_rusqlite::Error),
    /// The requested state transition was rejected by a trigger (illegal FSM
    /// edge, or terminal state is fully immutable).
    #[error("illegal state transition for op {0}")]
    IllegalTransition(String),
    /// The system clock is before the UNIX epoch.
    #[error("system clock pre-epoch")]
    Clock,
}

impl From<tokio_rusqlite::Error> for LintRepairWalError {
    fn from(e: tokio_rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

/// Open a new `lint_repair` op in ISSUED. Returns the `operation_id`.
///
/// # Errors
/// - [`LintRepairWalError::Clock`] if the system clock is before epoch.
/// - [`LintRepairWalError::Db`] on connection/DB failure.
pub async fn begin(
    conn: &Arc<Connection>,
    vault_id: &str,
    holder_id: &str,
    ttl: Duration,
) -> Result<String, LintRepairWalError> {
    const MAX_BEGIN_RETRIES: u8 = 8;
    let op_id = format!("op-{}", Uuid::new_v4());
    let now_ms = system_time_ms()?;
    let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
    let expires_at = now_ms.saturating_add(ttl_ms);

    // Build the placeholder envelope and hash it with blake3.
    let envelope =
        format!(r#"{{"version":1,"kind":"lint_repair","scope":{{"vault_id":"{vault_id}"}}}}"#);
    let target_hash = blake3::hash(envelope.as_bytes()).to_hex().to_string();
    let scope_json = format!(r#"{{"kind":"lint_repair","vault_id":"{vault_id}"}}"#);

    let op_id_clone = op_id.clone();
    let holder_id = holder_id.to_owned();

    // Multi-connection concurrency: another `wal_ops` writer on a
    // different connection can claim the same MAX+1 between our
    // SELECT and INSERT, tripping the UNIQUE/strictly-advancing
    // constraints. SQLite serialises writes at the database level
    // (via BEGIN IMMEDIATE / write lock), so the conflict surfaces
    // as either a UNIQUE failure or a strict-advance trigger
    // violation on commit. Retry up to 8 times with a tiny backoff
    // — the contention window is microseconds, and ULID-shaped
    // op_ids do not collide across retries.
    let mut last_err: Option<rusqlite::Error> = None;
    for attempt in 0..MAX_BEGIN_RETRIES {
        let op_id_attempt = op_id_clone.clone();
        let envelope_attempt = envelope.clone();
        let holder_attempt = holder_id.clone();
        let target_attempt = target_hash.clone();
        let scope_attempt = scope_json.clone();
        let result: Result<Result<(), rusqlite::Error>, tokio_rusqlite::Error> = conn
            .call(move |c| {
                let tx = c.transaction()?;
                let next_seq: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(issued_seq), 0) + 1 FROM wal_ops",
                    [],
                    |row| row.get(0),
                )?;
                let insert = tx.execute(
                    "INSERT INTO wal_ops(\
                       operation_id, issued_seq, kind, state, envelope, \
                       issuer, principal, target_hash, scope_json, plan_ref, \
                       expires_at, signature, issued_at, updated_at, reason) \
                     VALUES (?1, ?2, 'lint_repair', 'ISSUED', ?3, \
                             ?4, NULL, ?5, ?6, NULL, \
                             ?7, '', ?8, ?9, NULL)",
                    params![
                        op_id_attempt,
                        next_seq,
                        envelope_attempt,
                        holder_attempt,
                        target_attempt,
                        scope_attempt,
                        expires_at,
                        now_ms,
                        now_ms,
                    ],
                );
                match insert {
                    Ok(_) => {
                        tx.commit()?;
                        Ok::<_, tokio_rusqlite::Error>(Ok(()))
                    }
                    Err(e) => Ok(Err(e)),
                }
            })
            .await;
        match result? {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) if is_seq_conflict(&e) => {
                // Another writer raced us. Brief backoff scaled
                // by attempt to spread the retry distribution.
                let pause = Duration::from_millis(2u64 << attempt);
                tokio::time::sleep(pause).await;
                last_err = Some(e);
            }
            Err(e) => {
                return Err(LintRepairWalError::Db(tokio_rusqlite::Error::Rusqlite(e)));
            }
        }
    }
    if let Some(e) = last_err {
        return Err(LintRepairWalError::Db(tokio_rusqlite::Error::Rusqlite(e)));
    }

    Ok(op_id)
}

/// Advance a `lint_repair` op from ISSUED → PREPARED.
///
/// # Errors
/// - [`LintRepairWalError::IllegalTransition`] if the op is not in ISSUED
///   state or does not exist.
/// - [`LintRepairWalError::Db`] on connection/DB failure.
pub async fn prepare(conn: &Arc<Connection>, op_id: &str) -> Result<(), LintRepairWalError> {
    transition(conn, op_id, "PREPARED").await
}

/// Advance a `lint_repair` op from PREPARED → COMMITTED.
///
/// # Errors
/// - [`LintRepairWalError::IllegalTransition`] if the op is not in PREPARED
///   state or does not exist.
/// - [`LintRepairWalError::Db`] on connection/DB failure.
pub async fn commit(conn: &Arc<Connection>, op_id: &str) -> Result<(), LintRepairWalError> {
    transition(conn, op_id, "COMMITTED").await
}

/// Advance a `lint_repair` op from ISSUED → REJECTED, recording a reason.
///
/// Used by recovery after `begin()` succeeded but `prepare()` (or any
/// step before PREPARED) failed: the op is non-terminal and cannot be
/// `abort()`ed (PREPARED is the only legal source for ABORTED). REJECTED
/// is the FSM-correct terminal state for an ISSUED op that never made
/// it to PREPARED.
///
/// # Errors
/// - [`LintRepairWalError::IllegalTransition`] if the op is not in ISSUED
///   state or does not exist.
/// - [`LintRepairWalError::Db`] on connection/DB failure.
pub async fn reject(
    conn: &Arc<Connection>,
    op_id: &str,
    reason: &str,
) -> Result<(), LintRepairWalError> {
    let op_id_owned = op_id.to_owned();
    let reason_owned = reason.to_owned();
    let now_ms = system_time_ms()?;

    let rows = conn
        .call(move |c| {
            let n = c.execute(
                "UPDATE wal_ops \
                   SET state = 'REJECTED', updated_at = ?1, reason = ?2 \
                 WHERE operation_id = ?3 AND state = 'ISSUED'",
                params![now_ms, reason_owned, op_id_owned],
            );
            match n {
                Ok(rows) => Ok(rows),
                Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                    if is_fsm_trigger_error(msg) =>
                {
                    Ok(0usize)
                }
                Err(e) => Err(tokio_rusqlite::Error::Rusqlite(e)),
            }
        })
        .await?;

    if rows == 0 {
        return Err(LintRepairWalError::IllegalTransition(op_id.to_owned()));
    }
    Ok(())
}

/// Advance a `lint_repair` op from PREPARED → ABORTED, recording a reason.
///
/// # Errors
/// - [`LintRepairWalError::IllegalTransition`] if the op is not in PREPARED
///   state or does not exist.
/// - [`LintRepairWalError::Db`] on connection/DB failure.
pub async fn abort(
    conn: &Arc<Connection>,
    op_id: &str,
    reason: &str,
) -> Result<(), LintRepairWalError> {
    let op_id_owned = op_id.to_owned();
    let reason_owned = reason.to_owned();
    let now_ms = system_time_ms()?;

    let rows = conn
        .call(move |c| {
            // Single UPDATE: state, updated_at, reason. The
            // wal_ops_envelope_immutable trigger only blocks changes to
            // identity columns (operation_id, issued_seq, kind, envelope,
            // issuer, principal, target_hash, scope_json, plan_ref,
            // expires_at, signature, issued_at) — state, updated_at, and
            // reason are not in that list and are therefore updatable.
            let n = c.execute(
                "UPDATE wal_ops \
                   SET state = 'ABORTED', updated_at = ?1, reason = ?2 \
                 WHERE operation_id = ?3",
                params![now_ms, reason_owned, op_id_owned],
            );
            match n {
                Ok(rows) => Ok(rows),
                Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                    if is_fsm_trigger_error(msg) =>
                {
                    Ok(0usize) // treat trigger abort as 0-rows-affected
                }
                Err(e) => Err(tokio_rusqlite::Error::Rusqlite(e)),
            }
        })
        .await?;

    if rows == 0 {
        return Err(LintRepairWalError::IllegalTransition(op_id.to_owned()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Generic state transition for non-abort edges (no reason column update).
async fn transition(
    conn: &Arc<Connection>,
    op_id: &str,
    new_state: &str,
) -> Result<(), LintRepairWalError> {
    let op_id_owned = op_id.to_owned();
    let new_state = new_state.to_owned();
    let now_ms = system_time_ms()?;

    let rows = conn
        .call(move |c| {
            let n = c.execute(
                "UPDATE wal_ops SET state = ?1, updated_at = ?2 \
                 WHERE operation_id = ?3",
                params![new_state, now_ms, op_id_owned],
            );
            match n {
                Ok(rows) => Ok(rows),
                Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                    if is_fsm_trigger_error(msg) =>
                {
                    Ok(0usize) // treat trigger abort as 0-rows-affected
                }
                Err(e) => Err(tokio_rusqlite::Error::Rusqlite(e)),
            }
        })
        .await?;

    if rows == 0 {
        return Err(LintRepairWalError::IllegalTransition(op_id.to_owned()));
    }
    Ok(())
}

/// Returns `true` when a `wal_ops` INSERT failed because another
/// connection raced us on `issued_seq`. The trigger surfaces this as
/// `"issued_seq must strictly advance MAX(issued_seq)"` and the UNIQUE
/// constraint surfaces it as a UNIQUE violation on `issued_seq`.
fn is_seq_conflict(e: &rusqlite::Error) -> bool {
    let rusqlite::Error::SqliteFailure(_, Some(msg)) = e else {
        return false;
    };
    msg.contains("issued_seq must strictly advance")
        || msg.contains("UNIQUE constraint failed: wal_ops.issued_seq")
}

/// Returns `true` when an `SQLite` error message comes from a §5.6 FSM trigger
/// or the terminal-immutable trigger — both of which should map to
/// [`LintRepairWalError::IllegalTransition`].
fn is_fsm_trigger_error(msg: &str) -> bool {
    msg.contains("wal_ops.state transition not allowed by §5.6 FSM")
        || msg.contains("wal_ops terminal-state rows are fully immutable")
}

/// Current time in milliseconds since the UNIX epoch.
fn system_time_ms() -> Result<i64, LintRepairWalError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LintRepairWalError::Clock)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
