//! Brief §15 latency benches — 8 hot-path verbs, driven via `cairn` CLI subprocess.
//!
//! See `docs/superpowers/specs/2026-05-15-issue-99-release-gates-design.md` §4.1
//! for the rationale: the `cairn-sdk` Rust API returns `Unimplemented` for
//! `assemble_hot`, `retrieve --record`, `capture_trace`, `forget`, and `lint`
//! until issue #193 wires those verbs end-to-end. Until then, subprocess
//! wall-clock is the measurable proxy. Each bench seeds its own [`SeededVault`]
//! once outside the `b.iter` loop so vault creation cost is amortised.
//!
//! Per-bench CLI invocations (settled after inspecting generated verbs.rs):
//!
//! | bench | command |
//! |---|---|
//! | `assemble_hot_p95`    | `cairn assemble_hot --json` |
//! | `search_keyword_p95`  | `cairn search Lorem --mode keyword --json` |
//! | `search_semantic_p95` | `cairn search Lorem --mode semantic --json` (CAIRN_MOCK_EMBEDDER=1) |
//! | `search_hybrid_p95`   | `cairn search Lorem --mode hybrid --json` (CAIRN_MOCK_EMBEDDER=1) |
//! | `retrieve_p95`        | `cairn retrieve <id> --json` (positional id from first ingest) |
//! | `capture_trace_p95`   | `cairn capture_trace --from <trace.jsonl> --json` |
//! | `wal_apply_p95`       | `cairn ingest --kind reference --body bench-wal --json` |
//! | `workflow_enqueue_p95`| `cairn capture_trace --from <enqueue.jsonl> --json` (Stop event) |

// Bench targets are private dev tooling — public-API doc coverage is not
// required here. The criterion_group! macro generates an undocumented public
// item that would otherwise trigger `missing_docs`.
#![allow(missing_docs)]

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use cairn_bench::latency::vault::SeededVault;
use criterion::{Criterion, criterion_group, criterion_main};

/// Resolve the `cairn` binary path.
///
/// The path is captured at build time via `build.rs` (which reads
/// `CARGO_BIN_EXE_cairn` from the Cargo build-script environment) and
/// embedded as the `CAIRN_BIN_PATH` compile-time env var.  When the embedded
/// path is empty (standalone `rustc` invocations outside `cargo bench`) the
/// bench falls back to searching `$PATH` so it also works against an installed
/// release build.
fn cairn_bin() -> PathBuf {
    let embedded = env!("CAIRN_BIN_PATH");
    if embedded.is_empty() {
        PathBuf::from("cairn")
    } else {
        PathBuf::from(embedded)
    }
}

fn cli(vault: &std::path::Path) -> Command {
    let mut cmd = Command::new(cairn_bin());
    cmd.current_dir(vault);
    // Remove registry env so the bench subprocess uses the vault CWD.
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd
}

/// Spawn a single CLI invocation with captured stdout/stderr.
/// Returns `Ok(())` on exit 0, otherwise an `io::Error` carrying stderr.
fn run_cli(args: &[&str], vault: &std::path::Path) -> std::io::Result<()> {
    let out = cli(vault).args(args).output()?;
    if !out.status.success() {
        eprintln!(
            "cli {args:?} failed in {}:\nstderr: {}",
            vault.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        return Err(std::io::Error::other(format!(
            "cli {args:?} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

// ── bench functions ───────────────────────────────────────────────────────────

fn bench_assemble_hot(c: &mut Criterion) {
    let v = SeededVault::new(cairn_bin()).expect("seed vault for assemble_hot");
    c.bench_function("assemble_hot_p95", |b| {
        b.iter(|| run_cli(&["assemble_hot", "--json"], &v.path).expect("assemble_hot"));
    });
}

fn bench_search_keyword(c: &mut Criterion) {
    let v = SeededVault::new(cairn_bin()).expect("seed vault for search_keyword");
    c.bench_function("search_keyword_p95", |b| {
        b.iter(|| {
            // query is positional per generated verbs.rs: arg("query").required(true)
            run_cli(&["search", "Lorem", "--mode", "keyword", "--json"], &v.path)
                .expect("search keyword");
        });
    });
}

fn bench_search_semantic(c: &mut Criterion) {
    let v = SeededVault::new(cairn_bin()).expect("seed vault for search_semantic");
    c.bench_function("search_semantic_p95", |b| {
        b.iter(|| {
            // CAIRN_MOCK_EMBEDDER=1 avoids needing real model weights on disk
            // (matches the pattern in crates/cairn-cli/tests/search_modes_golden.rs).
            let out = cli(&v.path)
                .env("CAIRN_MOCK_EMBEDDER", "1")
                .args(["search", "Lorem", "--mode", "semantic", "--json"])
                .output()
                .expect("run cli for search semantic");
            if !out.status.success() {
                eprintln!(
                    "search semantic failed:\nstderr: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                panic!("search semantic failed");
            }
        });
    });
}

fn bench_search_hybrid(c: &mut Criterion) {
    let v = SeededVault::new(cairn_bin()).expect("seed vault for search_hybrid");
    c.bench_function("search_hybrid_p95", |b| {
        b.iter(|| {
            let out = cli(&v.path)
                .env("CAIRN_MOCK_EMBEDDER", "1")
                .args(["search", "Lorem", "--mode", "hybrid", "--json"])
                .output()
                .expect("run cli for search hybrid");
            if !out.status.success() {
                eprintln!(
                    "search hybrid failed:\nstderr: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                panic!("search hybrid failed");
            }
        });
    });
}

fn bench_retrieve(c: &mut Criterion) {
    let v = SeededVault::new(cairn_bin()).expect("seed vault for retrieve");
    let id = v.first_record_id().to_owned();
    c.bench_function("retrieve_p95", |b| {
        // id is positional per generated verbs.rs: arg("id").required(false)
        // grouped in retrieve_target with session_id / path / scope / profile.
        b.iter(|| run_cli(&["retrieve", &id, "--json"], &v.path).expect("retrieve record"));
    });
}

fn bench_capture_trace(c: &mut Criterion) {
    let v = SeededVault::new(cairn_bin()).expect("seed vault for capture_trace");
    // Write the trace JSONL once outside the iter loop.
    let trace_path = v
        .write_trace_jsonl("bench_capture")
        .expect("write trace jsonl");
    let trace_str = trace_path.to_string_lossy().into_owned();
    c.bench_function("capture_trace_p95", |b| {
        b.iter(|| {
            run_cli(
                &["capture_trace", "--from", &trace_str, "--json"],
                &v.path,
            )
            .expect("capture_trace");
        });
    });
}

fn bench_wal_apply(c: &mut Criterion) {
    let v = SeededVault::new(cairn_bin()).expect("seed vault for wal_apply");
    c.bench_function("wal_apply_p95", |b| {
        b.iter(|| {
            run_cli(
                &["ingest", "--kind", "reference", "--body", "bench-wal", "--json"],
                &v.path,
            )
            .expect("ingest (wal_apply)");
        });
    });
}

/// Bench the Stop-event path of `capture_trace`, which triggers the
/// rolling-summary workflow enqueue. Uses a separate session id so each
/// bench invocation processes a fresh (session, turn) group.
///
/// Note: Criterion re-runs this function many times, so successive calls will
/// attempt to import the same `(session_id, turn_id)` pair. The handler is
/// idempotent for duplicate turn imports (WAL dedup), so this does not fail —
/// it just measures the deduplicated-fast-path after the first sample.
fn bench_workflow_enqueue(c: &mut Criterion) {
    let v =
        SeededVault::new(cairn_bin()).expect("seed vault for workflow_enqueue");
    let enqueue_path = v
        .write_enqueue_trace_jsonl()
        .expect("write enqueue trace jsonl");
    let enqueue_str = enqueue_path.to_string_lossy().into_owned();
    c.bench_function("workflow_enqueue_p95", |b| {
        b.iter(|| {
            run_cli(
                &["capture_trace", "--from", &enqueue_str, "--json"],
                &v.path,
            )
            .expect("workflow_enqueue (capture_trace Stop)");
        });
    });
}

// ── criterion group ───────────────────────────────────────────────────────────

criterion_group! {
    name = hot_path;
    config = Criterion::default()
        // 8 s measurement window per bench. Subprocess spawn dominates (~100-300 ms)
        // so each bench produces ~25-80 samples in this window. The § 15 "2% regression"
        // rule is unit-independent; absolute SLOs are checked by `cairn-bench latency`.
        .measurement_time(Duration::from_secs(8))
        // 20 samples (not criterion's default 100) — subprocess spawn is slow.
        .sample_size(20)
        .warm_up_time(Duration::from_secs(2));
    targets =
        bench_assemble_hot,
        bench_search_keyword,
        bench_search_semantic,
        bench_search_hybrid,
        bench_retrieve,
        bench_capture_trace,
        bench_wal_apply,
        bench_workflow_enqueue
}

criterion_main!(hot_path);
