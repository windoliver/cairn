//! `cairn lint` handler.

use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use cairn_core::domain::{LintFinding, LintKind, Severity};
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use cairn_core::generated::verbs::lint::{
    LintData, LintDataFindings, LintDataFindingsKind, LintDataFindingsSeverity, LintDataSummary,
};
use cairn_store_sqlite::{
    EdgeLintReport, StoreError, lint_edges, migrate, resolve_edge_contradictions,
};
use clap::ArgMatches;
use rusqlite::{Connection, OpenFlags};

use super::envelope::{emit_json, human_error, new_operation_id};

/// Run `cairn lint`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let fix = sub.get_flag("fix");
    let operation_id = new_operation_id();

    match run_lint(fix, &operation_id) {
        Ok(report) => {
            let has_blocking_findings = report.findings.iter().any(has_warning_or_error);
            let data = lint_data(report);
            let response = committed_response(operation_id, data);
            if json {
                emit_json(&response);
            } else if let Some(ResponseData::Lint(data)) = response.data.as_ref() {
                emit_human(data, &response.operation_id);
            }

            if has_blocking_findings && !fix {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(err) => {
            let message = err.to_string();
            let response = aborted_response(operation_id, &message);
            if json {
                emit_json(&response);
            } else {
                human_error("lint", "Internal", &message, &response.operation_id);
            }
            ExitCode::FAILURE
        }
    }
}

fn run_lint(fix: bool, operation_id: &Ulid) -> Result<EdgeLintReport, StoreError> {
    let db_path = Path::new(".cairn").join("cairn.db");

    if fix {
        let mut conn = Connection::open(db_path)?;
        migrate(&conn)?;
        resolve_edge_contradictions(&mut conn, unix_now_seconds(), &operation_id.0)
    } else {
        let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        lint_edges(&conn)
    }
}

fn committed_response(operation_id: Ulid, data: LintData) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: Some(ResponseData::Lint(data)),
        error: None,
        operation_id,
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Committed,
        target: None,
        verb: ResponseVerb::Lint,
    }
}

fn aborted_response(operation_id: Ulid, message: &str) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "Internal",
            "message": message,
        })),
        operation_id,
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Aborted,
        target: None,
        verb: ResponseVerb::Lint,
    }
}

fn lint_data(report: EdgeLintReport) -> LintData {
    let total = usize_to_u64(report.findings.len());
    LintData {
        findings: report.findings.into_iter().map(lint_finding).collect(),
        report_path: None,
        summary: LintDataSummary {
            ambiguous_edges: Some(report.ambiguous_edges),
            auto_resolved: Some(report.auto_resolved),
            contradictions: Some(report.contradictions),
            orphans: Some(0),
            stale: Some(0),
            total,
        },
    }
}

fn lint_finding(finding: LintFinding) -> LintDataFindings {
    LintDataFindings {
        candidate_edge_ids: None,
        chosen_edge_id: None,
        confidence: None,
        confidence_score: None,
        conflict_group_id: None,
        entities: Some(finding.entities),
        entity_id: None,
        fix_applied: None,
        kind: lint_kind(finding.kind),
        message: finding.message,
        record_id: None,
        relation: None,
        resolution_reason: None,
        severity: Some(lint_severity(finding.severity)),
        suggestion: finding.suggestion,
    }
}

fn lint_kind(kind: LintKind) -> LintDataFindingsKind {
    match kind {
        LintKind::ContradictoryEdge => LintDataFindingsKind::ContradictoryEdge,
        LintKind::AmbiguousEdge => LintDataFindingsKind::AmbiguousEdge,
    }
}

fn lint_severity(severity: Severity) -> LintDataFindingsSeverity {
    match severity {
        Severity::Info => LintDataFindingsSeverity::Info,
        Severity::Warning => LintDataFindingsSeverity::Warning,
        Severity::Error => LintDataFindingsSeverity::Error,
    }
}

fn has_warning_or_error(finding: &LintFinding) -> bool {
    matches!(finding.severity, Severity::Warning | Severity::Error)
}

fn emit_human(data: &LintData, operation_id: &Ulid) {
    println!("cairn lint: committed (operation_id: {})", operation_id.0);
    println!(
        "summary: total={} contradictions={} ambiguous_edges={} auto_resolved={}",
        data.summary.total,
        data.summary.contradictions.unwrap_or(0),
        data.summary.ambiguous_edges.unwrap_or(0),
        data.summary.auto_resolved.unwrap_or(0)
    );

    for finding in &data.findings {
        let severity = finding
            .severity
            .map_or("unknown", |severity| match severity {
                LintDataFindingsSeverity::Info => "info",
                LintDataFindingsSeverity::Warning => "warning",
                LintDataFindingsSeverity::Error => "error",
            });
        println!("{severity}: {}", finding.message);
    }
}

fn unix_now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
