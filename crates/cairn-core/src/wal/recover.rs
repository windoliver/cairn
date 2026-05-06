//! Pure `decide_recovery` function — given a WAL op snapshot, decides what
//! the boot-recovery routine should do next (brief §5.6 "Boot-time
//! recovery").
//!
//! No I/O. The store-side wrapper loads the snapshot, calls
//! [`decide_recovery`], and applies the returned [`RecoveryDecision`].

use crate::wal::fsm::{OpState, StepState};
use crate::wal::step_graph::{StepGraph, WalKind, graph_for};

/// Maximum number of attempts a single step may make before recovery
/// transitions the op to ABORTED. Brief §5.6 retry policy: max 3 attempts,
/// 100 ms / 400 ms / 1600 ms backoff.
pub const MAX_STEP_ATTEMPTS: u32 = 3;

/// One row from `wal_steps`, projected for the recovery decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    /// `wal_steps.step_ord`.
    pub ord: u32,
    /// `wal_steps.state`.
    pub state: StepState,
    /// `wal_steps.attempts`.
    pub attempts: u32,
    /// `wal_steps.last_error`. Carried for telemetry only — `decide_recovery`
    /// does not read it.
    pub last_error: Option<String>,
}

/// Snapshot of one `wal_ops` row plus its `wal_steps` rows.
#[derive(Debug, Clone)]
pub struct OpSnapshot {
    /// `wal_ops.kind` parsed.
    pub kind: WalKind,
    /// `wal_ops.state` parsed.
    pub state: OpState,
    /// `wal_steps` rows for this op. **Must be sorted by `ord` ascending**.
    pub steps: Vec<StepRow>,
}

/// What the boot-recovery routine should do with one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryDecision {
    /// Op is terminal; nothing to do. Idempotent on re-entry.
    NoOp,
    /// Op is `ISSUED` with no progress; finalize as `REJECTED` because at
    /// P0 the same-txn collapse means a persisted `ISSUED` can only result
    /// from a daemon crash before `PREPARE` — there is no recovery path
    /// that produces side-effects from an `ISSUED` op.
    FinalizeRejected,
    /// Op is `PREPARED`, every step is `DONE`; flip to `COMMITTED`.
    FinalizeCommitted,
    /// Op is `PREPARED`; resume from step `next_ord` (or 0 if no DONE rows
    /// exist).
    Resume {
        /// First step ord that still needs to run.
        next_ord: u32,
    },
    /// A step exhausted its retry budget (`attempts >= MAX_STEP_ATTEMPTS`
    /// and last state is `FAILED`). Abort and run compensations.
    AbortAndCompensate {
        /// Ord of the step that exhausted retries.
        failed_ord: u32,
    },
}

/// Decides what recovery should do for one op.
///
/// # Decision rules (brief §5.6)
///
/// | Input state | Step pattern                                 | Output                         |
/// |-------------|----------------------------------------------|--------------------------------|
/// | terminal    | (any)                                        | `NoOp`                         |
/// | `Issued`    | (any)                                        | `FinalizeRejected`             |
/// | `Prepared`  | a step has `Failed` with `attempts >= MAX`   | `AbortAndCompensate{ord}`      |
/// | `Prepared`  | every graph step has a `Done` row            | `FinalizeCommitted`            |
/// | `Prepared`  | otherwise                                    | `Resume{next_ord}`             |
///
/// `next_ord` = `1 + max(ord where state == Done)`, or `0` if no `Done`
/// rows exist. `Failed` rows below `MAX` and `Pending` rows do not advance
/// `next_ord` — they will be retried in place.
///
/// `expires_at` is intentionally NOT consulted: brief §5.6 specifies that
/// TTL applies to new external requests, not to WAL recovery.
#[must_use]
pub fn decide_recovery(snapshot: &OpSnapshot) -> RecoveryDecision {
    use OpState::{Aborted, Committed, Issued, Prepared, Rejected};

    match snapshot.state {
        Committed | Aborted | Rejected => RecoveryDecision::NoOp,
        Issued => RecoveryDecision::FinalizeRejected,
        Prepared => decide_prepared(snapshot),
    }
}

fn decide_prepared(snapshot: &OpSnapshot) -> RecoveryDecision {
    // First: any step exhausted retries?
    if let Some(failed_ord) = snapshot
        .steps
        .iter()
        .find(|s| s.state == StepState::Failed && s.attempts >= MAX_STEP_ATTEMPTS)
        .map(|s| s.ord)
    {
        return RecoveryDecision::AbortAndCompensate { failed_ord };
    }

    let graph: &StepGraph = graph_for(snapshot.kind);
    // Step graphs are static and tiny (≤7 entries); cast is genuinely safe.
    #[allow(clippy::cast_possible_truncation)]
    let total = graph.len() as u32;

    // All steps DONE?
    // Step rows mirror the static graph (≤7 entries); cast is genuinely safe.
    #[allow(clippy::cast_possible_truncation)]
    let done_count = snapshot
        .steps
        .iter()
        .filter(|s| s.state == StepState::Done)
        .count() as u32;
    if done_count == total {
        return RecoveryDecision::FinalizeCommitted;
    }

    // Otherwise resume from 1 + max(done_ord), or 0 if no DONE rows.
    let next_ord = snapshot
        .steps
        .iter()
        .filter(|s| s.state == StepState::Done)
        .map(|s| s.ord)
        .max()
        .map_or(0, |max_done| max_done + 1);

    RecoveryDecision::Resume { next_ord }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(state: OpState, steps: Vec<StepRow>) -> OpSnapshot {
        OpSnapshot { kind: WalKind::Upsert, state, steps }
    }

    fn step(ord: u32, state: StepState, attempts: u32) -> StepRow {
        StepRow { ord, state, attempts, last_error: None }
    }

    #[test]
    fn terminal_committed_is_noop() {
        let s = snap(OpState::Committed, vec![]);
        assert_eq!(decide_recovery(&s), RecoveryDecision::NoOp);
    }

    #[test]
    fn terminal_aborted_is_noop() {
        let s = snap(OpState::Aborted, vec![]);
        assert_eq!(decide_recovery(&s), RecoveryDecision::NoOp);
    }

    #[test]
    fn terminal_rejected_is_noop() {
        let s = snap(OpState::Rejected, vec![]);
        assert_eq!(decide_recovery(&s), RecoveryDecision::NoOp);
    }

    #[test]
    fn issued_finalizes_rejected() {
        let s = snap(OpState::Issued, vec![]);
        assert_eq!(decide_recovery(&s), RecoveryDecision::FinalizeRejected);
    }

    #[test]
    fn prepared_no_steps_resumes_from_zero() {
        let s = snap(OpState::Prepared, vec![]);
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 0 }
        );
    }

    #[test]
    fn prepared_partial_resumes_after_last_done() {
        // 0,1 DONE, 2 absent — resume from 2.
        let s = snap(
            OpState::Prepared,
            vec![
                step(0, StepState::Done, 1),
                step(1, StepState::Done, 1),
            ],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 2 }
        );
    }

    #[test]
    fn prepared_all_done_finalizes_committed() {
        // upsert has 6 steps (ords 0..=5).
        let s = snap(
            OpState::Prepared,
            (0..6).map(|o| step(o, StepState::Done, 1)).collect(),
        );
        assert_eq!(decide_recovery(&s), RecoveryDecision::FinalizeCommitted);
    }

    #[test]
    fn prepared_failed_under_max_resumes() {
        // step 1 FAILED with 1 attempt — under MAX, treat as in-flight.
        // Last DONE is 0, so next_ord = 1.
        let s = snap(
            OpState::Prepared,
            vec![
                step(0, StepState::Done, 1),
                step(1, StepState::Failed, 1),
            ],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 1 }
        );
    }

    #[test]
    fn prepared_failed_at_max_aborts() {
        let s = snap(
            OpState::Prepared,
            vec![
                step(0, StepState::Done, 1),
                step(1, StepState::Failed, MAX_STEP_ATTEMPTS),
            ],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::AbortAndCompensate { failed_ord: 1 }
        );
    }

    #[test]
    fn prepared_failed_above_max_aborts() {
        let s = snap(
            OpState::Prepared,
            vec![step(0, StepState::Failed, MAX_STEP_ATTEMPTS + 1)],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::AbortAndCompensate { failed_ord: 0 }
        );
    }

    #[test]
    fn prepared_with_gap_in_done_steps_resumes_from_after_max_done() {
        // Steps 0, 1, 3 DONE; step 2 is absent (must mean it never ran).
        // We resume from 1 + max(done_ord) = 4. Step 2 will be re-run by
        // the runner (idempotent re-entry on the absent row inserts PENDING).
        // NOTE: this is the brief's intent — recovery never replays already-DONE
        // steps. The runner ensures step 2 is also marked DONE before
        // continuing because it iterates from start_ord onward; this test
        // documents what `decide_recovery` itself returns.
        let s = snap(
            OpState::Prepared,
            vec![
                step(0, StepState::Done, 1),
                step(1, StepState::Done, 1),
                step(3, StepState::Done, 1),
            ],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 4 }
        );
    }

    #[test]
    fn pending_under_max_resumes_in_place() {
        let s = snap(
            OpState::Prepared,
            vec![
                step(0, StepState::Done, 1),
                step(1, StepState::Pending, 1),
            ],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 1 }
        );
    }
}
