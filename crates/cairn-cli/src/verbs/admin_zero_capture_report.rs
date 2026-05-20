//! `cairn admin zero-capture-report [--json]`
//!
//! Renders a markdown or JSON summary for zero-capture audits emitted in
//! `.cairn/metrics.jsonl`.

use std::path::Path;
use std::process::ExitCode;

use cairn_core::domain::{ZeroCaptureReport, ZeroCaptureReportSummary, render_zero_capture_report};
use clap::ArgMatches;
use serde::Serialize;

use super::envelope::{human_error, new_operation_id};

#[derive(Debug, Serialize)]
struct ZeroCaptureReportOutput {
    summary: ZeroCaptureReportSummary,
    reports: Vec<ZeroCaptureReport>,
}

/// Run `cairn admin zero-capture-report`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let json = sub.get_flag("json");
    let reports = match load_reports_from_metrics(vault_root) {
        Ok(reports) => reports,
        Err(msg) => {
            let op_id = new_operation_id();
            if json {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "operation_id": op_id.0,
                        "error": { "code": "InvalidInput", "message": msg }
                    })
                );
            } else {
                human_error("admin zero-capture-report", "InvalidInput", &msg, &op_id);
            }
            return ExitCode::from(65); // EX_DATAERR
        }
    };

    if json {
        let output = ZeroCaptureReportOutput {
            summary: ZeroCaptureReportSummary::from_reports(&reports),
            reports,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&output)
                .expect("invariant: ZeroCaptureReportOutput is always serializable")
        );
    } else {
        print!("{}", render_zero_capture_report(&reports));
    }

    ExitCode::SUCCESS
}

#[derive(Debug, serde::Deserialize)]
struct ZeroCaptureAuditMetricRow {
    session_id: String,
    activity_count: u64,
    successful_write_count: u64,
    decision: String,
}

fn load_reports_from_metrics(vault_root: &Path) -> Result<Vec<ZeroCaptureReport>, String> {
    let metrics_path = vault_root.join(".cairn").join("metrics.jsonl");
    if !metrics_path.exists() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(&metrics_path)
        .map_err(|error| format!("reading {}: {error}", metrics_path.display()))?;
    let mut reports = Vec::new();
    for (line_no, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let raw = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(raw) => raw,
            Err(error) if metrics_line_sets_event(line, "zero_capture_audit") => {
                return Err(format!(
                    "parsing {} line {}: {error}",
                    metrics_path.display(),
                    line_no + 1
                ));
            }
            Err(_) => continue,
        };
        let event = raw.get("event").and_then(serde_json::Value::as_str);
        if event != Some("zero_capture_audit") {
            continue;
        }
        let row: ZeroCaptureAuditMetricRow = serde_json::from_value(raw).map_err(|error| {
            format!(
                "invalid zero-capture row in {} line {}: {error}",
                metrics_path.display(),
                line_no + 1
            )
        })?;
        let session_id =
            cairn_core::domain::SessionId::parse(&row.session_id).map_err(|error| {
                format!("invalid session_id in {}: {error}", metrics_path.display())
            })?;
        let decision: cairn_core::domain::ZeroCaptureDecisionCode =
            serde_json::from_str(&format!("\"{}\"", row.decision)).map_err(|error| {
                format!("invalid decision in {}: {error}", metrics_path.display())
            })?;
        reports.push(ZeroCaptureReport {
            session_id,
            activity_count: row.activity_count,
            successful_write_count: row.successful_write_count,
            decision,
        });
    }
    Ok(reports)
}

fn metrics_line_sets_event(line: &str, event: &str) -> bool {
    let Some(after_key) = line.split_once("\"event\"").map(|(_, rest)| rest) else {
        return false;
    };
    let after_key = after_key.trim_start();
    let Some(after_colon) = after_key.strip_prefix(':') else {
        return false;
    };
    let after_colon = after_colon.trim_start();
    let Some(after_quote) = after_colon.strip_prefix('"') else {
        return false;
    };
    let Some(after_event) = after_quote.strip_prefix(event) else {
        return false;
    };
    after_event.is_empty() || after_event.starts_with('"')
}
