//! Privacy gate subcommand.

pub mod fixture;
pub mod harness;

use std::path::PathBuf;

use clap::Args;

use crate::gates::report::GateOutcome;

/// Arguments for the `privacy` subcommand.
#[derive(Args, Debug)]
pub struct PrivacyArgs {
    /// Path to the fixtures directory.
    #[arg(long, default_value = "crates/cairn-bench/fixtures/privacy")]
    pub fixtures_dir: PathBuf,

    /// Parse fixtures without running them; used in CI for fast schema validation.
    #[arg(long)]
    pub check: bool,

    /// Output dir for `privacy.json`.
    #[arg(long, default_value = "target/cairn-bench")]
    pub out_dir: PathBuf,
}

impl PrivacyArgs {
    /// Construct args with CI-appropriate defaults.
    #[must_use]
    pub fn default_for_ci() -> Self {
        Self {
            fixtures_dir: "crates/cairn-bench/fixtures/privacy".into(),
            check: false,
            out_dir: "target/cairn-bench".into(),
        }
    }
}

/// Run the privacy gate.
///
/// # Errors
/// Returns an error if a fixture is malformed, the vault bootstrap fails, or
/// subprocess invocation fails. Assertion failures are NOT errors — they
/// appear in the report and trigger [`GateOutcome::Fail`].
pub fn run(args: &PrivacyArgs) -> anyhow::Result<GateOutcome> {
    let fixtures = fixture::load_dir(&args.fixtures_dir)?;
    if args.check {
        println!("privacy gate --check: parsed {} fixtures", fixtures.len());
        return Ok(GateOutcome::Pass);
    }

    let mut total_failures = Vec::new();
    let mut passed = 0_usize;
    let mut deferred = 0_usize;
    for (path, f) in &fixtures {
        if f.deferred {
            deferred += 1;
            println!("[DEFERRED] {} — {}", path.display(), f.scenario);
            continue;
        }
        let failures = harness::run_fixture(f)?;
        if failures.is_empty() {
            passed += 1;
            println!("[OK] {}", path.display());
        } else {
            println!("[FAIL] {} ({} failures)", path.display(), failures.len());
            for fail in &failures {
                println!(
                    "    [{}] {} expected={} actual={}",
                    fail.surface, fail.query_or_id, fail.expected, fail.actual
                );
            }
            total_failures.extend(failures);
        }
    }

    let report = harness::PrivacyReport {
        schema_version: 1,
        fixtures_run: fixtures.len(),
        fixtures_passed: passed,
        fixtures_deferred: deferred,
        ok: total_failures.is_empty(),
        failures: total_failures,
    };

    std::fs::create_dir_all(&args.out_dir)?;
    std::fs::write(
        args.out_dir.join("privacy.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;

    Ok(if report.ok {
        GateOutcome::Pass
    } else {
        GateOutcome::Fail
    })
}
