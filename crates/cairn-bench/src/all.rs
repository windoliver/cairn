//! `all` subcommand — runs latency + memory + privacy in order; worst exit code wins.

use crate::gates::report::GateOutcome;
use crate::latency::LatencyArgs;
use crate::memory::MemoryArgs;
use crate::privacy::PrivacyArgs;

/// Arguments for the `all` subcommand.
#[derive(Debug, Default)]
pub struct AllArgs {
    /// Gates to skip (latency, memory, privacy).
    pub skip: Vec<String>,
}

/// Run latency → memory → privacy gates. Returns the worst outcome.
///
/// # Errors
/// Returns the first error from any gate's infrastructure (cargo bench failure,
/// missing manifest, etc.). Per-metric / per-fixture failures are not errors;
/// they surface as `GateOutcome::Fail`.
pub fn run(args: &AllArgs) -> anyhow::Result<GateOutcome> {
    let mut worst = GateOutcome::Pass;

    if !args.skip.iter().any(|s| s == "latency") {
        println!("== latency gate ==");
        let outcome = crate::latency::run(&LatencyArgs::default_for_ci())?;
        worst = worst.worst_of(outcome);
    }
    if !args.skip.iter().any(|s| s == "memory") {
        println!("== memory gate ==");
        let outcome = crate::memory::run(&MemoryArgs::default_for_ci())?;
        worst = worst.worst_of(outcome);
    }
    if !args.skip.iter().any(|s| s == "privacy") {
        println!("== privacy gate ==");
        let outcome = crate::privacy::run(&PrivacyArgs::default_for_ci())?;
        worst = worst.worst_of(outcome);
    }
    Ok(worst)
}
