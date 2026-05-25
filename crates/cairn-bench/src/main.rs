#![forbid(unsafe_code)]
//! cairn-bench binary entry point — subcommand dispatcher.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "cairn-bench",
    about = "Cairn bench harness: scorecard + release gates."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// `BrainBench` retrieval-quality scorecard runner.
    Scorecard(cairn_bench::scorecard::ScorecardArgs),
    /// Latency regression gate: runs criterion, compares to baseline, writes report.
    Latency(cairn_bench::latency::LatencyArgs),
    /// Memory budget gate: measures binary + embedding model against manifest budget.
    Memory(cairn_bench::memory::MemoryArgs),
    /// Privacy leakage gate: parse fixtures and optionally run them.
    Privacy(cairn_bench::privacy::PrivacyArgs),
    /// SRE release gate: writes `sre.json` for `cairn admin sre report` import.
    Sre(cairn_bench::sre::SreArgs),
    /// Coherence release gate: scores extended cassettes against the threshold manifest.
    Coherence(cairn_bench::coherence::CoherenceArgs),
    /// Run latency + memory + privacy + SRE and exit non-zero on any failure.
    All {
        /// Skip one or more gates by name (latency, memory, privacy, sre).
        #[arg(long)]
        skip: Vec<String>,
    },
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Scorecard(args) => cairn_bench::scorecard::run(args).await,
        Cmd::Latency(args) => {
            let outcome = cairn_bench::latency::run(&args)?;
            std::process::exit(outcome.exit_code().into());
        }
        Cmd::Memory(args) => {
            let outcome = cairn_bench::memory::run(&args)?;
            std::process::exit(outcome.exit_code().into());
        }
        Cmd::Privacy(args) => {
            let outcome = cairn_bench::privacy::run(&args)?;
            std::process::exit(outcome.exit_code().into());
        }
        Cmd::Sre(args) => {
            let outcome = cairn_bench::sre::run(&args)?;
            std::process::exit(outcome.exit_code().into());
        }
        Cmd::Coherence(args) => {
            let code = cairn_bench::coherence::dispatch(args).await;
            std::process::exit(code.into());
        }
        Cmd::All { skip } => {
            let outcome = cairn_bench::all::run(&cairn_bench::all::AllArgs { skip })?;
            std::process::exit(outcome.exit_code().into());
        }
    }
}
