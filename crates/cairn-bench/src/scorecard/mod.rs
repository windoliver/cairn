//! Scorecard subcommand — `BrainBench` retrieval-quality runner.
//!
//! This module hosts the entrypoint previously in `src/main.rs`.

use std::path::PathBuf;

use clap::Args;

/// Arguments for the `scorecard` subcommand.
#[derive(Args, Debug)]
pub struct ScorecardArgs {
    /// Path to the fixture root (contains pages/, queries.json, upstream-baseline.json).
    #[arg(long, default_value = "fixtures/v0/brainbench-world-v1")]
    pub fixture: PathBuf,

    /// Output directory for report.md and per-query.jsonl.
    #[arg(long, default_value = "target/brainbench")]
    pub out_dir: PathBuf,

    /// Embedding cache file. Reused across runs; safe to delete.
    #[arg(long, default_value = "target/brainbench/embed-cache.bin")]
    pub cache: PathBuf,

    /// Skip the `OpenAI` columns even if the openai feature is compiled.
    #[arg(long)]
    pub skip_openai: bool,
}

/// Run the existing scorecard pipeline. Implementation is the body of the old `main`.
pub async fn run(args: ScorecardArgs) -> anyhow::Result<()> {
    crate::scorecard::runner::run(args).await
}

mod runner;
