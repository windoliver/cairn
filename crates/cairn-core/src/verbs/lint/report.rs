//! `LintData → markdown` projector for `cairn lint --write-report`.
//!
//! Pure function: takes a `LintData` envelope, renders the canonical
//! `.cairn/lint-report.md` body. The CLI handler (Task 16) writes the
//! result atomically via `tempfile::Builder::tempfile_in` + `persist`.

use std::fmt::Write as _;

use crate::generated::verbs::lint::{LintData, Severity};

/// Render `data` as the canonical lint-report markdown body.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::verbs::lint::{
        Finding, Kind, LintDataSummary, LintDataSummaryBySeverity, Target,
    };

    fn empty_summary() -> LintDataSummary {
        LintDataSummary {
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
            findings: vec![f],
            summary,
            report_path: None,
        };
        let s = render(&data);
        insta::assert_snapshot!(s);
    }
}
