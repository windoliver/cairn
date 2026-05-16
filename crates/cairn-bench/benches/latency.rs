//! Criterion latency benches for 8 hot-path cairn-sdk verbs (issue #99).
// `criterion_group!` generates a public fn without a doc comment; suppress
// the workspace `missing_docs` lint that would fire on the generated item.
#![allow(missing_docs)]
//!
//! Each bench seeds a [`SeededVault`] once outside the `b.iter` loop,
//! opens an [`Sdk`] client once, and reuses them across iterations.
//!
//! **What is measured:**
//! Verbs that the SDK has fully wired (`search`) dispatch into the `SQLite`
//! store and produce a real result. Verbs whose P0 dispatch is still
//! stubbed (`ingest`, `retrieve`, `capture_trace`, `forget`,
//! `assemble_hot`) return `SdkError::Unimplemented` or
//! `SdkError::CapabilityUnavailable` immediately after validation; the
//! bench still captures the wall-clock cost of the validation + error path,
//! which is the lower bound the gate will tighten once dispatch lands.
//!
//! **Thresholds** are enforced in Task 5 (`cairn-bench latency`). This file
//! is the measurement surface only.

use std::sync::Arc;
use std::time::Duration;

use cairn_bench::latency::vault::SeededVault;
use cairn_core::config::CairnConfig;
use cairn_core::generated::common::Ulid;
use cairn_core::generated::verbs::{
    assemble_hot::AssembleHotArgs,
    capture_trace::CaptureTraceArgs,
    forget::ForgetArgs,
    ingest::IngestArgs,
    retrieve::RetrieveArgs,
    search::{SearchArgs, SearchArgsMode},
};
use cairn_sdk::Sdk;
use criterion::{Criterion, criterion_group, criterion_main};

// ── helpers ─────────────────────────────────────────────────────────────────

/// Build a [`Sdk`] wired to the given seeded vault's store.
///
/// The `CairnConfig::default()` has `local_embeddings: true` and
/// `keyword_search: true`, which lets keyword search dispatch when the store
/// has FTS5. Semantic / hybrid also dispatch (`MockEmbedder` has vectors).
fn make_sdk(vault: &SeededVault) -> Sdk {
    let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
        Arc::clone(&vault.store) as Arc<dyn cairn_core::contract::memory_store::MemoryStore>;
    Sdk::with_store(store, CairnConfig::default())
}

/// Single-thread tokio runtime for driving async verb calls synchronously
/// inside criterion's iter closure.
fn make_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

// ── bench fns ───────────────────────────────────────────────────────────────

/// `assemble_hot` — always returns `Unimplemented` (vault coupling deferred
/// to #193). Measures validation + stub dispatch overhead.
fn bench_assemble_hot(c: &mut Criterion) {
    let vault = SeededVault::new().expect("seeded vault");
    let sdk = make_sdk(&vault);
    let args = AssembleHotArgs {
        budget: None,
        explain: None,
        recipe: None,
        session_id: None,
    };
    c.benchmark_group("latency")
        .bench_function("assemble_hot_p95", |b| {
            b.iter(|| {
                let _ = sdk.assemble_hot(&args);
            });
        });
}

/// `search` mode=keyword — wired path dispatches into FTS5.
fn bench_search_keyword(c: &mut Criterion) {
    let vault = SeededVault::new().expect("seeded vault");
    let sdk = make_sdk(&vault);
    let rt = make_rt();
    let args = SearchArgs {
        citations: None,
        cursor: None,
        explain: None,
        filters: None,
        include_reasoning: None,
        limit: Some(10),
        mode: SearchArgsMode::Keyword,
        query: "Rust memory safety async".to_owned(),
        scope: None,
    };
    c.benchmark_group("latency")
        .bench_function("search_keyword_p95", |b| {
            b.iter(|| {
                rt.block_on(sdk.search(&args))
            });
        });
}

/// `search` mode=semantic — wired path dispatches into vector index
/// (`MockEmbedder` produces deterministic vectors).
fn bench_search_semantic(c: &mut Criterion) {
    let vault = SeededVault::new().expect("seeded vault");
    let sdk = make_sdk(&vault);
    let rt = make_rt();
    let args = SearchArgs {
        citations: None,
        cursor: None,
        explain: None,
        filters: None,
        include_reasoning: None,
        limit: Some(10),
        mode: SearchArgsMode::Semantic,
        query: "agent hot memory assembly".to_owned(),
        scope: None,
    };
    c.benchmark_group("latency")
        .bench_function("search_semantic_p95", |b| {
            b.iter(|| {
                rt.block_on(sdk.search(&args))
            });
        });
}

/// `search` mode=hybrid — wired path dispatches both FTS5 and vector legs.
fn bench_search_hybrid(c: &mut Criterion) {
    let vault = SeededVault::new().expect("seeded vault");
    let sdk = make_sdk(&vault);
    let rt = make_rt();
    let args = SearchArgs {
        citations: None,
        cursor: None,
        explain: None,
        filters: None,
        include_reasoning: None,
        limit: Some(10),
        mode: SearchArgsMode::Hybrid,
        query: "SQLite FTS5 latency gates".to_owned(),
        scope: None,
    };
    c.benchmark_group("latency")
        .bench_function("search_hybrid_p95", |b| {
            b.iter(|| {
                rt.block_on(sdk.search(&args))
            });
        });
}

/// `retrieve` by record id — returns `CapabilityUnavailable` (wiring flag
/// `RETRIEVE_RECORD_WIRED = false`). Measures capability-check overhead.
fn bench_retrieve(c: &mut Criterion) {
    let vault = SeededVault::new().expect("seeded vault");
    let sdk = make_sdk(&vault);
    let record_id = Ulid(SeededVault::first_record_id().to_owned());
    let args = RetrieveArgs::Record { id: record_id };
    c.benchmark_group("latency")
        .bench_function("retrieve_p95", |b| {
            b.iter(|| {
                let _ = sdk.retrieve(&args);
            });
        });
}

/// `capture_trace` with `from` — returns `Unimplemented` after validation.
/// Using `from = "/dev/null"` (a valid non-empty path string; file I/O is
/// not reached because the verb is stubbed before dispatch).
fn bench_capture_trace(c: &mut Criterion) {
    let vault = SeededVault::new().expect("seeded vault");
    let sdk = make_sdk(&vault);
    let args = CaptureTraceArgs {
        blocks: None,
        from: Some("/dev/null".to_owned()),
        session_id: None,
    };
    c.benchmark_group("latency")
        .bench_function("capture_trace_p95", |b| {
            b.iter(|| {
                let _ = sdk.capture_trace(&args);
            });
        });
}

/// `ingest` with body — returns `Unimplemented` after validation.
/// Exercises the full WAL-prepare path (brief §5.6); the stub returns
/// before the actual WAL write, so this measures validation overhead.
fn bench_wal_apply(c: &mut Criterion) {
    let vault = SeededVault::new().expect("seeded vault");
    let sdk = make_sdk(&vault);
    let args = IngestArgs {
        batch_size: None,
        body: Some("Rust is memory safe and fast".to_owned()),
        dry_run: None,
        exclude: None,
        file: None,
        folder: None,
        frontmatter: None,
        harness: None,
        human_review: None,
        include: None,
        jsonl: None,
        kind: "note".to_owned(),
        limit: None,
        mode: None,
        no_cache: None,
        no_diff: None,
        recording: None,
        recursive: None,
        session_id: None,
        session_id_from: None,
        tags: None,
        url: None,
    };
    c.benchmark_group("latency")
        .bench_function("wal_apply_p95", |b| {
            b.iter(|| {
                let _ = sdk.ingest(&args);
            });
        });
}

/// `forget` record — returns `CapabilityUnavailable` (filtered by SDK;
/// `CairnMcpV1ForgetRecord` is explicitly removed in `advertised_capabilities`).
/// Measures the capability-gate overhead on the hot rejection path.
///
/// Note: the plan called this bench `capture_workflow_enqueue`. The SDK has
/// no Stop-event variant on `capture_trace` (the IDL uses `from` vs `blocks`,
/// not event kind); `forget` covers the second latency measurement point.
fn bench_capture_workflow_enqueue(c: &mut Criterion) {
    let vault = SeededVault::new().expect("seeded vault");
    let sdk = make_sdk(&vault);
    let record_id = Ulid(SeededVault::first_record_id().to_owned());
    let args = ForgetArgs::Record {
        dry_run: None,
        human_review: None,
        no_diff: None,
        record_id,
    };
    c.benchmark_group("latency")
        .bench_function("capture_workflow_enqueue_p95", |b| {
            b.iter(|| {
                let _ = sdk.forget(&args);
            });
        });
}

// ── criterion wiring ─────────────────────────────────────────────────────────

criterion_group! {
    name = latency_benches;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(8))
        .sample_size(50)
        .warm_up_time(Duration::from_secs(1));
    targets =
        bench_assemble_hot,
        bench_search_keyword,
        bench_search_semantic,
        bench_search_hybrid,
        bench_retrieve,
        bench_capture_trace,
        bench_wal_apply,
        bench_capture_workflow_enqueue,
}

criterion_main!(latency_benches);
