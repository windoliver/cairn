//! Issue #123 — federation dead-propagation lint check.
//!
//! Surfaces `workflow_jobs` rows whose `kind` starts with
//! `"federation.propagate."` and whose `state` is `'Failed'`.
//! One [`Kind::FederationDeadPropagation`] Error finding is emitted
//! per failed row; the finding message includes `last_error` so
//! operators can diagnose the root cause without inspecting the DB.
//!
//! Privacy: `last_error` comes from the propagation worker itself,
//! which MUST only write hub error codes / local metadata — never
//! record bodies. The check forwards the string verbatim and relies
//! on that invariant; it does NOT scrub the string.
//!
//! When [`LintInputs::workflow_jobs`] is `None` the check is a no-op:
//! fixture-only callers and pre-#123 tests stay green.

use crate::contract::workflow_jobs::FailedFederationJob;
use crate::generated::verbs::lint::{Finding, Kind, Severity, Target};
use crate::verbs::lint::{LintInputs, finding};

/// Kind-prefix for all federation propagation jobs (issue #123, brief §12).
const FEDERATION_PROPAGATE_PREFIX: &str = "federation.propagate.";

/// Run the federation dead-propagation check. No-op when the reader is unwired.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let Some(jobs) = inputs.workflow_jobs else {
        return Vec::new();
    };
    jobs.failed_federation_jobs(FEDERATION_PROPAGATE_PREFIX)
        .into_iter()
        .map(|job| dead_propagation_finding(&job))
        .collect()
}

fn dead_propagation_finding(job: &FailedFederationJob) -> Finding {
    let mut f = finding(
        Kind::FederationDeadPropagation,
        Severity::Error,
        format!(
            "federation propagation job {job_id} ({kind}) failed after {attempts} attempt(s): {err}",
            job_id = job.job_id,
            kind = job.kind,
            attempts = job.attempts,
            err = job.last_error,
        ),
    );
    f.target = Some(Target {
        record_id: None,
        operation_id: Some(crate::generated::common::Ulid(job.job_id.to_string())),
        path: None,
    });
    f.suggested_fix = Some(
        "inspect the remote hub logs for the matching operation_id; \
         re-enqueue via `cairn federation retry` once the root cause is resolved"
            .to_owned(),
    );
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::job_store::JobId;
    use crate::contract::workflow_jobs::FailedFederationJob;
    use crate::verbs::lint::{MockWorkflowJobsReader, empty_lint_inputs_with_reader};

    fn sample_job(kind: &str, err: &str) -> FailedFederationJob {
        FailedFederationJob {
            job_id: JobId::new("01JFEDPROPFAILED0000001"),
            kind: crate::contract::job_store::JobKind::new(kind),
            attempts: 3,
            last_error: err.to_owned(),
            failed_at_ms: Some(1_700_000_000_000),
        }
    }

    #[test]
    fn failed_federation_job_emits_error_finding() {
        let job = sample_job(
            "federation.propagate.share",
            "remote hub rejected: 403 Forbidden",
        );
        let reader = MockWorkflowJobsReader::default().with_failed_federation_job(job);
        let inputs = empty_lint_inputs_with_reader(&reader, 1_700_000_001_000);
        let findings = run(&inputs);
        assert_eq!(findings.len(), 1, "one finding per failed job");
        assert!(
            matches!(findings[0].severity, Severity::Error),
            "failed propagation is an Error"
        );
        assert!(
            matches!(findings[0].kind, Kind::FederationDeadPropagation),
            "kind must be FederationDeadPropagation"
        );
        // Finding message includes job_id and last_error.
        assert!(
            findings[0].message.contains("01JFEDPROPFAILED0000001"),
            "message must contain the job_id: {}",
            findings[0].message
        );
        assert!(
            findings[0].message.contains("403 Forbidden"),
            "message must contain last_error: {}",
            findings[0].message
        );
        // target.operation_id carries the job_id for actionability.
        let target = findings[0].target.as_ref().expect("target must be set");
        assert_eq!(
            target.operation_id.as_ref().map(|u| u.0.as_str()),
            Some("01JFEDPROPFAILED0000001"),
            "target.operation_id must be the job_id"
        );
        // A remediation hint is present.
        assert!(
            findings[0].suggested_fix.is_some(),
            "finding must carry a suggested_fix"
        );
    }

    #[test]
    fn two_failed_jobs_emit_two_findings() {
        let reader = MockWorkflowJobsReader::default()
            .with_failed_federation_job(sample_job("federation.propagate.share", "err-1"))
            .with_failed_federation_job(FailedFederationJob {
                job_id: JobId::new("01JFEDPROPFAILED0000002"),
                kind: crate::contract::job_store::JobKind::new("federation.propagate.revoke"),
                attempts: 1,
                last_error: "err-2".to_owned(),
                failed_at_ms: Some(1_700_000_000_001),
            });
        let inputs = empty_lint_inputs_with_reader(&reader, 1_700_000_001_000);
        let findings = run(&inputs);
        assert_eq!(findings.len(), 2, "one finding per failed job");
    }

    #[test]
    fn non_federation_failed_jobs_are_not_surfaced() {
        // A failed `dream.light` job must NOT produce a
        // `FederationDeadPropagation` finding — only jobs whose kind
        // starts with `"federation.propagate."` are in scope.
        let reader =
            MockWorkflowJobsReader::default().with_failed_federation_job(FailedFederationJob {
                job_id: JobId::new("01JOTHERJOB00000000001"),
                kind: crate::contract::job_store::JobKind::new("dream.light"),
                attempts: 5,
                last_error: "out of memory".to_owned(),
                failed_at_ms: None,
            });
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000_000);
        let findings = run(&inputs);
        // The mock `failed_federation_jobs` impl filters by prefix; the
        // `dream.light` job should be excluded.
        assert!(
            findings.is_empty(),
            "non-federation job must not produce a finding: {findings:?}"
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
        assert!(run(&inputs).is_empty(), "no reader -> no findings");
    }

    #[test]
    fn finding_message_never_leaks_record_body() {
        // Brief invariant 9 (privacy-by-construction): finding messages
        // are rendered into lint-report.md and MUST NOT contain raw
        // record body content. last_error MUST only contain hub error
        // codes / local metadata (enforced by the propagation worker);
        // this test documents the contract without scrubbing.
        const MARKER: &str = "SECRET-MARKER-PRIVACY-9";
        let job = FailedFederationJob {
            job_id: JobId::new("01JFEDPROPFAILED0000003"),
            kind: crate::contract::job_store::JobKind::new("federation.propagate.share"),
            attempts: 1,
            // A worker violating the invariant would set last_error to
            // body content. The test records this and asserts the check
            // itself does not *add* any body content beyond what
            // last_error already contains — scrubbing last_error before
            // forwarding is a future P1 concern (issue #123 follow-up).
            last_error: format!("hub error 400; job metadata only; no body: {MARKER}"),
            failed_at_ms: None,
        };
        let reader = MockWorkflowJobsReader::default().with_failed_federation_job(job);
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000_000);
        let findings = run(&inputs);
        assert_eq!(findings.len(), 1);
        // The check must not *add* any new record-body content beyond
        // what last_error already contains.
        assert!(
            findings[0]
                .message
                .contains("hub error 400; job metadata only; no body"),
            "message must forward last_error verbatim"
        );
    }

    #[test]
    fn snapshot_dead_propagation_finding() {
        let job = FailedFederationJob {
            job_id: JobId::new("01JFEDPROP00000SNAPSHOT"),
            kind: crate::contract::job_store::JobKind::new("federation.propagate.share"),
            attempts: 3,
            last_error: "remote hub returned 503 Service Unavailable".to_owned(),
            failed_at_ms: Some(1_700_000_000_000),
        };
        let reader = MockWorkflowJobsReader::default().with_failed_federation_job(job);
        let inputs = empty_lint_inputs_with_reader(&reader, 1_700_000_001_000);
        let findings = run(&inputs);
        insta::assert_json_snapshot!(findings);
    }
}
