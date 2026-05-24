//! Coherence release-gate (issue #137).
//!
//! See `docs/design/2026-05-24-coherence-benchmarks-design.md`.
//! Public API will be wired up in a later task.

pub mod category;
pub mod report;
pub mod score;
pub mod threshold;
pub mod trend;

pub use category::{ALL as ALL_CATEGORIES, DisplayCategory, MetricCategory, as_str};
pub use report::{GateReport, build as build_report, render_human, render_json};
pub use score::{CategoryScore, CategoryScores, ScoreError, aggregate, classify};
pub use threshold::{
    Baseline, CategoryThreshold, GateMode, MetricOutcome, MetricResult, ThresholdError,
    ThresholdManifest, all_pass, evaluate, load_manifest,
};
pub use trend::{TrendEntry, TrendError, append as append_trend, load as load_trend};

use std::path::PathBuf;

/// Knobs for one gate run. Constructed by the CLI wrapper in `main.rs`.
#[derive(Debug, Clone)]
pub struct GateOptions {
    /// Gate mode.
    pub mode: GateMode,
    /// Directory containing cassette JSON files.
    pub cassettes_dir: PathBuf,
    /// Cassette IDs to load from `cassettes_dir` (without `.json` extension).
    pub include: Vec<String>,
    /// Path to the threshold manifest TOML.
    pub manifest_path: PathBuf,
    /// Path to the committed baseline JSON. `None` skips the delta check.
    pub baseline_path: Option<PathBuf>,
    /// Path to the append-only trend JSONL.
    pub trend_path: PathBuf,
    /// Rewrite the baseline with this run's scores.
    pub update_baseline: bool,
    /// Append a line to the trend file.
    pub write_trend: bool,
    /// Cairn version string for the trend record.
    pub cairn_version: String,
    /// Git SHA for the trend record.
    pub git_sha: String,
    /// RFC-3339 timestamp for the trend record (`captured_at`).
    pub now: String,
    /// Stable run identifier for the trend record.
    pub run_id: String,
}

/// What `run_coherence_gate` returns. The caller maps `gate_passed` to a
/// process exit code.
#[derive(Debug, Clone)]
pub struct GateOutcome {
    /// `true` iff every metric passed (or `--gate none`).
    pub gate_passed: bool,
    /// Compact report (also serialised under `--json`).
    pub report: GateReport,
    /// Pre-rendered human-readable table.
    pub human: String,
}

/// Errors surfaced from the orchestrator.
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    /// Cassette replay failure.
    #[error(transparent)]
    Replay(#[from] cairn_test_fixtures::replay::ReplayError),
    /// Score aggregation failure.
    #[error(transparent)]
    Score(#[from] ScoreError),
    /// Threshold manifest load failure.
    #[error(transparent)]
    Threshold(#[from] ThresholdError),
    /// Trend file load or append failure.
    #[error(transparent)]
    Trend(#[from] TrendError),
    /// Baseline filesystem read or write failure.
    #[error("baseline {path}: {source}")]
    BaselineIo {
        /// Path that failed.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Baseline JSON parse or serialise failure.
    #[error("baseline {path}: {source}")]
    BaselineJson {
        /// Path that failed.
        path: String,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
}

/// Drive every included cassette through the replay engine, score per
/// category, evaluate the gate, render the report, optionally write the
/// trend line, and optionally rewrite the baseline.
///
/// # Errors
/// Returns any error from replay, scoring, threshold loading, or trend I/O.
pub async fn run_coherence_gate(opts: GateOptions) -> Result<GateOutcome, GateError> {
    let manifest = load_manifest(&opts.manifest_path)?;
    let baseline = match &opts.baseline_path {
        Some(p) if p.exists() => Some(load_baseline(p)?),
        _ => None,
    };

    let mut all_actions: Vec<cairn_test_fixtures::replay::ReplayAction> = Vec::new();
    let mut all_reports: Vec<cairn_test_fixtures::replay::ReplayCheckReport> = Vec::new();
    for cassette in &opts.include {
        let path = opts.cassettes_dir.join(format!("{cassette}.json"));
        let scenario = cairn_test_fixtures::replay::load_scenario_file(&path)?;
        let report = cairn_test_fixtures::replay::run_scenario(&scenario).await?;
        all_actions.extend(scenario.actions);
        all_reports.extend(report.checks);
    }
    let cassettes = u32::try_from(opts.include.len()).unwrap_or(u32::MAX);
    let actions = u32::try_from(all_actions.len()).unwrap_or(u32::MAX);

    let scores = aggregate(&all_actions, &all_reports)?;
    let results = evaluate(opts.mode, &scores, &manifest, baseline.as_ref());
    let gate_passed = all_pass(&results);
    let report = build_report(opts.mode, &results, cassettes, actions);
    let human = render_human(&report, &results);

    if opts.write_trend {
        let entry = TrendEntry {
            schema_version: 1,
            run_id: opts.run_id.clone(),
            ts: opts.now.clone(),
            cairn_version: opts.cairn_version.clone(),
            git_sha: opts.git_sha.clone(),
            gate: report.gate.to_owned(),
            outcome: report.outcome.to_owned(),
            failures: report.failures.iter().map(|s| (*s).to_owned()).collect(),
            metrics: report.metrics.clone(),
        };
        append_trend(&opts.trend_path, &entry)?;
    }

    if opts.update_baseline
        && let Some(path) = &opts.baseline_path
    {
        write_baseline(
            path,
            &Baseline {
                schema_version: 1,
                captured_at: opts.now.clone(),
                cairn_version: opts.cairn_version.clone(),
                git_sha: opts.git_sha.clone(),
                metrics: report.metrics.clone(),
            },
        )?;
    }

    Ok(GateOutcome {
        gate_passed,
        report,
        human,
    })
}

fn load_baseline(path: &std::path::Path) -> Result<Baseline, GateError> {
    let raw = std::fs::read_to_string(path).map_err(|source| GateError::BaselineIo {
        path: path.display().to_string(),
        source,
    })?;
    serde_json::from_str(&raw).map_err(|source| GateError::BaselineJson {
        path: path.display().to_string(),
        source,
    })
}

fn write_baseline(path: &std::path::Path, baseline: &Baseline) -> Result<(), GateError> {
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(baseline).map_err(|source| GateError::BaselineJson {
        path: path.display().to_string(),
        source,
    })?;
    std::fs::write(&tmp, body).map_err(|source| GateError::BaselineIo {
        path: tmp.display().to_string(),
        source,
    })?;
    std::fs::rename(&tmp, path).map_err(|source| GateError::BaselineIo {
        path: path.display().to_string(),
        source,
    })?;
    Ok(())
}
