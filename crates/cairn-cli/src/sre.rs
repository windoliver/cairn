//! Operator-facing SRE report builder and renderers.

use std::{collections::BTreeMap, path::Path, process::ExitCode};

use cairn_core::domain::metrics::MetricEvent;
use cairn_core::domain::{
    SreDetail, SreGateResult, SreGateSummary, SreMeasurement, SrePrivacySummary,
    SreProjectionSummary, SreProjectionTargetSummary, SreRehydrationSummary, SreReport,
    SreSearchModeSummary, SreSearchSummary, SreStatus, SreVaultSummary, SreWorkflowKindSummary,
    SreWorkflowSummary, classify_threshold,
};
use clap::ArgMatches;
use serde::Deserialize;
use serde_json::Value;

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
pub fn build_report(vault_root: &Path, config: &cairn_core::config::CairnConfig) -> SreReport {
    build_report_with_bench(vault_root, config, None).expect("no bench dir cannot fail")
}

fn build_report_with_bench(
    vault_root: &Path,
    config: &cairn_core::config::CairnConfig,
    bench_report_dir: Option<&Path>,
) -> Result<SreReport, String> {
    let metrics = read_metric_events(vault_root);
    let events = metrics.events;
    let rehydration_latencies: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            MetricEvent::RehydrationCompleted {
                latency_ms, status, ..
            } if status == "committed" => Some(*latency_ms),
            _ => None,
        })
        .collect();
    let search_modes = summarize_search(&events, config, vault_root);
    let workflow = summarize_workflow(&events);
    let projection = summarize_projection(&events);
    let p95 = percentile_u64(&rehydration_latencies, 0.95);
    let rehydration_status = classify_threshold(p95, SLO_COLD_REHYDRATE_MS);
    let mut gates = load_bench_gates(bench_report_dir)?;
    add_metric_parse_gate(&mut gates, metrics.parse_error_count);
    Ok(SreReport {
        schema_version: 1,
        captured_at_ms: metrics::now_ms(),
        vault: SreVaultSummary {
            id_hash: "sha256:local-vault".into(),
            name: "local_vault".into(),
        },
        workflow,
        rehydration: SreRehydrationSummary {
            status: rehydration_status,
            latest_latency_ms: rehydration_latencies.last().copied(),
            p95_latency_ms: p95.and_then(SreMeasurement::new),
            slo_ms: SreMeasurement::new(SLO_COLD_REHYDRATE_MS).expect("finite rehydration SLO"),
            sample_count: rehydration_latencies.len() as u64,
            last_gate: None,
        },
        projection,
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

struct ReadMetricEvents {
    events: Vec<MetricEvent>,
    parse_error_count: u64,
}

fn read_metric_events(vault_root: &Path) -> ReadMetricEvents {
    let path = vault_root.join(".cairn/metrics.jsonl");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return ReadMetricEvents {
            events: Vec::new(),
            parse_error_count: 0,
        };
    };
    let mut events = Vec::new();
    let mut parse_error_count = 0_u64;
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            parse_error_count = parse_error_count.saturating_add(1);
            continue;
        };
        let Some(event_name) = value.get("event").and_then(Value::as_str) else {
            parse_error_count = parse_error_count.saturating_add(1);
            continue;
        };
        if !is_known_metric_event(event_name) {
            continue;
        }
        match serde_json::from_value::<MetricEvent>(value) {
            Ok(event) => events.push(event),
            Err(_err) => parse_error_count = parse_error_count.saturating_add(1),
        }
    }
    ReadMetricEvents {
        events,
        parse_error_count,
    }
}

fn is_known_metric_event(event_name: &str) -> bool {
    matches!(
        event_name,
        "verb_invocation"
            | "search_completed"
            | "sensor_emission"
            | "wal_apply"
            | "hot_prefix_assembled"
            | "projection_rebuild"
            | "trace_canvas_rendered"
            | "evaluation_completed"
            | "rehydration_completed"
            | "workflow_job_started"
            | "workflow_job_completed"
            | "workflow_job_failed"
    )
}

const WORKFLOW_BACKLOG_THRESHOLD_MS: i64 = 300_000;

#[derive(Default)]
struct WorkflowKindAggregate {
    leased: u64,
    done_recent: u64,
    failed_recent: u64,
    oldest_queued_age_ms: Option<i64>,
    last_success_ts_ms: Option<i64>,
    dead_letters: usize,
}

fn summarize_workflow(events: &[MetricEvent]) -> SreWorkflowSummary {
    let mut kinds: BTreeMap<String, WorkflowKindAggregate> = BTreeMap::new();
    let mut saw_workflow_event = false;
    let mut oldest_queued_age_ms: Option<i64> = None;
    let mut latest_ts_ms: Option<i64> = None;

    for event in events {
        match event {
            MetricEvent::WorkflowJobStarted {
                ts_ms,
                kind,
                queue_lag_ms,
                ..
            } => {
                saw_workflow_event = true;
                latest_ts_ms = Some(latest_ts_ms.map_or(*ts_ms, |latest| latest.max(*ts_ms)));
                let kind = workflow_kind_label(kind);
                let entry = kinds.entry(kind).or_default();
                entry.leased = entry.leased.saturating_add(1);
                if *queue_lag_ms >= 0 {
                    entry.oldest_queued_age_ms = Some(
                        entry
                            .oldest_queued_age_ms
                            .map_or(*queue_lag_ms, |current| current.max(*queue_lag_ms)),
                    );
                    oldest_queued_age_ms = Some(
                        oldest_queued_age_ms
                            .map_or(*queue_lag_ms, |current| current.max(*queue_lag_ms)),
                    );
                }
            }
            MetricEvent::WorkflowJobCompleted { ts_ms, kind, .. } => {
                saw_workflow_event = true;
                latest_ts_ms = Some(latest_ts_ms.map_or(*ts_ms, |latest| latest.max(*ts_ms)));
                let kind = workflow_kind_label(kind);
                let entry = kinds.entry(kind).or_default();
                entry.done_recent = entry.done_recent.saturating_add(1);
                entry.last_success_ts_ms = Some(
                    entry
                        .last_success_ts_ms
                        .map_or(*ts_ms, |current| current.max(*ts_ms)),
                );
            }
            MetricEvent::WorkflowJobFailed {
                ts_ms,
                kind,
                disposition,
                ..
            } => {
                saw_workflow_event = true;
                latest_ts_ms = Some(latest_ts_ms.map_or(*ts_ms, |latest| latest.max(*ts_ms)));
                let kind = workflow_kind_label(kind);
                let entry = kinds.entry(kind).or_default();
                entry.failed_recent = entry.failed_recent.saturating_add(1);
                if disposition == "permanent" {
                    entry.dead_letters = entry.dead_letters.saturating_add(1);
                }
            }
            _ => {}
        }
    }

    if !saw_workflow_event {
        return SreWorkflowSummary {
            status: SreStatus::Unknown,
            oldest_queued_age_ms: None,
            longest_held_lease_ms: None,
            dead_letter_count: 0,
            kinds: Vec::new(),
        };
    }

    let latest_ts_ms = latest_ts_ms.unwrap_or(0);
    let mut dead_letter_count = 0_usize;
    let kinds: Vec<SreWorkflowKindSummary> = kinds
        .into_iter()
        .map(|(kind, aggregate)| {
            dead_letter_count = dead_letter_count.saturating_add(aggregate.dead_letters);
            let status = if aggregate.failed_recent > 0 || aggregate.dead_letters > 0 {
                SreStatus::Warning
            } else {
                SreStatus::Ok
            };
            SreWorkflowKindSummary {
                kind,
                queued: 0,
                leased: aggregate.leased,
                done_recent: aggregate.done_recent,
                failed_recent: aggregate.failed_recent,
                oldest_queued_age_ms: aggregate.oldest_queued_age_ms,
                last_success_age_ms: aggregate
                    .last_success_ts_ms
                    .map(|success_ts| latest_ts_ms.saturating_sub(success_ts).max(0)),
                backlog_threshold_ms: WORKFLOW_BACKLOG_THRESHOLD_MS,
                status,
            }
        })
        .collect();

    let status = if kinds.iter().any(|kind| kind.status == SreStatus::Warning) {
        SreStatus::Warning
    } else {
        SreStatus::Ok
    };

    SreWorkflowSummary {
        status,
        oldest_queued_age_ms,
        longest_held_lease_ms: None,
        dead_letter_count,
        kinds,
    }
}

#[derive(Default)]
struct ProjectionTargetAggregate {
    current: u64,
    failed: u64,
    max_lag_ms: Option<i64>,
    last_rebuild_latency_ms: Option<u64>,
    degraded: bool,
}

fn summarize_projection(events: &[MetricEvent]) -> SreProjectionSummary {
    let mut targets: BTreeMap<String, ProjectionTargetAggregate> = BTreeMap::new();
    let mut saw_projection_event = false;

    for event in events {
        let MetricEvent::ProjectionRebuild {
            projection,
            status,
            latency_ms,
            records_rebuilt,
            queue_lag_ms,
            error,
            degradation_state,
            ..
        } = event
        else {
            continue;
        };
        saw_projection_event = true;
        let target = projection_target_label(projection);
        let entry = targets.entry(target).or_default();
        if status == "committed" {
            entry.current = entry.current.saturating_add(*records_rebuilt);
        } else {
            entry.failed = entry.failed.saturating_add(1);
        }
        if error.is_some()
            || degradation_state
                .as_deref()
                .is_some_and(|state| state != "none")
        {
            entry.degraded = true;
        }
        if *queue_lag_ms >= 0 {
            entry.max_lag_ms = Some(
                entry
                    .max_lag_ms
                    .map_or(*queue_lag_ms, |current| current.max(*queue_lag_ms)),
            );
        }
        entry.last_rebuild_latency_ms = Some(*latency_ms);
    }

    if !saw_projection_event {
        return SreProjectionSummary {
            status: SreStatus::Unknown,
            nexus_state: "unknown".into(),
            nexus_reason: None,
            targets: Vec::new(),
        };
    }

    let targets: Vec<SreProjectionTargetSummary> = targets
        .into_iter()
        .map(|(target, aggregate)| {
            let status = if aggregate.failed > 0 || aggregate.degraded {
                SreStatus::Warning
            } else {
                SreStatus::Ok
            };
            SreProjectionTargetSummary {
                target,
                current: aggregate.current,
                stale: 0,
                failed: aggregate.failed,
                missing: 0,
                max_lag_ms: aggregate.max_lag_ms,
                last_rebuild_latency_ms: aggregate.last_rebuild_latency_ms,
                status,
            }
        })
        .collect();
    let status = if targets
        .iter()
        .any(|target| target.status == SreStatus::Warning)
    {
        SreStatus::Warning
    } else {
        SreStatus::Ok
    };
    SreProjectionSummary {
        status,
        nexus_state: if status == SreStatus::Warning {
            "degraded".into()
        } else {
            "healthy".into()
        },
        nexus_reason: if status == SreStatus::Warning {
            Some("projection_rebuild_warning".into())
        } else {
            None
        },
        targets,
    }
}

fn workflow_kind_label(raw: &str) -> String {
    match raw {
        "dream.light" => raw.to_owned(),
        _ => "redacted_workflow".to_owned(),
    }
}

fn projection_target_label(raw: &str) -> String {
    match raw {
        "sqlite.from_db" => raw.to_owned(),
        _ => "redacted_projection".to_owned(),
    }
}

fn summarize_search(
    events: &[MetricEvent],
    config: &cairn_core::config::CairnConfig,
    vault_root: &Path,
) -> Vec<SreSearchModeSummary> {
    let caps = search_capabilities(config, vault_root);
    let search_completed_observations: Vec<(&str, i64, u64)> = events
        .iter()
        .filter_map(|event| match event {
            MetricEvent::SearchCompleted {
                mode,
                ts_ms,
                latency_ms,
                ..
            } => Some((mode.as_str(), *ts_ms, *latency_ms)),
            _ => None,
        })
        .collect();
    ["keyword", "semantic", "hybrid"]
        .into_iter()
        .map(|mode| {
            let mut invocations = 0_u64;
            let mut degraded = 0_u64;
            let mut failed = 0_u64;
            let mut latencies = Vec::new();
            for event in events {
                let Some(observation) =
                    search_observation(event, mode, &search_completed_observations)
                else {
                    continue;
                };
                invocations += 1;
                latencies.push(observation.latency_ms);
                if observation.degraded {
                    degraded += 1;
                }
                if observation.failed {
                    failed += 1;
                }
            }
            let advertised = match mode {
                "keyword" => caps.keyword_search,
                "semantic" => caps.semantic_search,
                "hybrid" => caps.hybrid_search,
                _ => false,
            };
            SreSearchModeSummary {
                mode: mode.into(),
                advertised,
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

struct SearchObservation {
    latency_ms: u64,
    degraded: bool,
    failed: bool,
}

fn search_observation(
    event: &MetricEvent,
    mode: &str,
    search_completed_observations: &[(&str, i64, u64)],
) -> Option<SearchObservation> {
    match event {
        MetricEvent::SearchCompleted {
            mode: event_mode,
            latency_ms,
            degradation_state,
            error,
            ..
        } if event_mode == mode => Some(SearchObservation {
            latency_ms: *latency_ms,
            degraded: degradation_state != "none",
            failed: error.is_some(),
        }),
        MetricEvent::VerbInvocation {
            ts_ms,
            verb,
            surface,
            mode: Some(event_mode),
            status,
            latency_ms,
            error,
            degradation_state,
            ..
        } if verb == "search"
            && event_mode == mode
            && (surface != "cli"
                || !search_completed_observations.contains(&(
                    event_mode.as_str(),
                    *ts_ms,
                    *latency_ms,
                )))
            && (status != "committed" || error.is_some()) =>
        {
            Some(SearchObservation {
                latency_ms: *latency_ms,
                degraded: degradation_state
                    .as_deref()
                    .is_some_and(|state| state != "none"),
                failed: true,
            })
        }
        _ => None,
    }
}

fn search_capabilities(
    config: &cairn_core::config::CairnConfig,
    vault_root: &Path,
) -> cairn_core::config::CapabilitySet {
    let models_root = vault_root.join(".cairn").join("models");
    let cache = cairn_embeddings_local::ModelCache::new(&models_root);
    let kind = config.search.embedding_model;
    let mock_embedder = std::env::var("CAIRN_MOCK_EMBEDDER").as_deref() == Ok("1");
    let model_present = mock_embedder || cache.is_present(kind);
    let provider_ready =
        crate::verbs::embedding_provider_ready(config, model_present, Some(vault_root));
    config.capabilities(provider_ready)
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
                name: allowlisted_gate_name(&name),
                status: parse_gate_status(&status),
                measured: check.measured.and_then(SreMeasurement::new),
                threshold: check.threshold.and_then(SreMeasurement::new),
                unit: allowlisted_gate_unit(&unit),
                detail: check.detail.map(|_detail| SreDetail::from_raw("SECRET")),
            })
        })
        .collect::<Result<_, &'static str>>()?;
    Ok(SreGateSummary {
        status: rollup_gate_status(&gates),
        gates,
    })
}

fn allowlisted_gate_name(raw: &str) -> String {
    match raw {
        "migration_backlog"
        | "sre_privacy_scrub"
        | "cold_rehydrate_p95"
        | "projection_lag_fixture" => raw.to_owned(),
        _ => "redacted_gate".to_owned(),
    }
}

fn allowlisted_gate_unit(raw: &str) -> String {
    match raw {
        "ms" | "count" | "forbidden_fields" | "bytes" | "records" => raw.to_owned(),
        _ => "redacted".to_owned(),
    }
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

fn add_metric_parse_gate(gates: &mut SreGateSummary, parse_error_count: u64) {
    if parse_error_count == 0 {
        return;
    }
    gates.gates.push(SreGateResult {
        name: "metric_parse_errors".into(),
        status: SreStatus::Warning,
        measured: SreMeasurement::new(parse_error_count as f64),
        threshold: SreMeasurement::new(0.0),
        unit: "count".into(),
        detail: Some(SreDetail::from_raw("SECRET")),
    });
    gates.status = rollup_gate_status(&gates.gates);
}

/// Render a compact operator-readable SRE report.
#[must_use]
pub fn render_human(report: &SreReport) -> String {
    format!(
        "SRE status: {overall}\nworkflow: {workflow:?}\nrehydration: {rehydration:?} (p95 {p95:?} / {slo:.0}ms)\nprojection: {projection:?}\nsearch: {search:?}{search_detail}\ngates: {gates:?}{gate_detail}",
        overall = status_text(report),
        workflow = report.workflow.status,
        rehydration = report.rehydration.status,
        p95 = report.rehydration.p95_latency_ms.map(SreMeasurement::get),
        slo = report.rehydration.slo_ms.get(),
        projection = report.projection.status,
        search = report.search.status,
        search_detail = search_human_detail(&report.search),
        gates = report.gates.status,
        gate_detail = gate_human_detail(&report.gates),
    )
}

fn search_human_detail(search: &SreSearchSummary) -> String {
    let details: Vec<String> = search
        .modes
        .iter()
        .filter(|mode| mode.degraded > 0 || mode.failed > 0)
        .map(|mode| {
            let mut parts = Vec::new();
            if mode.failed > 0 {
                parts.push(format!("failed {}/{}", mode.failed, mode.invocations));
            }
            if mode.degraded > 0 {
                parts.push(format!("degraded {}/{}", mode.degraded, mode.invocations));
            }
            format!("{} {}", mode.mode, parts.join(" "))
        })
        .collect();
    if details.is_empty() {
        String::new()
    } else {
        format!(" ({})", details.join(", "))
    }
}

fn gate_human_detail(gates: &SreGateSummary) -> String {
    let details: Vec<&str> = gates
        .gates
        .iter()
        .filter(|gate| gate.status != SreStatus::Ok)
        .map(|gate| gate.name.as_str())
        .collect();
    if details.is_empty() {
        String::new()
    } else {
        format!(" ({})", details.join(", "))
    }
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
    if saw_warning {
        SreStatus::Warning
    } else if saw_unknown {
        SreStatus::Unknown
    } else {
        SreStatus::Ok
    }
}
