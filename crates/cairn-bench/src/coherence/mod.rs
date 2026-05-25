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

impl GateError {
    /// Map an error to its sysexits-style process exit code.
    ///
    /// - `78` (`EX_CONFIG`) — bad manifest, baseline, or cassette content.
    /// - `1` — runtime error (trend I/O, score invariant, etc.).
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Replay(_)
            | Self::Threshold(_)
            | Self::BaselineIo { .. }
            | Self::BaselineJson { .. } => 78,
            Self::Score(_) | Self::Trend(_) => 1,
        }
    }
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

use clap::{Args, ValueEnum};

/// CLI args block for the `coherence` subcommand.
#[derive(Debug, Args)]
pub struct CoherenceArgs {
    /// Subcommand selector.
    #[command(subcommand)]
    pub cmd: CoherenceCmd,
}

/// `cairn-bench coherence <subcommand>` variants.
#[derive(Debug, clap::Subcommand)]
pub enum CoherenceCmd {
    /// Run the coherence gate over the configured cassettes.
    Run(CoherenceRunArgs),
}

/// Arguments for `cairn-bench coherence run`.
#[derive(Debug, Args)]
pub struct CoherenceRunArgs {
    /// Gate mode.
    #[arg(long, value_enum, default_value_t = CliGate::Beta)]
    pub gate: CliGate,
    /// Cassettes directory.
    #[arg(long, default_value = "fixtures/v0/replay")]
    pub cassettes: PathBuf,
    /// Cassettes to include (default: extended #136 cassettes).
    #[arg(long = "include", num_args = 1.., default_values_t = default_includes())]
    pub include: Vec<String>,
    /// Threshold manifest path.
    #[arg(long, default_value = "crates/cairn-bench/manifests/coherence.toml")]
    pub manifest: PathBuf,
    /// Baseline path.
    #[arg(long, default_value = "crates/cairn-bench/baselines/coherence.json")]
    pub baseline: PathBuf,
    /// Trend file path.
    #[arg(
        long,
        default_value = "crates/cairn-bench/baselines/coherence-trend.jsonl"
    )]
    pub trend: PathBuf,
    /// Overwrite the baseline with this run's scores.
    #[arg(long)]
    pub update_baseline: bool,
    /// Skip appending to the trend file.
    #[arg(long)]
    pub no_trend_write: bool,
    /// Machine-readable output on stdout.
    #[arg(long)]
    pub json: bool,
}

/// `clap` value-enum for `--gate`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliGate {
    /// Beta gate (uses `beta_min`).
    Beta,
    /// Release-candidate gate (uses `rc_min`).
    Rc,
    /// Record only, never fail.
    None,
}

impl From<CliGate> for GateMode {
    fn from(v: CliGate) -> Self {
        match v {
            CliGate::Beta => Self::Beta,
            CliGate::Rc => Self::Rc,
            CliGate::None => Self::None,
        }
    }
}

fn default_includes() -> Vec<String> {
    vec![
        "research_domain".to_owned(),
        "engineering_domain".to_owned(),
        "support_domain".to_owned(),
    ]
}

/// Dispatch the `coherence` subcommand. Returns the process exit code.
///
/// Maps orchestrator errors to their sysexits-style exit codes
/// (see [`GateError::exit_code`]) and writes a one-line diagnostic to
/// stderr instead of bubbling the error up to `main`, which would print
/// the chain and exit with code 1.
pub async fn dispatch(args: CoherenceArgs) -> u8 {
    match args.cmd {
        CoherenceCmd::Run(run) => dispatch_run(run).await,
    }
}

async fn dispatch_run(run: CoherenceRunArgs) -> u8 {
    let trend_path = run.trend.clone();
    let json = run.json;
    let opts = GateOptions {
        mode: run.gate.into(),
        cassettes_dir: run.cassettes,
        include: run.include,
        manifest_path: run.manifest,
        baseline_path: Some(run.baseline),
        trend_path: run.trend,
        update_baseline: run.update_baseline,
        write_trend: !run.no_trend_write,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: std::env::var("GIT_SHA").unwrap_or_else(|_| "unknown".to_owned()),
        now: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        run_id: ulid_like(),
    };
    let run_id = opts.run_id.clone();
    let outcome = match run_coherence_gate(opts).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("coherence: {e}");
            return e.exit_code();
        }
    };
    if json {
        let value = render_json(&outcome.report, &trend_path.display().to_string(), &run_id);
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        print!("{}", outcome.human);
    }
    if outcome.gate_passed { 0 } else { 69 }
}

fn ulid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("run-{nanos:032x}")
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
    let body =
        serde_json::to_string_pretty(baseline).map_err(|source| GateError::BaselineJson {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_maps_config_errors_to_78() {
        let cases = [
            GateError::Threshold(ThresholdError::UnsupportedManifestVersion { version: 99 }),
            GateError::BaselineIo {
                path: "p".to_owned(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            },
            GateError::BaselineJson {
                path: "p".to_owned(),
                source: serde_json::from_str::<serde_json::Value>("not json").unwrap_err(),
            },
            GateError::Replay(cairn_test_fixtures::replay::ReplayError::InvalidManifest(
                "bad".to_owned(),
            )),
        ];
        for err in cases {
            assert_eq!(err.exit_code(), 78, "{err:?} should map to 78");
        }
    }

    #[test]
    fn exit_code_maps_runtime_errors_to_1() {
        let cases = [
            GateError::Score(ScoreError::LengthMismatch {
                actions: 1,
                reports: 0,
            }),
            GateError::Trend(TrendError::MissingSchemaVersion {
                path: "p".to_owned(),
                line: 1,
            }),
        ];
        for err in cases {
            assert_eq!(err.exit_code(), 1, "{err:?} should map to 1");
        }
    }
}
