//! Operator-facing SRE report builder and renderers.

use std::{path::Path, process::ExitCode};

use cairn_core::domain::metrics::MetricEvent;
use cairn_core::domain::{
    SreDetail, SreGateResult, SreGateSummary, SreMeasurement, SrePrivacySummary,
    SreProjectionSummary, SreRehydrationSummary, SreReport, SreSearchModeSummary, SreSearchSummary,
    SreStatus, SreVaultSummary, SreWorkflowSummary, classify_threshold,
};
use clap::ArgMatches;
use serde::Deserialize;

use crate::{config, metrics};

/// Cold rehydration latency SLO, in milliseconds.
pub const SLO_COLD_REHYDRATE_MS: f64 = 3_000.0;

/// Run `cairn admin sre report`.
#[must_use]
pub fn run_report(matches: &ArgMatches, vault_root: &Path) -> ExitCode {
    let json = matches.get_flag("json");
    let bench_report_dir = matches.get_one::<String>("bench-report-dir").map(Path::new);
    if let Some(dir) = bench_report_dir
        && !dir.is_dir()
    {
        eprintln!("cairn admin sre report: bench-report-dir is not a directory");
        return ExitCode::from(78);
    }
    let config = match config::load(vault_root, &config::CliOverrides::default()) {
        Ok(config) => config,
        Err(_err) => {
            eprintln!("cairn admin sre report: config error");
            return ExitCode::from(78);
        }
    };
    let report = match build_report_with_bench(vault_root, &config, bench_report_dir) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("cairn admin sre report: bench-report-dir error - {err}");
            return ExitCode::from(78);
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string(&report).expect("SRE report serializes")
        );
    } else {
        println!("{}", render_human(&report));
    }
    ExitCode::SUCCESS
}

/// Build a scrubbed local SRE report from vault-local metrics.
#[must_use]
pub fn build_report(vault_root: &Path, _config: &cairn_core::config::CairnConfig) -> SreReport {
    build_report_with_bench(vault_root, _config, None).expect("no bench dir cannot fail")
}

fn build_report_with_bench(
    vault_root: &Path,
    _config: &cairn_core::config::CairnConfig,
    bench_report_dir: Option<&Path>,
) -> Result<SreReport, String> {
    let events = read_metric_events(vault_root);
    let rehydration_latencies: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            MetricEvent::RehydrationCompleted {
                latency_ms, status, ..
            } if status == "committed" => Some(*latency_ms),
            _ => None,
        })
        .collect();
    let search_modes = summarize_search(&events);
    let p95 = percentile_u64(&rehydration_latencies, 0.95);
    let rehydration_status = classify_threshold(p95, SLO_COLD_REHYDRATE_MS);
    let gates = load_bench_gates(bench_report_dir)?;
    Ok(SreReport {
        schema_version: 1,
        captured_at_ms: metrics::now_ms(),
        vault: SreVaultSummary {
            id_hash: "sha256:local-vault".into(),
            name: vault_root
                .file_name()
                .map(|s| stable_string(&s.to_string_lossy(), "local_vault"))
                .unwrap_or_else(|| "local_vault".into()),
        },
        workflow: SreWorkflowSummary {
            status: SreStatus::Unknown,
            oldest_queued_age_ms: None,
            longest_held_lease_ms: None,
            dead_letter_count: 0,
            kinds: Vec::new(),
        },
        rehydration: SreRehydrationSummary {
            status: rehydration_status,
            latest_latency_ms: rehydration_latencies.last().copied(),
            p95_latency_ms: p95.and_then(SreMeasurement::new),
            slo_ms: SreMeasurement::new(SLO_COLD_REHYDRATE_MS).expect("finite rehydration SLO"),
            sample_count: rehydration_latencies.len() as u64,
            last_gate: None,
        },
        projection: SreProjectionSummary {
            status: SreStatus::Unknown,
            nexus_state: "unknown".into(),
            nexus_reason: None,
            targets: Vec::new(),
        },
        search: SreSearchSummary {
            status: rollup_search_status(&search_modes),
            modes: search_modes,
        },
        gates,
        privacy: SrePrivacySummary {
            scrubbed: true,
            forbidden_field_count: 0,
        },
    })
}

fn read_metric_events(vault_root: &Path) -> Vec<MetricEvent> {
    let path = vault_root.join(".cairn/metrics.jsonl");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| serde_json::from_str::<MetricEvent>(line).ok())
        .collect()
}

fn summarize_search(events: &[MetricEvent]) -> Vec<SreSearchModeSummary> {
    ["keyword", "semantic", "hybrid"]
        .into_iter()
        .map(|mode| {
            let mut invocations = 0_u64;
            let mut degraded = 0_u64;
            let mut failed = 0_u64;
            let mut latencies = Vec::new();
            for event in events {
                if let MetricEvent::SearchCompleted {
                    mode: event_mode,
                    latency_ms,
                    degradation_state,
                    error,
                    ..
                } = event
                    && event_mode == mode
                {
                    invocations += 1;
                    latencies.push(*latency_ms);
                    if degradation_state != "none" {
                        degraded += 1;
                    }
                    if error.is_some() {
                        failed += 1;
                    }
                }
            }
            SreSearchModeSummary {
                mode: mode.into(),
                advertised: true,
                invocations,
                degraded,
                failed,
                p95_latency_ms: percentile_u64(&latencies, 0.95).and_then(SreMeasurement::new),
                status: if invocations == 0 {
                    SreStatus::Unknown
                } else if failed > 0 {
                    SreStatus::Fail
                } else if degraded > 0 {
                    SreStatus::Warning
                } else {
                    SreStatus::Ok
                },
            }
        })
        .collect()
}

fn rollup_search_status(modes: &[SreSearchModeSummary]) -> SreStatus {
    rollup_status(modes.iter().map(|mode| mode.status))
}

fn percentile_u64(values: &[u64], percentile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() - 1) as f64 * percentile).round() as usize;
    sorted.get(idx).map(|value| *value as f64)
}

#[derive(Deserialize)]
struct BenchSreReport {
    checks: Option<Vec<BenchSreCheck>>,
}

#[derive(Deserialize)]
struct BenchSreCheck {
    name: Option<String>,
    status: Option<String>,
    measured: Option<f64>,
    threshold: Option<f64>,
    unit: Option<String>,
    detail: Option<String>,
}

fn load_bench_gates(bench_report_dir: Option<&Path>) -> Result<SreGateSummary, String> {
    let Some(dir) = bench_report_dir else {
        return Ok(empty_gate_summary());
    };
    let path = dir.join("sre.json");
    if !path.exists() {
        return Ok(empty_gate_summary());
    }
    let raw = std::fs::read_to_string(&path).map_err(|_err| "failed to read sre.json")?;
    let report: BenchSreReport =
        serde_json::from_str(&raw).map_err(|_err| "failed to parse sre.json")?;
    let checks = report.checks.ok_or("invalid sre.json schema")?;
    let gates: Vec<SreGateResult> = checks
        .into_iter()
        .map(|check| {
            let name = check.name.ok_or("invalid sre.json schema")?;
            let status = check.status.ok_or("invalid sre.json schema")?;
            let unit = check.unit.ok_or("invalid sre.json schema")?;
            Ok(SreGateResult {
                name: stable_string(&name, "redacted_gate"),
                status: parse_gate_status(&status),
                measured: check.measured.and_then(SreMeasurement::new),
                threshold: check.threshold.and_then(SreMeasurement::new),
                unit: stable_string(&unit, "redacted"),
                detail: check.detail.map(|detail| {
                    SreDetail::stable(&detail).unwrap_or_else(|| SreDetail::from_raw(&detail))
                }),
            })
        })
        .collect::<Result<_, &'static str>>()?;
    Ok(SreGateSummary {
        status: rollup_gate_status(&gates),
        gates,
    })
}

fn stable_string(raw: &str, fallback: &str) -> String {
    SreDetail::stable(raw)
        .map(|detail| detail.as_str().to_owned())
        .unwrap_or_else(|| fallback.to_owned())
}

fn empty_gate_summary() -> SreGateSummary {
    SreGateSummary {
        status: SreStatus::Unknown,
        gates: Vec::new(),
    }
}

fn parse_gate_status(status: &str) -> SreStatus {
    match status.to_ascii_lowercase().as_str() {
        "ok" | "pass" | "passed" => SreStatus::Ok,
        "warning" | "warn" => SreStatus::Warning,
        "fail" | "failed" => SreStatus::Fail,
        _ => SreStatus::Unknown,
    }
}

fn rollup_gate_status(gates: &[SreGateResult]) -> SreStatus {
    if gates.is_empty() {
        return SreStatus::Unknown;
    }
    let mut saw_warning = false;
    let mut saw_unknown = false;
    for gate in gates {
        match gate.status {
            SreStatus::Fail => return SreStatus::Fail,
            SreStatus::Warning => saw_warning = true,
            SreStatus::Unknown => saw_unknown = true,
            SreStatus::Ok => {}
        }
    }
    if saw_unknown {
        SreStatus::Unknown
    } else if saw_warning {
        SreStatus::Warning
    } else {
        SreStatus::Ok
    }
}

/// Render a compact operator-readable SRE report.
#[must_use]
pub fn render_human(report: &SreReport) -> String {
    format!(
        "SRE status: {overall}\nworkflow: {workflow:?}\nrehydration: {rehydration:?} (p95 {p95:?} / {slo:.0}ms)\nprojection: {projection:?}\nsearch: {search:?}\ngates: {gates:?}",
        overall = status_text(report),
        workflow = report.workflow.status,
        rehydration = report.rehydration.status,
        p95 = report.rehydration.p95_latency_ms.map(SreMeasurement::get),
        slo = report.rehydration.slo_ms.get(),
        projection = report.projection.status,
        search = report.search.status,
        gates = report.gates.status,
    )
}

fn status_text(report: &SreReport) -> &'static str {
    match rollup_status([
        report.workflow.status,
        report.rehydration.status,
        report.projection.status,
        report.search.status,
        report.gates.status,
    ]) {
        SreStatus::Fail => "fail",
        SreStatus::Warning => "warning",
        SreStatus::Unknown => "unknown",
        SreStatus::Ok => "ok",
    }
}

fn rollup_status(statuses: impl IntoIterator<Item = SreStatus>) -> SreStatus {
    let mut saw_warning = false;
    let mut saw_unknown = false;
    for status in statuses {
        match status {
            SreStatus::Fail => return SreStatus::Fail,
            SreStatus::Warning => saw_warning = true,
            SreStatus::Unknown => saw_unknown = true,
            SreStatus::Ok => {}
        }
    }
    if saw_unknown {
        SreStatus::Unknown
    } else if saw_warning {
        SreStatus::Warning
    } else {
        SreStatus::Ok
    }
}
