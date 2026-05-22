//! SRE release gate subcommand.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use cairn_core::domain::{
    SreDetail, SreGateResult, SreMeasurement, SrePrivacySummary, SreProjectionSummary,
    SreProjectionTargetSummary, SreRehydrationSummary, SreSearchModeSummary, SreSearchSummary,
    SreStatus, SreWorkflowKindSummary, SreWorkflowSummary, classify_threshold,
};
use clap::Args;
use serde::{Deserialize, Serialize};

use crate::gates::report::GateOutcome;
use crate::gates::thresholds::{SLO_COLD_REHYDRATE_MS, SLO_MIGRATION_BACKLOG_MS};

const SCHEMA_VERSION: u32 = 1;
const COLD_REHYDRATE_BENCH: &str = "cold_rehydrate_p95";
const FIXTURE_WORKFLOW_REFERENCE_NOW_MS: i64 = 1_800_000;
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

    /// `SQLite` workflow database to use for migration backlog gates.
    #[arg(long)]
    pub workflow_db: Option<PathBuf>,
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
            workflow_db: None,
        }
    }
}

/// Run the SRE release gate.
///
/// # Errors
/// Returns an error if report serialization or filesystem writes fail.
pub fn run(args: &SreArgs) -> anyhow::Result<GateOutcome> {
    std::fs::create_dir_all(&args.out_dir).context("create SRE output dir")?;
    let workflow_measurement = workflow_measurement(args)?;
    let cold_measurement = if args.fixtures_only {
        ColdMeasurement::FixturesOnly
    } else {
        if !args.no_run {
            run_lifecycle_criterion(&args.criterion_dir)?;
        }
        match load_cold_rehydrate_p95(&args.criterion_dir)? {
            Some(measured_ms) => ColdMeasurement::Measured(measured_ms),
            None => ColdMeasurement::MissingInput,
        }
    };
    let mut report = fixture_report(cold_measurement, workflow_measurement);
    let forbidden = forbidden_fragment_count(&serde_json::to_string(&report)?);
    set_privacy_check(&mut report, forbidden);

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

#[derive(Debug, Clone, Copy)]
struct WorkflowMeasurement {
    queued: i64,
    held_lease: Option<i64>,
    last_success: Option<i64>,
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

fn workflow_measurement(args: &SreArgs) -> anyhow::Result<WorkflowMeasurement> {
    let (workflow_db, reference_now_ms) = if let Some(path) = &args.workflow_db {
        (path.clone(), current_epoch_ms()?)
    } else {
        let path = args.out_dir.join("workflow-fixture.sqlite");
        write_fixture_workflow_db(&path)?;
        (path, FIXTURE_WORKFLOW_REFERENCE_NOW_MS)
    };
    read_workflow_measurement(&workflow_db, reference_now_ms)
        .with_context(|| format!("read workflow fixture {}", workflow_db.display()))
}

fn write_fixture_workflow_db(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create workflow fixture dir")?;
    }
    if path.exists() {
        std::fs::remove_file(path).context("remove stale workflow fixture")?;
    }
    let conn = rusqlite::Connection::open(path).context("open workflow fixture")?;
    create_workflow_jobs_table(&conn)?;
    insert_workflow_job(
        &conn,
        WorkflowFixtureRow {
            job_id: "queued-migration",
            kind: "expire.tier",
            state: "queued",
            next_run_at: FIXTURE_WORKFLOW_REFERENCE_NOW_MS - FIXTURE_WORKFLOW_AGE_MS_I64,
            updated_at: FIXTURE_WORKFLOW_REFERENCE_NOW_MS,
            lease_started: None,
            lease_expires_at: None,
            completed_at_ms: None,
        },
    )?;
    insert_workflow_job(
        &conn,
        WorkflowFixtureRow {
            job_id: "leased-migration",
            kind: "expire.tier",
            state: "leased",
            next_run_at: FIXTURE_WORKFLOW_REFERENCE_NOW_MS,
            updated_at: FIXTURE_WORKFLOW_REFERENCE_NOW_MS,
            lease_started: Some(1),
            lease_expires_at: Some(
                FIXTURE_WORKFLOW_REFERENCE_NOW_MS - FIXTURE_LONGEST_HELD_LEASE_MS,
            ),
            completed_at_ms: None,
        },
    )?;
    insert_workflow_job(
        &conn,
        WorkflowFixtureRow {
            job_id: "done-migration",
            kind: "expire.tier",
            state: "done",
            next_run_at: FIXTURE_WORKFLOW_REFERENCE_NOW_MS,
            updated_at: FIXTURE_WORKFLOW_REFERENCE_NOW_MS,
            lease_started: None,
            lease_expires_at: None,
            completed_at_ms: Some(FIXTURE_WORKFLOW_REFERENCE_NOW_MS - FIXTURE_LAST_SUCCESS_AGE_MS),
        },
    )?;
    Ok(())
}

fn read_workflow_measurement(
    path: &Path,
    reference_now_ms: i64,
) -> anyhow::Result<WorkflowMeasurement> {
    let conn = rusqlite::Connection::open(path).context("open workflow database")?;
    let oldest_next_run_at = conn
        .query_row(
            "SELECT MIN(next_run_at) FROM workflow_jobs WHERE state = 'queued'",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .context("query queued workflow age")?;
    let longest_held_lease_ms = conn
        .query_row(
            "SELECT MIN(lease_expires_at) FROM workflow_jobs \
             WHERE state = 'leased' AND lease_expires_at <= ?1",
            [reference_now_ms],
            |row| row.get::<_, Option<i64>>(0),
        )
        .context("query leased workflow age")?
        .map(|lease_expires_at| reference_now_ms.saturating_sub(lease_expires_at).max(0));
    let last_completed_at = conn
        .query_row(
            "SELECT MAX(completed_at_ms) FROM workflow_jobs WHERE completed_at_ms IS NOT NULL",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .context("query workflow last success")?;
    Ok(WorkflowMeasurement {
        queued: oldest_next_run_at.map_or(0, |next_run_at| {
            reference_now_ms.saturating_sub(next_run_at).max(0)
        }),
        held_lease: longest_held_lease_ms,
        last_success: last_completed_at
            .map(|completed_at| reference_now_ms.saturating_sub(completed_at).max(0)),
    })
}

fn current_epoch_ms() -> anyhow::Result<i64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before unix epoch")?
        .as_millis();
    i64::try_from(millis).context("epoch milliseconds exceed i64")
}

fn create_workflow_jobs_table(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE workflow_jobs (
            job_id TEXT NOT NULL PRIMARY KEY,
            kind TEXT NOT NULL,
            payload BLOB NOT NULL,
            state TEXT NOT NULL,
            attempts INTEGER NOT NULL,
            delivery_count INTEGER NOT NULL,
            max_attempts INTEGER NOT NULL,
            base_backoff_ms INTEGER NOT NULL,
            backoff_multiplier INTEGER NOT NULL,
            max_backoff_ms INTEGER NOT NULL,
            next_run_at INTEGER NOT NULL,
            enqueued_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            lease_owner TEXT,
            lease_nonce TEXT,
            lease_started INTEGER,
            lease_expires_at INTEGER,
            failure_class TEXT,
            dead_letter_at_ms INTEGER,
            completed_at_ms INTEGER,
            last_error TEXT
        );",
    )
    .context("create workflow_jobs fixture table")
}

#[derive(Clone, Copy)]
struct WorkflowFixtureRow {
    job_id: &'static str,
    kind: &'static str,
    state: &'static str,
    next_run_at: i64,
    updated_at: i64,
    lease_started: Option<i64>,
    lease_expires_at: Option<i64>,
    completed_at_ms: Option<i64>,
}

fn insert_workflow_job(conn: &rusqlite::Connection, row: WorkflowFixtureRow) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO workflow_jobs (
            job_id, kind, payload, state, attempts, delivery_count, max_attempts,
            base_backoff_ms, backoff_multiplier, max_backoff_ms, next_run_at,
            enqueued_at, updated_at, lease_started, lease_expires_at, completed_at_ms
        ) VALUES (?1, ?2, x'', ?3, 0, 0, 3, 1, 2, 60000, ?4, ?5, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            row.job_id,
            row.kind,
            row.state,
            row.next_run_at,
            row.updated_at,
            row.lease_started,
            row.lease_expires_at,
            row.completed_at_ms,
        ],
    )
    .context("insert workflow fixture row")?;
    Ok(())
}

fn fixture_report(
    cold_measurement: ColdMeasurement,
    workflow_measurement: WorkflowMeasurement,
) -> BenchSreReport {
    let oldest_queued_age_ms = measurement_i64(workflow_measurement.queued);
    let migration_status = classify_threshold(Some(oldest_queued_age_ms), SLO_MIGRATION_BACKLOG_MS);
    let migration_backlog = gate(
        "migration_backlog",
        migration_status,
        Some(oldest_queued_age_ms),
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
        workflow: workflow_dashboard(migration_status, workflow_measurement),
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

fn workflow_dashboard(status: SreStatus, measurement: WorkflowMeasurement) -> SreWorkflowSummary {
    SreWorkflowSummary {
        status,
        oldest_queued_age_ms: Some(measurement.queued),
        longest_held_lease_ms: measurement.held_lease,
        dead_letter_count: 0,
        kinds: vec![SreWorkflowKindSummary {
            kind: "expire.tier".to_owned(),
            queued: 1,
            leased: 0,
            done_recent: 3,
            failed_recent: 0,
            oldest_queued_age_ms: Some(measurement.queued),
            last_success_age_ms: measurement.last_success,
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

fn measurement_i64(value: i64) -> f64 {
    value.to_string().parse().expect("i64 parses as f64")
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
    let sample_path = criterion_sample_path(criterion_dir);
    if !sample_path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&sample_path)
        .with_context(|| format!("read criterion sample {}", sample_path.display()))?;
    let sample: CriterionSample = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse criterion sample {}", sample_path.display()))?;
    sample.p95_ms(&sample_path)
}

fn criterion_sample_path(criterion_dir: &Path) -> PathBuf {
    criterion_dir
        .join(COLD_REHYDRATE_BENCH)
        .join("new")
        .join("sample.json")
}

#[derive(Debug, Deserialize)]
struct CriterionSample {
    times: Vec<f64>,
    iters: Vec<f64>,
}

impl CriterionSample {
    fn p95_ms(&self, sample_path: &Path) -> anyhow::Result<Option<f64>> {
        if self.times.is_empty() {
            return Ok(None);
        }
        anyhow::ensure!(
            self.times.len() == self.iters.len(),
            "criterion sample times/iters length mismatch in {}",
            sample_path.display()
        );
        let mut per_iter_ms = Vec::with_capacity(self.times.len());
        for (time_ns, iters) in self.times.iter().zip(&self.iters) {
            anyhow::ensure!(
                time_ns.is_finite() && *time_ns >= 0.0,
                "invalid criterion sample time in {}",
                sample_path.display()
            );
            anyhow::ensure!(
                iters.is_finite() && *iters > 0.0,
                "invalid criterion sample iteration count in {}",
                sample_path.display()
            );
            per_iter_ms.push(time_ns / iters / 1_000_000.0);
        }
        per_iter_ms.sort_by(f64::total_cmp);
        let idx = (per_iter_ms.len() - 1)
            .saturating_mul(95)
            .saturating_add(50)
            / 100;
        Ok(per_iter_ms.get(idx).copied())
    }
}

fn run_lifecycle_criterion(criterion_dir: &Path) -> anyhow::Result<()> {
    let build_status = Command::new("cargo")
        .args([
            "build",
            "-p",
            "cairn-cli",
            "--bin",
            "cairn",
            "--release",
            "--locked",
        ])
        .status()
        .context("spawn release cairn build")?;
    anyhow::ensure!(build_status.success(), "release cairn binary build failed");

    let cargo_bench_output = cargo_bench_criterion_dir();
    let fallback_bench_dir = cargo_bench_output.join(COLD_REHYDRATE_BENCH);
    if fallback_bench_dir.exists() {
        std::fs::remove_dir_all(&fallback_bench_dir).with_context(|| {
            format!(
                "remove stale lifecycle criterion output {}",
                fallback_bench_dir.display()
            )
        })?;
    }
    let requested_bench_dir = criterion_dir.join(COLD_REHYDRATE_BENCH);
    if requested_bench_dir.exists() {
        std::fs::remove_dir_all(&requested_bench_dir).with_context(|| {
            format!(
                "remove stale requested criterion output {}",
                requested_bench_dir.display()
            )
        })?;
    }

    let status = Command::new("cargo")
        .env("CRITERION_HOME", criterion_dir)
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
    import_cargo_bench_criterion_output(criterion_dir, &cargo_bench_output)?;
    Ok(())
}

fn cargo_bench_criterion_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("criterion")
}

fn import_cargo_bench_criterion_output(
    criterion_dir: &Path,
    cargo_bench_output: &Path,
) -> anyhow::Result<()> {
    if criterion_sample_path(criterion_dir).exists() {
        return Ok(());
    }

    let source = cargo_bench_output.join(COLD_REHYDRATE_BENCH);
    if !source.join("new").join("sample.json").exists() {
        return Ok(());
    }

    let destination = criterion_dir.join(COLD_REHYDRATE_BENCH);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)
            .with_context(|| format!("replace criterion output {}", destination.display()))?;
    }
    copy_dir_all(&source, &destination)
}

fn copy_dir_all(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)
        .with_context(|| format!("create criterion output {}", destination.display()))?;
    for entry in std::fs::read_dir(source)
        .with_context(|| format!("read criterion output {}", source.display()))?
    {
        let entry = entry.with_context(|| format!("read criterion output {}", source.display()))?;
        let file_type = entry.file_type().with_context(|| {
            format!("inspect criterion output entry {}", entry.path().display())
        })?;
        let next_destination = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &next_destination)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &next_destination).with_context(|| {
                format!(
                    "copy criterion output {} to {}",
                    entry.path().display(),
                    next_destination.display()
                )
            })?;
        }
    }
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
