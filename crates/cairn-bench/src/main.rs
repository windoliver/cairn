#![forbid(unsafe_code)]
//! cairn-bench binary entry point.
//!
//! Loads the world-v1 corpus + queries + upstream baseline, runs the four
//! cairn adapters (`bm25-only`, `vector-bge`, `hybrid-bge-rrf`,
//! `hybrid-openai-rrf`), and emits a markdown report + per-query JSONL.
//!
//! Adapter dispatch lands in Task 11; report writing lands in Task 12
//! (stub today). The embedding cache is loaded once at startup and
//! flushed before exit so re-runs against the same fixture skip the
//! inference / HTTP cost — full threading into adapter calls is a
//! follow-up.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use cairn_bench::adapter::{Adapter, Bm25Adapter, HybridAdapter, VectorAdapter, ingest_pages};
use cairn_bench::cache::EmbeddingCache;
use cairn_bench::fixture::{Fixture, Query};
use cairn_bench::metrics::{PerQueryMetrics, compute};
use cairn_bench::report::{AdapterQueryRun, AdapterResults};
use cairn_core::config::EmbeddingModelKind;
use cairn_embeddings_local::{EmbeddingModel, ModelCache};
use cairn_store_sqlite::{
    open_in_memory, open_in_memory_with_embedder, open_in_memory_with_embedder_and_config,
};
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

    // Cache bookends. Threading the cache through the embedder calls
    // themselves is deferred; loading + saving here gives us a stable
    // on-disk artifact and exercises the format end-to-end.
    let cache = EmbeddingCache::load(&args.cache).context("load embed cache")?;
    println!("embed cache: {} entries", cache.len());

    let mut all_runs: Vec<AdapterResults> = Vec::new();

    all_runs.push(run_bm25_adapter(&fixture).await?);
    all_runs.push(run_vector_bge_adapter(&fixture).await?);
    all_runs.push(run_hybrid_bge_adapter(&fixture).await?);

    #[cfg(feature = "openai")]
    {
        if !args.skip_openai && std::env::var("OPENAI_API_KEY").is_ok() {
            all_runs.push(run_hybrid_openai_adapter(&fixture).await?);
        } else {
            all_runs.push(skipped(
                "hybrid-openai-rrf",
                "OPENAI_API_KEY not set or --skip-openai",
            ));
        }
    }
    #[cfg(not(feature = "openai"))]
    {
        let _ = args.skip_openai; // referenced under the openai-feature path only
        all_runs.push(skipped(
            "hybrid-openai-rrf",
            "feature `openai` not compiled",
        ));
    }

    cache.save(&args.cache).context("save embed cache")?;

    cairn_bench::report::write_report(&args.out_dir, &fixture, &all_runs)
        .context("write report")?;
    Ok(())
}

fn skipped(name: &str, why: &str) -> AdapterResults {
    eprintln!("skipping adapter `{name}`: {why}");
    (name.to_owned(), Vec::new())
}

async fn run_bm25_adapter(fixture: &Fixture) -> anyhow::Result<AdapterResults> {
    let store = open_in_memory().await?;
    let id_to_slug = ingest_pages(&store, &fixture.pages).await?;
    let adapter = Bm25Adapter {
        store: &store,
        id_to_slug: &id_to_slug,
    };
    run_adapter(&adapter, &fixture.queries).await
}

async fn run_vector_bge_adapter(fixture: &Fixture) -> anyhow::Result<AdapterResults> {
    let kind = EmbeddingModelKind::BgeSmallEnV1_5;
    let cache_for_fetch = ModelCache::new(std::path::Path::new(".cairn/models"));
    let _report = tokio::task::spawn_blocking(move || cache_for_fetch.fetch(kind)).await??;
    let cache_for_load = ModelCache::new(std::path::Path::new(".cairn/models"));
    let embedder: Arc<dyn EmbeddingModel> =
        tokio::task::spawn_blocking(move || cache_for_load.ensure(kind)).await??;
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder))).await?;
    let id_to_slug = ingest_pages(&store, &fixture.pages).await?;
    let adapter = VectorAdapter {
        store: &store,
        id_to_slug: &id_to_slug,
        model_label: kind.as_str().to_owned(),
        adapter_name: "vector-bge".to_owned(),
    };
    run_adapter(&adapter, &fixture.queries).await
}

async fn run_hybrid_bge_adapter(fixture: &Fixture) -> anyhow::Result<AdapterResults> {
    let kind = EmbeddingModelKind::BgeSmallEnV1_5;
    let cache_for_fetch = ModelCache::new(std::path::Path::new(".cairn/models"));
    let _report = tokio::task::spawn_blocking(move || cache_for_fetch.fetch(kind)).await??;
    let cache_for_load = ModelCache::new(std::path::Path::new(".cairn/models"));
    let embedder: Arc<dyn EmbeddingModel> =
        tokio::task::spawn_blocking(move || cache_for_load.ensure(kind)).await??;
    let store = open_in_memory_with_embedder_and_config(
        Some(Arc::clone(&embedder)),
        [10.0, 10.0, 5.0, 1.0],
    )
    .await?;
    let id_to_slug = ingest_pages(&store, &fixture.pages).await?;
    let adapter = HybridAdapter {
        store: &store,
        id_to_slug: &id_to_slug,
        model_label: kind.as_str().to_owned(),
        adapter_name: "hybrid-bge-rrf".to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
    };
    run_adapter(&adapter, &fixture.queries).await
}

#[cfg(feature = "openai")]
async fn run_hybrid_openai_adapter(fixture: &Fixture) -> anyhow::Result<AdapterResults> {
    use cairn_embeddings_openai::OpenAiEmbedder;
    let kind = EmbeddingModelKind::OpenAiTextEmbedding3Large;
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(OpenAiEmbedder::from_env(kind)?);
    let store = open_in_memory_with_embedder_and_config(
        Some(Arc::clone(&embedder)),
        [10.0, 10.0, 5.0, 1.0],
    )
    .await?;
    let id_to_slug = ingest_pages(&store, &fixture.pages).await?;
    let adapter = HybridAdapter {
        store: &store,
        id_to_slug: &id_to_slug,
        model_label: kind.as_str().to_owned(),
        adapter_name: "hybrid-openai-rrf".to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
    };
    run_adapter(&adapter, &fixture.queries).await
}

async fn run_adapter<A: Adapter + ?Sized>(
    adapter: &A,
    queries: &[Query],
) -> anyhow::Result<AdapterResults> {
    use std::collections::BTreeSet;
    let mut runs: Vec<AdapterQueryRun> = Vec::with_capacity(queries.len());
    for q in queries {
        let hits = adapter.run_query(q).await?;
        let rel: BTreeSet<String> = q.relevant.iter().cloned().collect();
        let m: PerQueryMetrics = compute(&hits, &rel, &q.grades);
        runs.push((q.id.clone(), hits, m));
    }
    Ok((adapter.name().to_owned(), runs))
}
