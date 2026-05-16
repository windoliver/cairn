//! Privacy gate subcommand.

pub mod fixture;

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

    /// Output dir.
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

/// Run the privacy gate. Currently supports `--check` only; the runner lands in Task 8.
///
/// # Errors
/// Returns an error if the fixtures directory cannot be read or YAML is malformed.
pub fn run(args: &PrivacyArgs) -> anyhow::Result<GateOutcome> {
    let fixtures = fixture::load_dir(&args.fixtures_dir)?;
    if args.check {
        println!("privacy gate --check: parsed {} fixtures", fixtures.len());
        return Ok(GateOutcome::Pass);
    }
    anyhow::bail!("privacy runner is not yet wired — Task 8");
}
