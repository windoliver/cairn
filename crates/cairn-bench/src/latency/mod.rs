//! Latency gate subcommand and shared bench helpers.

pub mod harness;
pub mod vault;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use chrono::Utc;
use clap::Args;

use crate::gates::baseline::{Baseline, BaselineError};
use crate::gates::report::GateOutcome;

/// Arguments for the `latency` subcommand.
#[derive(Args, Debug)]
pub struct LatencyArgs {
    /// Path to the committed baseline directory (defaults to crate-local `baselines/`).
    #[arg(long, default_value = "crates/cairn-bench/baselines")]
    pub baselines_dir: PathBuf,

    /// Where to write `target/cairn-bench/latency.json`.
    #[arg(long, default_value = "target/cairn-bench")]
    pub out_dir: PathBuf,

    /// Path to criterion's output dir.
    #[arg(long, default_value = "target/criterion")]
    pub criterion_dir: PathBuf,

    /// Skip running the bench and reuse the existing criterion output.
    #[arg(long)]
    pub no_run: bool,

    /// Run the benches and overwrite the baseline file for the current runner.
    #[arg(long)]
    pub refresh_baseline: bool,
}

impl LatencyArgs {
    /// Construct args with CI-appropriate defaults.
    #[must_use]
    pub fn default_for_ci() -> Self {
        Self {
            baselines_dir: "crates/cairn-bench/baselines".into(),
            out_dir: "target/cairn-bench".into(),
            criterion_dir: "target/criterion".into(),
            no_run: false,
            refresh_baseline: false,
        }
    }
}

/// Run the latency gate.
///
/// # Errors
/// Returns an error if criterion fails, JSON parsing fails, or the baseline file is unreadable.
pub fn run(args: &LatencyArgs) -> anyhow::Result<GateOutcome> {
    let profile = crate::gates::runner_profile();
    let baseline_path = args.baselines_dir.join(format!("latency.{profile}.json"));

    if !args.no_run {
        run_criterion()?;
    }

    let measured = harness::parse_criterion_dir(&args.criterion_dir)?;

    if args.refresh_baseline {
        let commit = current_commit().unwrap_or_else(|_| "unknown".into());
        let now = Utc::now().to_rfc3339();
        // Ensure the baselines directory exists.
        std::fs::create_dir_all(&args.baselines_dir)?;
        let new = Baseline {
            schema_version: 1,
            runner: profile.into(),
            captured_at: now,
            commit,
            metrics: measured.into_iter().collect(),
            regression_pct: BTreeMap::default(),
        };
        new.save(&baseline_path)?;
        println!("wrote refreshed baseline to {}", baseline_path.display());
        return Ok(GateOutcome::Pass);
    }

    let baseline = match Baseline::load(&baseline_path) {
        Ok(b) => b,
        Err(BaselineError::NotFound(_)) => {
            eprintln!(
                "baseline missing: {} — run `cairn-bench latency --refresh-baseline`",
                baseline_path.display()
            );
            return Ok(GateOutcome::MissingInput);
        }
        Err(e) => return Err(e.into()),
    };

    let report = harness::compare(&measured, &baseline);

    std::fs::create_dir_all(&args.out_dir)?;
    let out = args.out_dir.join("latency.json");
    std::fs::write(&out, serde_json::to_vec_pretty(&report)?)?;
    print_human_summary(&report);
    Ok(harness::outcome_from_report(&report))
}

fn run_criterion() -> anyhow::Result<()> {
    // Inherit env so `CAIRN_MOCK_EMBEDDER` etc. propagate.
    let status = Command::new("cargo")
        .args([
            "bench",
            "-p",
            "cairn-bench",
            "--bench",
            "latency",
            "--locked",
        ])
        .status()?;
    anyhow::ensure!(status.success(), "criterion run failed");
    Ok(())
}

fn current_commit() -> anyhow::Result<String> {
    let out = Command::new("git").args(["rev-parse", "HEAD"]).output()?;
    anyhow::ensure!(out.status.success(), "git rev-parse failed");
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

fn print_human_summary(r: &harness::LatencyReport) {
    println!("latency gate: {}", if r.ok { "PASS" } else { "FAIL" });
    for m in &r.metrics {
        let mark = if m.slo_ok && m.regression_ok {
            "OK"
        } else {
            "FAIL"
        };
        println!(
            "  [{mark}] {bench}: {meas:.2} ms (SLO {slo:.0} ms, baseline {base:.2} ms, +2% = {thr:.2} ms)",
            bench = m.bench,
            meas = m.measured_ms,
            slo = m.slo_ms,
            base = m.baseline_ms,
            thr = m.regression_threshold_ms,
        );
    }
    if !r.failures.is_empty() {
        println!("failures:");
        for f in &r.failures {
            println!("  - {f}");
        }
    }
}
