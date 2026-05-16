//! Issue #92 — workflow health lint check (spec §4.10).
//!
//! Reads [`crate::contract::workflow_jobs::WorkflowJobsReader`] and emits
//! one of four findings:
//!
//! * [`Kind::WorkflowDeadLetter`] (Error) — one per dead-lettered row,
//!   plus one aggregate Warning when the total exceeds
//!   `max_dead_letter_listed` (issue #92 round-6 finding 6.3).
//! * [`Kind::WorkflowStuck`] (Warning) — oldest queued row past `stuck_queue_threshold_ms`.
//! * [`Kind::WorkflowStaleSummary`] (Warning) — `dream.light` last success too old.
//! * [`Kind::WorkflowOverdue`] (Warning) — `expire.tier` / `evaluate.sweep` last success too old.
//!
//! When `LintInputs::workflow_jobs` is `None` the check is a no-op: fixture-only
//! callers and pre-#92 tests stay green.

use crate::contract::job_store::JobKind;
use crate::contract::workflow_jobs::DeadLetterRow;
use crate::generated::verbs::lint::{Finding, Kind, Severity, Target};
use crate::verbs::lint::{LintInputs, finding};

/// Run the workflow-health checks. No-op when the reader is unwired.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let Some(jobs) = inputs.workflow_jobs else {
        return Vec::new();
    };
    let cfg = &inputs.config.workflows.lint;
    let now = inputs.now_ms;
    let mut out: Vec<Finding> = Vec::new();

    // 1. Dead-letter rows -> Error per row, plus one aggregate Warning
    //    when the total exceeds the listing cap. Without the overflow
    //    finding a vault with 10 dead-lettered jobs and one with
    //    100,000 produce the same number of findings — operators get
    //    no signal that the queue is degenerating. The total count
    //    query is index-backed via `workflow_jobs_dead_letter_idx`
    //    (migration 0062), so the extra round-trip is cheap.
    let listing_cap = cfg.max_dead_letter_listed as usize;
    let total = jobs.dead_letter_count(None);
    for row in jobs.dead_letter_rows(listing_cap) {
        out.push(dead_letter_finding(&row));
    }
    if total > listing_cap {
        out.push(dead_letter_overflow_finding(listing_cap, total));
    }

    // 2. Stuck queue -> Warning if oldest Queued > threshold.
    if let Some(age) = jobs.oldest_queued_age_ms(None, now)
        && age > cfg.stuck_queue_threshold_ms
    {
        out.push(stuck_finding(age));
    }

    // 3. Stale dream summary.
    if let Some(t) = jobs.last_success_ms(&JobKind::new("dream.light")) {
        let age = now - t;
        if age > cfg.stale_dream_threshold_ms {
            out.push(stale_summary_finding(age));
        }
    }

    // 4. Overdue expire / eval.
    for kind in ["expire.tier", "evaluate.sweep"] {
        if let Some(t) = jobs.last_success_ms(&JobKind::new(kind)) {
            let age = now - t;
            if age > cfg.overdue_threshold_ms {
                out.push(overdue_finding(kind, age));
            }
        }
    }

    // Issue #92 round-7 finding 7.2: drain the reader's last_error
    // slot AFTER every query has run. If any of the methods above
    // swallowed a backend failure (locked DB, poisoned mutex,
    // post-construction schema drift), surface it as a `DeferredCheck`
    // Info finding so an operator does not mistake "queries failed"
    // for "no workflow issues". One finding per run, regardless of
    // how many methods failed — the first failure usually identifies
    // the root cause and noisy duplicates hurt more than they help.
    if let Some(reason) = jobs.take_last_error() {
        out.push(reader_degraded_finding(&reason));
    }

    out
}

fn dead_letter_finding(row: &DeadLetterRow) -> Finding {
    let mut f = finding(
        Kind::WorkflowDeadLetter,
        Severity::Error,
        format!(
            "workflow {kind} job {job_id} dead-lettered after {attempts} attempts ({class}): {err}",
            kind = row.kind,
            job_id = row.job_id,
            attempts = row.attempts,
            class = row.failure_class.as_str(),
            err = row.last_error,
        ),
    );
    f.target = Some(Target {
        record_id: None,
        operation_id: Some(crate::generated::common::Ulid(row.job_id.to_string())),
        path: None,
    });
    f
}

/// Issue #92 round-6 finding 6.3: aggregate Warning surfaced when the
/// dead-letter total exceeds the listing cap. Reuses `WorkflowDeadLetter`
/// so the downstream pipeline (CLI summary, MCP tool surface) does not
/// need a new finding kind for what is logically the same family of
/// issues — only the severity differs from per-row Errors.
fn dead_letter_overflow_finding(listed: usize, total: usize) -> Finding {
    finding(
        Kind::WorkflowDeadLetter,
        Severity::Warning,
        format!(
            "showing {listed} of {total} dead-lettered workflow jobs — \
             increase workflows.lint.max_dead_letter_listed to see all"
        ),
    )
}

fn stuck_finding(age_ms: i64) -> Finding {
    finding(
        Kind::WorkflowStuck,
        Severity::Warning,
        format!("oldest queued workflow job has waited {age_ms}ms — workers idle?"),
    )
}

fn stale_summary_finding(age_ms: i64) -> Finding {
    finding(
        Kind::WorkflowStaleSummary,
        Severity::Warning,
        format!("no dream.light success in {age_ms}ms — rolling summary may be stale"),
    )
}

fn overdue_finding(kind: &str, age_ms: i64) -> Finding {
    finding(
        Kind::WorkflowOverdue,
        Severity::Warning,
        format!("no {kind} success in {age_ms}ms — schedule may be stalled"),
    )
}

/// Issue #92 round-7 finding 7.2: build the `DeferredCheck` finding
/// surfaced when the reader reports that one of its queries failed
/// at runtime. Severity is `Info` (matching the consent-journal /
/// index-stats deferred-check paths) — the check ran but its data
/// is unreliable, not a hard error, and the operator's fix lives at
/// the DB layer rather than in the records being linted.
fn reader_degraded_finding(reason: &str) -> Finding {
    let mut f = finding(
        Kind::DeferredCheck,
        Severity::Info,
        format!(
            "workflow_health: reader degraded ({reason}); \
             findings for this run may underreport dead-letter / stuck / overdue jobs"
        ),
    );
    f.suggested_fix = Some(
        "inspect .cairn/cairn.db for lock contention, schema drift, or corruption; \
         re-run migrations and retry lint"
            .to_owned(),
    );
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::job_store::{FailureClass, JobId};
    use crate::contract::workflow_jobs::DeadLetterRow;
    use crate::verbs::lint::{MockWorkflowJobsReader, empty_lint_inputs_with_reader};

    #[test]
    fn dead_letter_row_emits_error_finding() {
        let row = DeadLetterRow {
            job_id: JobId::new("j-dead"),
            kind: JobKind::new("dream.light"),
            attempts: 3,
            failure_class: FailureClass::Validation,
            last_error: "bad payload".into(),
            dead_letter_at_ms: 500,
        };
        let reader = MockWorkflowJobsReader::default().with_dead_letter(row);
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000);
        let findings = super::run(&inputs);
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::Error));
        assert!(matches!(findings[0].kind, Kind::WorkflowDeadLetter));
        // Acceptance: target.operation_id carries the job_id for actionability.
        let target = findings[0].target.as_ref().expect("dead-letter target set");
        assert_eq!(
            target.operation_id.as_ref().map(|u| u.0.as_str()),
            Some("j-dead")
        );
        // Acceptance: message includes the job_id and failure_class string.
        assert!(findings[0].message.contains("j-dead"));
        assert!(findings[0].message.contains("validation"));
    }

    #[test]
    fn stuck_queue_emits_warning_when_above_threshold() {
        let reader = MockWorkflowJobsReader::default().with_oldest_queued_age(11 * 60_000);
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000_000);
        let findings = super::run(&inputs);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::WorkflowStuck))
        );
    }

    #[test]
    fn stuck_queue_below_threshold_emits_nothing() {
        let reader = MockWorkflowJobsReader::default().with_oldest_queued_age(9 * 60_000);
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000_000);
        let findings = super::run(&inputs);
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f.kind, Kind::WorkflowStuck))
        );
    }

    #[test]
    fn stale_dream_emits_warning() {
        let reader = MockWorkflowJobsReader::default().with_last_success("dream.light", 0);
        let inputs = empty_lint_inputs_with_reader(&reader, 25 * 3_600_000); // 25 h > 24 h
        let findings = super::run(&inputs);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::WorkflowStaleSummary))
        );
    }

    #[test]
    fn fresh_dream_emits_nothing() {
        let reader = MockWorkflowJobsReader::default().with_last_success("dream.light", 0);
        let inputs = empty_lint_inputs_with_reader(&reader, 23 * 3_600_000); // 23 h < 24 h
        let findings = super::run(&inputs);
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f.kind, Kind::WorkflowStaleSummary))
        );
    }

    #[test]
    fn overdue_expire_emits_warning() {
        let reader = MockWorkflowJobsReader::default().with_last_success("expire.tier", 0);
        let inputs = empty_lint_inputs_with_reader(&reader, 49 * 3_600_000); // 49 h > 48 h
        let findings = super::run(&inputs);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::WorkflowOverdue))
        );
    }

    #[test]
    fn overdue_evaluate_emits_warning() {
        let reader = MockWorkflowJobsReader::default().with_last_success("evaluate.sweep", 0);
        let inputs = empty_lint_inputs_with_reader(&reader, 49 * 3_600_000);
        let findings = super::run(&inputs);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::WorkflowOverdue))
        );
    }

    #[test]
    fn snapshot_dead_letter_finding() {
        let row = DeadLetterRow {
            job_id: JobId::new("01JTESTJOBDEADLETTER0001"),
            kind: JobKind::new("dream.light"),
            attempts: 3,
            failure_class: FailureClass::Poison,
            last_error: "panic in step 2".into(),
            dead_letter_at_ms: 1_700_000_000_000,
        };
        let reader = MockWorkflowJobsReader::default().with_dead_letter(row);
        let inputs = empty_lint_inputs_with_reader(&reader, 1_700_000_001_000);
        let findings = super::run(&inputs);
        insta::assert_json_snapshot!(findings);
    }

    #[test]
    fn snapshot_stuck_finding() {
        let reader = MockWorkflowJobsReader::default().with_oldest_queued_age(900_000);
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000_000);
        let findings = super::run(&inputs);
        insta::assert_json_snapshot!(findings);
    }

    #[test]
    fn snapshot_stale_summary_finding() {
        let reader = MockWorkflowJobsReader::default().with_last_success("dream.light", 0);
        let inputs = empty_lint_inputs_with_reader(&reader, 25 * 3_600_000);
        let findings = super::run(&inputs);
        insta::assert_json_snapshot!(findings);
    }

    #[test]
    fn snapshot_overdue_finding() {
        let reader = MockWorkflowJobsReader::default().with_last_success("expire.tier", 0);
        let inputs = empty_lint_inputs_with_reader(&reader, 49 * 3_600_000);
        let findings = super::run(&inputs);
        insta::assert_json_snapshot!(findings);
    }

    #[test]
    fn dead_letter_overflow_emits_aggregate_warning() {
        // Issue #92 round-6 finding 6.3: with 25 dead-lettered rows and
        // the default cap of 10, the check must emit 10 per-row Errors
        // AND one aggregate Warning whose message names both counts.
        // Without the overflow Warning operators have no signal that
        // their dead-letter queue is degenerating beyond what they're
        // told to look at.
        let mut reader = MockWorkflowJobsReader::default();
        for i in 0..25 {
            reader = reader.with_dead_letter(DeadLetterRow {
                job_id: JobId::new(format!("j-{i:02}")),
                kind: JobKind::new("dream.light"),
                attempts: 3,
                failure_class: FailureClass::Transient,
                last_error: "boom".into(),
                dead_letter_at_ms: 1_000 + i64::from(i),
            });
        }
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000_000);
        let findings = super::run(&inputs);

        let errors: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                matches!(f.kind, Kind::WorkflowDeadLetter) && matches!(f.severity, Severity::Error)
            })
            .collect();
        assert_eq!(
            errors.len(),
            10,
            "10 per-row Errors at default cap, got {}: {findings:?}",
            errors.len()
        );

        let warnings: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                matches!(f.kind, Kind::WorkflowDeadLetter)
                    && matches!(f.severity, Severity::Warning)
            })
            .collect();
        assert_eq!(
            warnings.len(),
            1,
            "exactly one aggregate Warning: {findings:?}"
        );
        let msg = &warnings[0].message;
        assert!(msg.contains("10 of 25"), "message must name counts: {msg}");
        assert!(
            msg.contains("max_dead_letter_listed"),
            "message must name the config knob: {msg}"
        );
    }

    #[test]
    fn dead_letter_exactly_at_cap_emits_no_overflow_warning() {
        // Boundary check: total == listing_cap must NOT emit the
        // aggregate Warning (no overflow, all rows are visible).
        let mut reader = MockWorkflowJobsReader::default();
        for i in 0..10 {
            reader = reader.with_dead_letter(DeadLetterRow {
                job_id: JobId::new(format!("j-{i:02}")),
                kind: JobKind::new("dream.light"),
                attempts: 3,
                failure_class: FailureClass::Transient,
                last_error: "boom".into(),
                dead_letter_at_ms: 1_000 + i64::from(i),
            });
        }
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000_000);
        let findings = super::run(&inputs);
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f.severity, Severity::Warning)
                    && matches!(f.kind, Kind::WorkflowDeadLetter)),
            "no overflow Warning when total <= cap: {findings:?}"
        );
    }

    #[test]
    fn reader_last_error_emits_deferred_check_info() {
        // Issue #92 round-7 finding 7.2: when the reader records a
        // runtime failure (e.g. locked DB, schema drift detected
        // post-construction), `workflow_health::run` must surface a
        // `DeferredCheck` Info finding instead of leaving the operator
        // with a clean output that hides the failure.
        let reader = MockWorkflowJobsReader::default()
            .with_last_error("dead_letter_count: database is locked");
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000_000);
        let findings = super::run(&inputs);
        let degraded: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                matches!(f.kind, Kind::DeferredCheck) && matches!(f.severity, Severity::Info)
            })
            .collect();
        assert_eq!(
            degraded.len(),
            1,
            "exactly one reader-degraded DeferredCheck Info finding: {findings:?}"
        );
        assert!(
            degraded[0].message.contains("workflow_health"),
            "message must scope the deferral to workflow_health: {}",
            degraded[0].message
        );
        assert!(
            degraded[0].message.contains("database is locked"),
            "message must surface the underlying reason: {}",
            degraded[0].message
        );
        assert!(
            degraded[0].suggested_fix.is_some(),
            "deferred check must carry a remediation hint"
        );
    }

    #[test]
    fn reader_last_error_drains_after_one_finding() {
        // Drain semantics: a second `run` against a reader whose
        // `take_last_error` has already returned `Some` (because the
        // previous run drained it) must NOT emit a second deferred
        // check. Without this, repeated lint invocations against a
        // briefly-degraded reader would emit findings forever.
        let reader = MockWorkflowJobsReader::default().with_last_error("transient lock");
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000_000);
        let first = super::run(&inputs);
        let second = super::run(&inputs);
        assert_eq!(
            first
                .iter()
                .filter(|f| matches!(f.kind, Kind::DeferredCheck))
                .count(),
            1,
            "first run drains the error"
        );
        assert_eq!(
            second
                .iter()
                .filter(|f| matches!(f.kind, Kind::DeferredCheck))
                .count(),
            0,
            "second run sees no error (slot was drained)"
        );
    }

    #[test]
    fn missing_reader_emits_nothing() {
        let cfg = crate::config::CairnConfig::default();
        let inputs = crate::verbs::lint::LintInputs {
            records: &[],
            config: &cfg,
            index_stats: crate::contract::memory_store::IndexStats::new(0, 0),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            now_ms: 0,
        };
        assert!(super::run(&inputs).is_empty());
    }
}
