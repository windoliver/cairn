//! Issue #92 — workflow health lint check (spec §4.10).
//!
//! Reads [`crate::contract::workflow_jobs::WorkflowJobsReader`] and emits
//! one of four findings:
//!
//! * [`Kind::WorkflowDeadLetter`] (Error) — one per dead-lettered row.
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

    // 1. Dead-letter rows -> Error per row.
    for row in jobs.dead_letter_rows(cfg.max_dead_letter_listed as usize) {
        out.push(dead_letter_finding(&row));
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
