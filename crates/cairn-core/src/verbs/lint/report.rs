//! `LintData → markdown` projector for `cairn lint --write-report`.
//!
//! Pure function: takes a `LintData` envelope, renders the canonical
//! `.cairn/lint-report.md` body. The CLI handler (Task 16) writes the
//! result atomically via `tempfile::Builder::tempfile_in` + `persist`.

use std::fmt::Write as _;

use crate::generated::verbs::lint::{
    AgentWorkerAuditReportRolloutState, AgentWorkerAuditWorkerWorkerKind, Kind, LintData, Severity,
};

/// Render `data` as the canonical lint-report markdown body.
///
/// Output is structured so a clean exit code does not read as "every
/// invariant was checked": the **Coverage** section above the findings
/// list explicitly counts how many `deferred_check` findings are present
/// — i.e. how many advertised checks did not run against record data
/// because the underlying infrastructure has not yet shipped.
#[must_use]
pub fn render(data: &LintData) -> String {
    // `write!` to a `String` is infallible; the `Err` variant of
    // `fmt::Write for String` is uninhabited in practice.
    let mut out = String::new();
    out.push_str("# Lint report\n\n");
    let _ = writeln!(out, "- total: {}", data.summary.total);
    let _ = writeln!(out, "- error: {}", data.summary.by_severity.error);
    let _ = writeln!(out, "- warning: {}", data.summary.by_severity.warning);
    let _ = writeln!(out, "- info: {}\n", data.summary.by_severity.info);

    if let Some(agent) = &data.agent_worker_audit {
        out.push_str("## Agent worker audit\n\n");
        if agent.observed_records {
            let state = agent
                .rollout_state
                .map_or("unconfigured", render_rollout_state);
            let _ = writeln!(out, "- state: {state}");
            let _ = writeln!(out, "- runs: {}", agent.total_runs);
            let _ = writeln!(
                out,
                "- accepted candidates: {} / {}",
                agent.accepted_candidates, agent.generated_candidates
            );
            let _ = writeln!(out, "- cost units: {}", agent.cost_units);
            let _ = writeln!(out, "- tool calls: {}", agent.tool_calls);
            let failures = render_failure_modes(&agent.failure_modes);
            let _ = writeln!(out, "- failures: {failures}");
            if !agent.workers.is_empty() {
                out.push_str("- workers:\n");
                for worker in &agent.workers {
                    let canary_label = worker.canary_label.as_deref().unwrap_or("unlabeled");
                    let failures = render_failure_modes(&worker.failure_modes);
                    let acceptance_rate = render_rate(worker.acceptance_rate);
                    let _ = writeln!(
                        out,
                        "  - {} `{}` ({canary_label}): runs {} (completed {}, failed {}), accepted candidates {} / {} (rate {acceptance_rate}), turns {}, cost units {}, tool calls {}, failures {failures}",
                        render_worker_kind(worker.worker_kind),
                        worker.worker_name,
                        worker.total_runs,
                        worker.completed_runs,
                        worker.failed_runs,
                        worker.accepted_candidates,
                        worker.generated_candidates,
                        worker.turns,
                        worker.cost_units,
                        worker.tool_calls,
                    );
                }
            }
            out.push('\n');
        } else {
            out.push_str("- no agent-worker audit records observed\n\n");
        }
    }

    let deferred = data
        .findings
        .iter()
        .filter(|f| matches!(f.kind, Kind::DeferredCheck))
        .count();
    if deferred > 0 {
        out.push_str("## coverage\n\n");
        let _ = writeln!(
            out,
            "- {deferred} check categor{ies} not yet enforced — see `deferred_check` finding(s) below for tracking issues. A clean exit code does not imply these invariants were verified.",
            ies = if deferred == 1 { "y" } else { "ies" },
        );
        out.push('\n');
    }

    if data.findings.is_empty() {
        out.push_str("_no findings_\n");
        return out;
    }

    out.push_str("## findings\n\n");
    for f in &data.findings {
        let sev = match f.severity {
            Severity::Error => "ERROR",
            Severity::Warning => "WARN ",
            Severity::Info => "INFO ",
        };
        let _ = write!(out, "- **{sev}** [{:?}] {}", f.kind, f.message);
        if let Some(fix) = &f.suggested_fix {
            let _ = write!(out, "\n  - fix: {fix}");
        }
        if let Some(t) = &f.target {
            if let Some(rid) = &t.record_id {
                let _ = write!(out, "\n  - record: {}", rid.0);
            } else if let Some(p) = &t.path {
                let _ = write!(out, "\n  - path: {p}");
            } else if let Some(op) = &t.operation_id {
                let _ = write!(out, "\n  - operation: {}", op.0);
            }
        }
        if let Some(issue) = f.tracking_issue {
            let _ = write!(out, "\n  - tracking: #{issue}");
        }
        out.push('\n');
    }
    out
}

fn render_rollout_state(state: AgentWorkerAuditReportRolloutState) -> &'static str {
    match state {
        AgentWorkerAuditReportRolloutState::Paused => "paused",
        AgentWorkerAuditReportRolloutState::Canary => "canary",
        AgentWorkerAuditReportRolloutState::Enabled => "enabled",
        AgentWorkerAuditReportRolloutState::RolledBack => "rolled_back",
    }
}

fn render_worker_kind(kind: AgentWorkerAuditWorkerWorkerKind) -> &'static str {
    match kind {
        AgentWorkerAuditWorkerWorkerKind::Extractor => "extractor",
        AgentWorkerAuditWorkerWorkerKind::Dream => "dream",
    }
}

fn render_rate(rate: Option<f64>) -> String {
    rate.map_or_else(|| "n/a".to_owned(), |value| format!("{value:.3}"))
}

fn render_failure_modes(value: &serde_json::Value) -> String {
    let Some(map) = value.as_object() else {
        return "none".to_owned();
    };
    if map.is_empty() {
        return "none".to_owned();
    }
    let rendered = map
        .iter()
        .filter_map(|(key, value)| value.as_u64().map(|count| format!("{key}={count}")))
        .collect::<Vec<_>>()
        .join(", ");
    if rendered.is_empty() {
        "none".to_owned()
    } else {
        rendered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::verbs::lint::{
        Finding, Kind, LintDataSummary, LintDataSummaryBySeverity, Target,
    };

    fn empty_summary() -> LintDataSummary {
        LintDataSummary {
            auto_resolved: None,
            total: 0,
            by_severity: LintDataSummaryBySeverity {
                error: 0,
                warning: 0,
                info: 0,
            },
            by_kind: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    #[test]
    fn empty_data_renders_no_findings_block() {
        let data = LintData {
            agent_worker_audit: None,
            findings: vec![],
            summary: empty_summary(),
            report_path: None,
        };
        let s = render(&data);
        insta::assert_snapshot!(s);
    }

    #[test]
    fn finding_with_path_target_and_fix_renders() {
        let f = Finding {
            entities: None,
            kind: Kind::IndexDrift,
            severity: Severity::Error,
            message: "FTS5 rows (8) do not match active records (10)".to_owned(),
            suggested_fix: Some("rebuild the FTS5 mirror".to_owned()),
            target: Some(Target {
                record_id: None,
                operation_id: None,
                path: Some("records_fts".to_owned()),
            }),
            tracking_issue: None,
        };
        let mut summary = empty_summary();
        summary.total = 1;
        summary.by_severity.error = 1;
        let mut by_kind = serde_json::Map::new();
        by_kind.insert("index_drift".into(), serde_json::Value::Number(1.into()));
        summary.by_kind = serde_json::Value::Object(by_kind);
        let data = LintData {
            agent_worker_audit: None,
            findings: vec![f],
            summary,
            report_path: None,
        };
        let s = render(&data);
        insta::assert_snapshot!(s);
    }

    #[test]
    fn deferred_info_finding_with_tracking_issue_renders() {
        let f = Finding {
            entities: None,
            kind: Kind::DeferredCheck,
            severity: Severity::Info,
            message: "sensor-consent enforcement requires the receipt timeline introduced in #253"
                .to_owned(),
            suggested_fix: Some("ship #253 to enable §6.5".to_owned()),
            target: None,
            tracking_issue: Some(253),
        };
        let mut summary = empty_summary();
        summary.total = 1;
        summary.by_severity.info = 1;
        let mut by_kind = serde_json::Map::new();
        by_kind.insert("deferred_check".into(), serde_json::Value::Number(1.into()));
        summary.by_kind = serde_json::Value::Object(by_kind);
        let data = LintData {
            agent_worker_audit: None,
            findings: vec![f],
            summary,
            report_path: None,
        };
        let s = render(&data);
        insta::assert_snapshot!(s);
    }

    #[test]
    fn agent_worker_audit_section_renders_without_body_text() {
        let data = LintData {
            agent_worker_audit: Some(crate::generated::verbs::lint::AgentWorkerAuditReport {
                rollout_state: Some(
                    crate::generated::verbs::lint::AgentWorkerAuditReportRolloutState::Canary,
                ),
                failure_modes: serde_json::json!({"budget_exceeded": 2}),
                workers: vec![crate::generated::verbs::lint::AgentWorkerAuditWorker {
                    worker_kind:
                        crate::generated::verbs::lint::AgentWorkerAuditWorkerWorkerKind::Extractor,
                    worker_name: "agent_extractor".to_owned(),
                    canary_label: Some("canary-05".to_owned()),
                    total_runs: 4,
                    completed_runs: 2,
                    failed_runs: 2,
                    generated_candidates: 10,
                    accepted_candidates: 5,
                    acceptance_rate: Some(0.5),
                    turns: 8,
                    tool_calls: 12,
                    cost_units: 200,
                    failure_modes: serde_json::json!({"budget_exceeded": 2}),
                }],
                observed_records: true,
                total_runs: 4,
                completed_runs: 2,
                failed_runs: 2,
                generated_candidates: 10,
                accepted_candidates: 5,
                acceptance_rate: Some(0.5),
                turns: 8,
                tool_calls: 12,
                cost_units: 200,
            }),
            findings: vec![],
            summary: empty_summary(),
            report_path: None,
        };

        let rendered = render(&data);

        assert!(rendered.contains("## Agent worker audit"));
        assert!(rendered.contains("- state: canary"));
        assert!(rendered.contains("- accepted candidates: 5 / 10"));
        assert!(rendered.contains("- failures: budget_exceeded=2"));
        assert!(rendered.contains("- workers:"));
        assert!(rendered.contains(
            "- extractor `agent_extractor` (canary-05): runs 4 (completed 2, failed 2), accepted candidates 5 / 10 (rate 0.500), turns 8, cost units 200, tool calls 12, failures budget_exceeded=2"
        ));
        assert!(!rendered.contains("prompt"));
        assert!(!rendered.contains("candidate body"));
    }

    #[test]
    fn malformed_agent_failure_mode_counts_render_none() {
        let rendered = render_failure_modes(&serde_json::json!({
            "budget_exceeded": "2",
        }));

        assert_eq!(rendered, "none");
    }
}
