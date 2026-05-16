#![forbid(unsafe_code)]
//! cairn-bench binary entry point — subcommand dispatcher.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "cairn-bench", about = "Cairn bench harness: scorecard + release gates.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// `BrainBench` retrieval-quality scorecard runner.
    Scorecard(cairn_bench::scorecard::ScorecardArgs),
    /// Latency regression gate (placeholder until Task 4).
    Latency,
    /// Memory budget gate (placeholder until Task 6).
    Memory,
    /// Privacy leakage gate (placeholder until Task 8).
    Privacy,
    /// Run latency + memory + privacy and exit non-zero on any failure.
    All {
        /// Skip one or more gates by name (latency, memory, privacy).
        #[arg(long)]
        skip: Vec<String>,
    },
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Scorecard(args) => cairn_bench::scorecard::run(args).await,
        Cmd::Latency | Cmd::Memory | Cmd::Privacy | Cmd::All { .. } => {
            anyhow::bail!("not implemented yet — wired in later tasks")
        }
    }
}
