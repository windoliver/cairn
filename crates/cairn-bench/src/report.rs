//! `BrainBench` report writer.
//!
//! Task 11 ships the stub so the binary compiles; Task 12 fills in the
//! markdown table + per-query JSONL emission. The signature is fixed
//! here so callers don't move when the body lands.

use std::path::Path;

use anyhow::Result;

use crate::fixture::Fixture;
use crate::metrics::PerQueryMetrics;

/// One run for one query: `(query_id, ranked_hits, metrics)`. The triple
/// stays inline because per-query JSONL is the most natural form to hand
/// the writer in Task 12.
pub type AdapterQueryRun = (String, Vec<String>, PerQueryMetrics);

/// All runs for one adapter: `(adapter_name, per_query_runs)`. Empty
/// `per_query_runs` is the convention used by `skipped(...)` in the
/// binary to mark adapters that were not executed for this run.
pub type AdapterResults = (String, Vec<AdapterQueryRun>);

/// Write the `BrainBench` report under `out_dir`.
///
/// Stub for Task 11 — Task 12 implements the markdown scorecard and
/// per-query JSONL artifacts. The signature is the contract callers in
/// `main.rs` already depend on.
///
/// # Errors
///
/// Currently infallible; Task 12 will surface I/O errors from the table
/// renderer and JSONL writer.
#[allow(
    clippy::missing_const_for_fn,
    reason = "stub will gain non-const I/O in Task 12"
)]
pub fn write_report(
    _out_dir: &Path,
    _fixture: &Fixture,
    _all_runs: &[AdapterResults],
) -> Result<()> {
    Ok(())
}
