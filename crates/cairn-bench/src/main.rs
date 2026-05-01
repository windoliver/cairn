#![forbid(unsafe_code)]
//! cairn-bench binary entry point.
//!
//! Loads the world-v1 corpus + queries + upstream baseline, runs the four
//! cairn adapters, and emits a markdown report + per-query JSONL.
//!
//! At scaffold time (Task 10) the binary only loads the fixture and
//! prints sizes; adapter dispatch and report writing land in Tasks 11–12.

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "cairn-bench", about = "BrainBench retrieval scorecard runner.")]
struct Cli {
    /// Path to the fixture root (contains pages/, queries.json, upstream-baseline.json).
    #[arg(long, default_value = "fixtures/v0/brainbench-world-v1")]
    fixture: PathBuf,

    /// Output directory for report.md and per-query.jsonl.
    #[arg(long, default_value = "target/brainbench")]
    out_dir: PathBuf,

    /// Embedding cache file. Reused across runs; safe to delete.
    #[arg(long, default_value = "target/brainbench/embed-cache.bin")]
    cache: PathBuf,

    /// Skip the `OpenAI` columns even if the openai feature is compiled.
    #[arg(long)]
    skip_openai: bool,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    std::fs::create_dir_all(&args.out_dir).context("create out_dir")?;
    let fixture = cairn_bench::fixture::load(&args.fixture).context("load fixture")?;
    println!(
        "loaded fixture: {} pages, {} queries (from {})",
        fixture.pages.len(),
        fixture.queries.len(),
        args.fixture.display()
    );
    // Cache + skip_openai are wired in Task 11; reference them here so
    // unused-variable lints stay quiet without a leading underscore.
    let _ = (&args.cache, args.skip_openai);

    // Adapters + report wiring lands in Tasks 11–12.
    Ok(())
}
