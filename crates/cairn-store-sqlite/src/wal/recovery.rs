//! Boot-time WAL recovery (brief §5.6 "Boot-time recovery").
//!
//! Reads `wal_ops` + `wal_steps` from the open `SQLite` connection, calls
//! `cairn_core::wal::decide_recovery` for each non-terminal op, and applies
//! the returned [`RecoveryDecision`].
//!
//! Decision-only mode: when no [`StepBodyRegistry`] is configured, the
//! recovery routine handles the decisions that don't require a body
//! ([`RecoveryDecision::NoOp`], `FinalizeRejected`, `FinalizeCommitted`)
//! and emits a `tracing::warn!` for each `Resume` / `AbortAndCompensate`
//! decision it cannot complete. This is the v0.1 default until #57/#58
//! land.

use std::sync::Arc;

use cairn_core::wal::{
    OpSnapshot, OpState, OperationId, RecoveryDecision, StepRow, StepState, WalKind,
    decide_recovery, graph_for,
};
use thiserror::Error;
use tokio_rusqlite::Connection;
use tracing::warn;

use crate::wal::runner::{self, RunnerError, StepBody};

/// Errors from [`recover_pending`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RecoveryError {
    /// Underlying storage failure.
    #[error("recovery storage error")]
    Storage(#[source] tokio_rusqlite::Error),
    /// `wal_ops.state` or `wal_ops.kind` held a value the FSM/CHECK
    /// constraints should have rejected — schema invariant violation.
    #[error("recovery invariant violated: {0}")]
    Invariant(String),
    /// Wraps a runner error so the recovery report can surface it.
    #[error("recovery runner error")]
    Runner(#[source] RunnerError),
}

impl From<tokio_rusqlite::Error> for RecoveryError {
    fn from(e: tokio_rusqlite::Error) -> Self {
        Self::Storage(e)
    }
}

/// Maps each [`WalKind`] to the [`StepBody`] that should run its steps.
/// Implementations are owned by sibling issues #57 / #58.
pub trait StepBodyRegistry: Send + Sync {
    /// Returns the body for the given kind, or `None` if no body is
    /// registered. `None` causes `Resume` and `AbortAndCompensate` to be
    /// skipped with a structured warn.
    fn body_for(&self, kind: WalKind) -> Option<Arc<dyn StepBody>>;
}

/// Empty registry — every kind returns `None`. Used as the default while
/// #57/#58 are in flight; lets terminal-finalize cases run without a body.
pub struct EmptyRegistry;

impl StepBodyRegistry for EmptyRegistry {
    fn body_for(&self, _kind: WalKind) -> Option<Arc<dyn StepBody>> {
        None
    }
}

/// Configuration for [`recover_pending`].
pub struct RecoveryConfig {
    /// Set `false` to skip recovery entirely (e.g. in tests that pre-seed
    /// fixture state and don't want it advanced).
    pub enabled: bool,
    /// Body registry. Default is [`EmptyRegistry`].
    pub bodies: Box<dyn StepBodyRegistry>,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bodies: Box::new(EmptyRegistry),
        }
    }
}

impl std::fmt::Debug for RecoveryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryConfig")
            .field("enabled", &self.enabled)
            .field("bodies", &"<dyn StepBodyRegistry>")
            .finish()
    }
}

/// Outcome of one recovery pass.
#[derive(Debug, Default)]
pub struct RecoveryReport {
    /// Ops finalized to `COMMITTED` during this pass.
    pub finalized_committed: Vec<OperationId>,
    /// Ops finalized to `REJECTED` (recovered from `ISSUED`).
    pub finalized_rejected: Vec<OperationId>,
    /// Ops finalized to `ABORTED` because a step exhausted retries.
    pub aborted: Vec<(OperationId, u32)>,
    /// Ops resumed that ran successfully and finalized to `COMMITTED`.
    pub resumed_committed: Vec<OperationId>,
    /// Ops where `Resume` / `AbortAndCompensate` was needed but no body
    /// was registered; skipped with a warn.
    pub skipped_no_body: Vec<(OperationId, RecoveryDecision)>,
    /// Ops already terminal — no action.
    pub no_op: Vec<OperationId>,
    /// Ops whose `wal_ops.kind` is not in scope for the #55 scaffold
    /// (anything other than `upsert`, `forget_record`, `expire`).
    /// Recorded with the raw kind string for observability.
    pub skipped_unhandled_kind: Vec<(OperationId, String)>,
}

/// Runs recovery on every non-terminal `wal_ops` row.
///
/// Processes ops in `issued_seq` order (oldest first). Returns the
/// [`RecoveryReport`] for the caller to log/metric on.
///
/// # Errors
/// - [`RecoveryError::Storage`] on connection failure during snapshot loads
///   or finalize transitions.
/// - [`RecoveryError::Invariant`] if the schema produced an unexpected
///   `wal_ops.state` / `kind` value.
/// - [`RecoveryError::Runner`] propagated from a body exhaustion that the
///   second-pass `decide_recovery` couldn't handle (should not happen — a
///   second pass on Exhausted always returns `AbortAndCompensate`).
pub async fn recover_pending(
    conn: &Arc<Connection>,
    config: &RecoveryConfig,
) -> Result<RecoveryReport, RecoveryError> {
    let mut report = RecoveryReport::default();

    if !config.enabled {
        return Ok(report);
    }

    let pending = list_open_ops(conn).await?;

    for (op_id, kind_str, op_state) in pending {
        let Ok(kind) = parse_kind(&kind_str) else {
            warn!(
                op_id = %op_id,
                kind = %kind_str,
                state = ?op_state,
                "WAL op kind not handled by issue #55 scaffold; skipping"
            );
            report.skipped_unhandled_kind.push((op_id, kind_str));
            continue;
        };
        let snapshot = load_snapshot(conn, &op_id, kind, op_state).await?;
        let decision = decide_recovery(&snapshot);
        apply_decision(conn, &snapshot, decision, &op_id, config, &mut report).await?;
    }

    Ok(report)
}

async fn list_open_ops(
    conn: &Arc<Connection>,
) -> Result<Vec<(OperationId, String, OpState)>, RecoveryError> {
    let rows: Vec<(String, String, String)> = conn
        .call(|c| {
            let mut stmt = c.prepare(
                "SELECT operation_id, kind, state \
                 FROM wal_ops \
                 WHERE state IN ('ISSUED', 'PREPARED') \
                 ORDER BY issued_seq ASC",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, tokio_rusqlite::Error>(rows)
        })
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for (op, kind, state) in rows {
        let op_id = OperationId::parse(op).map_err(|e| {
            RecoveryError::Invariant(format!("wal_ops.operation_id parse: {e}"))
        })?;
        // Defer kind parsing to the per-op loop so unhandled kinds can be
        // skipped with a warn instead of aborting recovery for everyone.
        let state = parse_op_state(&state)?;
        out.push((op_id, kind, state));
    }
    Ok(out)
}

fn parse_kind(s: &str) -> Result<WalKind, RecoveryError> {
    match s {
        "upsert" => Ok(WalKind::Upsert),
        "forget_record" => Ok(WalKind::ForgetRecord),
        "expire" => Ok(WalKind::Expire),
        // Other kinds (promote, forget_session, evolve, graph_*, lint_repair)
        // are out of scope for #55. Recovery skips them with an invariant
        // marker — the body for those kinds is the existing per-kind helper
        // (lint_repair) or a future issue.
        other => Err(RecoveryError::Invariant(format!(
            "wal_ops.kind {other:?} not handled by #55 scaffold"
        ))),
    }
}

fn parse_op_state(s: &str) -> Result<OpState, RecoveryError> {
    match s {
        "ISSUED" => Ok(OpState::Issued),
        "PREPARED" => Ok(OpState::Prepared),
        "COMMITTED" => Ok(OpState::Committed),
        "ABORTED" => Ok(OpState::Aborted),
        "REJECTED" => Ok(OpState::Rejected),
        other => Err(RecoveryError::Invariant(format!(
            "wal_ops.state {other:?} violates schema CHECK"
        ))),
    }
}

fn parse_step_state(s: &str) -> Result<StepState, RecoveryError> {
    match s {
        "PENDING" => Ok(StepState::Pending),
        "DONE" => Ok(StepState::Done),
        "FAILED" => Ok(StepState::Failed),
        "COMPENSATED" => Ok(StepState::Compensated),
        other => Err(RecoveryError::Invariant(format!(
            "wal_steps.state {other:?} violates schema CHECK"
        ))),
    }
}

async fn load_snapshot(
    conn: &Arc<Connection>,
    op_id: &OperationId,
    kind: WalKind,
    state: OpState,
) -> Result<OpSnapshot, RecoveryError> {
    let op = op_id.as_str().to_owned();
    let rows: Vec<(u32, String, u32, Option<String>)> = conn
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT step_ord, state, attempts, last_error \
                 FROM wal_steps \
                 WHERE operation_id = ?1 \
                 ORDER BY step_ord ASC",
            )?;
            let rows = stmt
                .query_map([op], |r| {
                    Ok((
                        r.get::<_, u32>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, u32>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, tokio_rusqlite::Error>(rows)
        })
        .await?;

    let mut steps = Vec::with_capacity(rows.len());
    for (ord, state_str, attempts, last_error) in rows {
        steps.push(StepRow {
            ord,
            state: parse_step_state(&state_str)?,
            attempts,
            last_error,
        });
    }

    Ok(OpSnapshot { kind, state, steps })
}

async fn apply_decision(
    conn: &Arc<Connection>,
    snapshot: &OpSnapshot,
    decision: RecoveryDecision,
    op_id: &OperationId,
    config: &RecoveryConfig,
    report: &mut RecoveryReport,
) -> Result<(), RecoveryError> {
    match decision {
        RecoveryDecision::NoOp => {
            report.no_op.push(op_id.clone());
            Ok(())
        }
        RecoveryDecision::FinalizeRejected => {
            finalize(conn, op_id, OpState::Rejected, "recovered_from_issued").await?;
            report.finalized_rejected.push(op_id.clone());
            Ok(())
        }
        RecoveryDecision::FinalizeCommitted => {
            finalize(conn, op_id, OpState::Committed, "recovered").await?;
            report.finalized_committed.push(op_id.clone());
            Ok(())
        }
        RecoveryDecision::Resume { next_ord } => {
            handle_resume(conn, snapshot, next_ord, op_id, config, report).await
        }
        RecoveryDecision::AbortAndCompensate { failed_ord } => {
            // Compensation invocation is deferred to #57/#58. Mark op
            // ABORTED and warn so the gap is observable.
            let reason = format!("recovered: step {failed_ord} exhausted");
            finalize(conn, op_id, OpState::Aborted, &reason).await?;
            warn!(
                op_id = %op_id,
                kind = ?snapshot.kind,
                failed_ord,
                "WAL op aborted by recovery; compensations not run (deferred to #57/#58)"
            );
            report.aborted.push((op_id.clone(), failed_ord));
            Ok(())
        }
        // `RecoveryDecision` is `#[non_exhaustive]`; surface unknown
        // variants as an invariant so a future addition can't silently
        // bypass recovery.
        other => Err(RecoveryError::Invariant(format!(
            "unknown RecoveryDecision variant: {other:?}"
        ))),
    }
}

async fn handle_resume(
    conn: &Arc<Connection>,
    snapshot: &OpSnapshot,
    next_ord: u32,
    op_id: &OperationId,
    config: &RecoveryConfig,
    report: &mut RecoveryReport,
) -> Result<(), RecoveryError> {
    let Some(body) = config.bodies.body_for(snapshot.kind) else {
        warn!(
            op_id = %op_id,
            kind = ?snapshot.kind,
            next_ord,
            "WAL op resume skipped — no StepBody registered (decision-only mode)"
        );
        report
            .skipped_no_body
            .push((op_id.clone(), RecoveryDecision::Resume { next_ord }));
        return Ok(());
    };

    let graph = graph_for(snapshot.kind);
    match runner::run_from(conn, graph, op_id, next_ord, body).await {
        Ok(()) => {
            // Reload snapshot and re-decide. The follow-up decision must be
            // FinalizeCommitted or AbortAndCompensate (terminal-bound).
            let reloaded = load_snapshot(conn, op_id, snapshot.kind, OpState::Prepared).await?;
            match decide_recovery(&reloaded) {
                RecoveryDecision::FinalizeCommitted => {
                    finalize(conn, op_id, OpState::Committed, "recovered").await?;
                    report.resumed_committed.push(op_id.clone());
                }
                RecoveryDecision::AbortAndCompensate { failed_ord } => {
                    let reason = format!("recovered: step {failed_ord} exhausted");
                    finalize(conn, op_id, OpState::Aborted, &reason).await?;
                    report.aborted.push((op_id.clone(), failed_ord));
                }
                other => {
                    return Err(RecoveryError::Invariant(format!(
                        "post-resume decision unexpected: {other:?}"
                    )));
                }
            }
            Ok(())
        }
        Err(RunnerError::Exhausted { op_id: e_op, step_ord }) => {
            let reason = format!("recovered: step {step_ord} exhausted");
            finalize(conn, &e_op, OpState::Aborted, &reason).await?;
            report.aborted.push((e_op, step_ord));
            Ok(())
        }
        Err(e) => Err(RecoveryError::Runner(e)),
    }
}

async fn finalize(
    conn: &Arc<Connection>,
    op_id: &OperationId,
    new_state: OpState,
    reason: &str,
) -> Result<(), RecoveryError> {
    let op = op_id.as_str().to_owned();
    let new_state_str = new_state.as_str();
    let reason_owned = reason.to_owned();
    let now = now_ms();

    conn.call(move |c| {
        c.execute(
            "UPDATE wal_ops \
               SET state = ?1, reason = COALESCE(reason, ?2), updated_at = ?3 \
             WHERE operation_id = ?4",
            rusqlite::params![new_state_str, reason_owned, now, op],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;
    Ok(())
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}
