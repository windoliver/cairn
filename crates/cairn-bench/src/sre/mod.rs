//! SRE release gate subcommand.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use cairn_core::domain::{
    SreDetail, SreGateResult, SreMeasurement, SrePrivacySummary, SreProjectionSummary,
    SreProjectionTargetSummary, SreRehydrationSummary, SreSearchModeSummary, SreSearchSummary,
    SreStatus, SreWorkflowKindSummary, SreWorkflowSummary, classify_threshold,
};
use clap::Args;
use serde::Serialize;

use crate::gates::report::GateOutcome;
use crate::gates::thresholds::{SLO_COLD_REHYDRATE_MS, SLO_MIGRATION_BACKLOG_MS};

const SCHEMA_VERSION: u32 = 1;
const COLD_REHYDRATE_BENCH: &str = "cold_rehydrate_p95";
const FIXTURE_WORKFLOW_AGE_MS_F64: f64 = 120_000.0;
const FIXTURE_WORKFLOW_AGE_MS_I64: i64 = 120_000;
const FIXTURE_LONGEST_HELD_LEASE_MS: i64 = 30_000;
const FIXTURE_LAST_SUCCESS_AGE_MS: i64 = 60_000;
const FIXTURE_MIGRATION_BACKLOG_THRESHOLD_MS: i64 = 600_000;
const FIXTURE_COLD_REHYDRATE_P95_MS: f64 = 2_250.0;
const FIXTURE_LATEST_REHYDRATE_MS: u64 = 2_200;
const FIXTURE_REHYDRATE_SAMPLES: u64 = 5;
const FIXTURE_PROJECTION_STALE_FAILED: f64 = 0.0;
const SEEDED_FORBIDDEN_DETAIL: &str = "SECRET_PRIVATE_TOKEN /Users/alice private body query text";
const FORBIDDEN_FRAGMENTS: &[&str] = &[
    "SECRET_PRIVATE_TOKEN",
    "/Users/alice",
    "private body",
    "query text",
];

/// Arguments for the `sre` subcommand.
#[derive(Args, Debug)]
pub struct SreArgs {
    /// Output directory for `sre.json`.
    #[arg(long, default_value = "target/cairn-bench")]
    pub out_dir: PathBuf,

    /// Write only deterministic SRE fixtures; intended for CI smoke coverage.
    #[arg(long)]
    pub fixtures_only: bool,

    /// Path to criterion's output dir for lifecycle measurements.
    #[arg(long, default_value = "target/criterion")]
    pub criterion_dir: PathBuf,

    /// Skip running the lifecycle bench and reuse the existing criterion output.
    #[arg(long)]
    pub no_run: bool,

    /// Also refresh `sre.baseline.json` with the generated SRE fixture report.
    #[arg(long)]
    pub refresh_baseline: bool,
}

impl SreArgs {
    /// Construct args with CI-appropriate defaults.
    #[must_use]
    pub fn default_for_ci() -> Self {
        Self {
            out_dir: "target/cairn-bench".into(),
            fixtures_only: true,
            criterion_dir: "target/criterion".into(),
            no_run: false,
            refresh_baseline: false,
        }
    }
}

/// Run the SRE release gate.
///
/// # Errors
/// Returns an error if report serialization or filesystem writes fail.
pub fn run(args: &SreArgs) -> anyhow::Result<GateOutcome> {
    let cold_measurement = if args.fixtures_only {
        ColdMeasurement::FixturesOnly
    } else {
        if !args.no_run {
            run_lifecycle_criterion()?;
        }
        match load_cold_rehydrate_p95(&args.criterion_dir)? {
            Some(measured_ms) => ColdMeasurement::Measured(measured_ms),
            None => ColdMeasurement::MissingInput,
        }
    };
    let mut report = fixture_report(cold_measurement);
    let forbidden = forbidden_fragment_count(&serde_json::to_string(&report)?);
    set_privacy_check(&mut report, forbidden);

    std::fs::create_dir_all(&args.out_dir).context("create SRE output dir")?;
    let encoded = serde_json::to_vec_pretty(&report)?;
    std::fs::write(args.out_dir.join("sre.json"), &encoded).context("write sre.json")?;
    if args.refresh_baseline {
        std::fs::write(args.out_dir.join("sre.baseline.json"), encoded)
            .context("write sre.baseline.json")?;
    }

    print_human_summary(&report);
    Ok(
        if matches!(cold_measurement, ColdMeasurement::MissingInput) {
            GateOutcome::MissingInput
        } else if report.ok {
            GateOutcome::Pass
        } else {
            GateOutcome::Fail
        },
    )
}

#[derive(Debug, Clone, Copy)]
enum ColdMeasurement {
    FixturesOnly,
    MissingInput,
    Measured(f64),
}

#[derive(Debug, Clone, Serialize)]
struct BenchSreReport {
    schema_version: u32,
    ok: bool,
    checks: Vec<SreGateResult>,
    dashboard: BenchSreDashboard,
}

#[derive(Debug, Clone, Serialize)]
struct BenchSreDashboard {
    workflow: SreWorkflowSummary,
    rehydration: SreRehydrationSummary,
    projection: SreProjectionSummary,
    search: SreSearchSummary,
    privacy: SrePrivacySummary,
}

fn fixture_report(cold_measurement: ColdMeasurement) -> BenchSreReport {
    let migration_status =
        classify_threshold(Some(FIXTURE_WORKFLOW_AGE_MS_F64), SLO_MIGRATION_BACKLOG_MS);
    let migration_backlog = gate(
        "migration_backlog",
        migration_status,
        Some(FIXTURE_WORKFLOW_AGE_MS_F64),
        SLO_MIGRATION_BACKLOG_MS,
        "ms",
        fixture_detail(),
    );
    let projection_lag = gate(
        "projection_lag_fixture",
        classify_threshold(Some(FIXTURE_PROJECTION_STALE_FAILED), 0.0),
        Some(FIXTURE_PROJECTION_STALE_FAILED),
        0.0,
        "count",
        fixture_detail(),
    );
    let cold_rehydrate = cold_rehydrate_gate(cold_measurement);

    let mut checks = vec![migration_backlog.clone(), projection_lag.clone()];
    if let Some(cold_rehydrate) = cold_rehydrate.clone() {
        checks.push(cold_rehydrate.clone());
    }
    checks.push(privacy_check(0, SreStatus::Ok));

    let dashboard = BenchSreDashboard {
        workflow: workflow_dashboard(migration_status),
        rehydration: rehydration_dashboard(cold_measurement, cold_rehydrate),
        projection: projection_dashboard(projection_lag.status),
        search: search_dashboard(),
        privacy: SrePrivacySummary {
            scrubbed: true,
            forbidden_field_count: 0,
        },
    };

    BenchSreReport {
        schema_version: SCHEMA_VERSION,
        ok: checks_pass(&checks),
        checks,
        dashboard,
    }
}

fn workflow_dashboard(status: SreStatus) -> SreWorkflowSummary {
    SreWorkflowSummary {
        status,
        oldest_queued_age_ms: Some(FIXTURE_WORKFLOW_AGE_MS_I64),
        longest_held_lease_ms: Some(FIXTURE_LONGEST_HELD_LEASE_MS),
        dead_letter_count: 0,
        kinds: vec![SreWorkflowKindSummary {
            kind: "migration".to_owned(),
            queued: 1,
            leased: 0,
            done_recent: 3,
            failed_recent: 0,
            oldest_queued_age_ms: Some(FIXTURE_WORKFLOW_AGE_MS_I64),
            last_success_age_ms: Some(FIXTURE_LAST_SUCCESS_AGE_MS),
            backlog_threshold_ms: FIXTURE_MIGRATION_BACKLOG_THRESHOLD_MS,
            status,
        }],
    }
}

fn cold_rehydrate_gate(cold_measurement: ColdMeasurement) -> Option<SreGateResult> {
    match cold_measurement {
        ColdMeasurement::FixturesOnly => None,
        ColdMeasurement::MissingInput => Some(gate(
            COLD_REHYDRATE_BENCH,
            SreStatus::Unknown,
            None,
            SLO_COLD_REHYDRATE_MS,
            "ms",
            fixture_detail(),
        )),
        ColdMeasurement::Measured(measured_ms) => Some(gate(
            COLD_REHYDRATE_BENCH,
            classify_threshold(Some(measured_ms), SLO_COLD_REHYDRATE_MS),
            Some(measured_ms),
            SLO_COLD_REHYDRATE_MS,
            "ms",
            fixture_detail(),
        )),
    }
}

fn rehydration_dashboard(
    cold_measurement: ColdMeasurement,
    last_gate: Option<SreGateResult>,
) -> SreRehydrationSummary {
    match cold_measurement {
        ColdMeasurement::FixturesOnly => SreRehydrationSummary {
            status: classify_threshold(Some(FIXTURE_COLD_REHYDRATE_P95_MS), SLO_COLD_REHYDRATE_MS),
            latest_latency_ms: Some(FIXTURE_LATEST_REHYDRATE_MS),
            p95_latency_ms: Some(measurement(FIXTURE_COLD_REHYDRATE_P95_MS)),
            slo_ms: measurement(SLO_COLD_REHYDRATE_MS),
            sample_count: FIXTURE_REHYDRATE_SAMPLES,
            last_gate,
        },
        ColdMeasurement::MissingInput => SreRehydrationSummary {
            status: SreStatus::Unknown,
            latest_latency_ms: None,
            p95_latency_ms: None,
            slo_ms: measurement(SLO_COLD_REHYDRATE_MS),
            sample_count: 0,
            last_gate,
        },
        ColdMeasurement::Measured(measured_ms) => SreRehydrationSummary {
            status: classify_threshold(Some(measured_ms), SLO_COLD_REHYDRATE_MS),
            latest_latency_ms: None,
            p95_latency_ms: Some(measurement(measured_ms)),
            slo_ms: measurement(SLO_COLD_REHYDRATE_MS),
            sample_count: 1,
            last_gate,
        },
    }
}

fn projection_dashboard(status: SreStatus) -> SreProjectionSummary {
    SreProjectionSummary {
        status,
        nexus_state: "current".to_owned(),
        nexus_reason: Some("fixture".to_owned()),
        targets: vec![SreProjectionTargetSummary {
            target: "nexus_fixture".to_owned(),
            current: 12,
            stale: 0,
            failed: 0,
            missing: 0,
            max_lag_ms: Some(0),
            last_rebuild_latency_ms: Some(150),
            status,
        }],
    }
}

fn search_dashboard() -> SreSearchSummary {
    SreSearchSummary {
        status: SreStatus::Ok,
        modes: vec![SreSearchModeSummary {
            mode: "hybrid".to_owned(),
            advertised: true,
            invocations: 8,
            degraded: 0,
            failed: 0,
            p95_latency_ms: Some(measurement(22.0)),
            status: SreStatus::Ok,
        }],
    }
}

fn set_privacy_check(report: &mut BenchSreReport, forbidden_count: u32) {
    let status = if forbidden_count == 0 {
        SreStatus::Ok
    } else {
        SreStatus::Fail
    };
    let privacy = privacy_check(forbidden_count, status);
    if let Some(existing) = report
        .checks
        .iter_mut()
        .find(|check| check.name == "sre_privacy_scrub")
    {
        *existing = privacy;
    } else {
        report.checks.push(privacy);
    }
    report.dashboard.privacy = privacy_summary(forbidden_count);
    report.ok = checks_pass(&report.checks);
}

fn privacy_check(forbidden_count: u32, status: SreStatus) -> SreGateResult {
    gate(
        "sre_privacy_scrub",
        status,
        Some(f64::from(forbidden_count)),
        0.0,
        "forbidden_fields",
        Some(SreDetail::from_raw(SEEDED_FORBIDDEN_DETAIL)),
    )
}

fn gate(
    name: impl Into<String>,
    status: SreStatus,
    measured: Option<f64>,
    threshold: f64,
    unit: impl Into<String>,
    detail: Option<SreDetail>,
) -> SreGateResult {
    SreGateResult {
        name: name.into(),
        status,
        measured: measured.map(measurement),
        threshold: Some(measurement(threshold)),
        unit: unit.into(),
        detail,
    }
}

fn measurement(value: f64) -> SreMeasurement {
    SreMeasurement::new(value).expect("finite SRE fixture measurement")
}

fn fixture_detail() -> Option<SreDetail> {
    SreDetail::stable("fixture")
}

fn checks_pass(checks: &[SreGateResult]) -> bool {
    checks.iter().all(|check| check.status == SreStatus::Ok)
}

fn forbidden_fragment_count(serialized: &str) -> u32 {
    u32::try_from(
        FORBIDDEN_FRAGMENTS
            .iter()
            .filter(|fragment| serialized.contains(**fragment))
            .count(),
    )
    .expect("forbidden fragment count fits in u32")
}

fn load_cold_rehydrate_p95(criterion_dir: &Path) -> anyhow::Result<Option<f64>> {
    if !criterion_dir.is_dir() {
        return Ok(None);
    }
    let measured = crate::latency::harness::parse_criterion_dir(criterion_dir)?;
    Ok(measured.get(COLD_REHYDRATE_BENCH).copied())
}

fn run_lifecycle_criterion() -> anyhow::Result<()> {
    let status = Command::new("cargo")
        .args([
            "bench",
            "-p",
            "cairn-bench",
            "--bench",
            "lifecycle",
            "--locked",
        ])
        .status()?;
    anyhow::ensure!(status.success(), "criterion lifecycle run failed");
    Ok(())
}

fn privacy_summary(forbidden_field_count: u32) -> SrePrivacySummary {
    SrePrivacySummary {
        scrubbed: true,
        forbidden_field_count: u64::from(forbidden_field_count),
    }
}

fn print_human_summary(report: &BenchSreReport) {
    println!("sre gate: {}", if report.ok { "PASS" } else { "FAIL" });
    for check in &report.checks {
        println!(
            "  [{status:?}] {name}: {measured:.0} {unit} / {threshold:.0}",
            status = check.status,
            name = check.name,
            measured = check.measured.map(SreMeasurement::get).unwrap_or_default(),
            unit = check.unit,
            threshold = check.threshold.map(SreMeasurement::get).unwrap_or_default(),
        );
    }
}
