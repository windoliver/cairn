//! Inner implementation of the scorecard subcommand.
//!
//! Extracted from the old top-level `main.rs`; no semantic changes.

use std::sync::{Arc, Mutex};

use anyhow::Context;
use crate::adapter::{
    Adapter, Bm25Adapter, GraphHybridAdapter, HybridAdapter, VectorAdapter, extract_link_graph,
    ingest_pages,
};
// `ingest_text_for` is only consumed by the gated `openai` adapter path
// (`run_hybrid_openai_adapter`); importing it unconditionally would
// regress to the dead-code warning under the default feature set.
#[cfg(feature = "openai")]
use crate::adapter::ingest_text_for;
use crate::cache::EmbeddingCache;
use crate::fixture::{Fixture, Query};
use crate::metrics::{PerQueryMetrics, compute};
use crate::report::{AdapterQueryRun, AdapterResults};
use cairn_core::config::EmbeddingModelKind;
use cairn_embeddings_local::{EmbeddingModel, ModelCache};
use cairn_store_sqlite::{
    open_in_memory, open_in_memory_with_embedder, open_in_memory_with_embedder_and_config,
};

pub(super) async fn run(args: super::ScorecardArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.out_dir).context("create out_dir")?;
    let fixture = crate::fixture::load(&args.fixture).context("load fixture")?;
    println!(
        "loaded fixture: {} pages, {} queries (from {})",
        fixture.pages.len(),
        fixture.queries.len(),
        args.fixture.display()
    );

    // Embedding cache: shared across adapters, persisted between runs.
    // OpenAI's adapter pre-warms via the batch endpoint to dodge rate
    // limits; BGE adapters could populate it on a future pass.
    let cache = Arc::new(Mutex::new(
        EmbeddingCache::load(&args.cache).context("load embed cache")?,
    ));
    {
        let entries = cache
            .lock()
            .map_err(|e| anyhow::anyhow!("cache poisoned: {e}"))?
            .len();
        println!("embed cache: {entries} entries");
    }

    let mut all_runs: Vec<AdapterResults> = Vec::new();

    all_runs.push(run_bm25_adapter(&fixture).await?);
    all_runs.push(run_vector_bge_adapter(&fixture).await?);
    all_runs.push(run_hybrid_bge_adapter(&fixture).await?);
    all_runs.push(run_graph_hybrid_bge_adapter(&fixture).await?);

    #[cfg(feature = "openai")]
    {
        if !args.skip_openai && std::env::var("OPENAI_API_KEY").is_ok() {
            all_runs.push(run_hybrid_openai_adapter(&fixture, Arc::clone(&cache)).await?);
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

    {
        let cache = cache
            .lock()
            .map_err(|e| anyhow::anyhow!("cache poisoned: {e}"))?;
        cache.save(&args.cache).context("save embed cache")?;
    }

    crate::report::write_report(&args.out_dir, &fixture, &all_runs)
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
    if !cache_for_fetch.is_present(kind) {
        let _report = tokio::task::spawn_blocking(move || cache_for_fetch.fetch(kind)).await??;
    }
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
    if !cache_for_fetch.is_present(kind) {
        let _report = tokio::task::spawn_blocking(move || cache_for_fetch.fetch(kind)).await??;
    }
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

async fn run_graph_hybrid_bge_adapter(fixture: &Fixture) -> anyhow::Result<AdapterResults> {
    let kind = EmbeddingModelKind::BgeSmallEnV1_5;
    let cache_for_fetch = ModelCache::new(std::path::Path::new(".cairn/models"));
    if !cache_for_fetch.is_present(kind) {
        let _report = tokio::task::spawn_blocking(move || cache_for_fetch.fetch(kind)).await??;
    }
    let cache_for_load = ModelCache::new(std::path::Path::new(".cairn/models"));
    let embedder: Arc<dyn EmbeddingModel> =
        tokio::task::spawn_blocking(move || cache_for_load.ensure(kind)).await??;
    let store = open_in_memory_with_embedder_and_config(
        Some(Arc::clone(&embedder)),
        [10.0, 10.0, 5.0, 1.0],
    )
    .await?;
    let id_to_slug = ingest_pages(&store, &fixture.pages).await?;
    let (link_graph, title_index) = extract_link_graph(&fixture.pages);
    let adapter = GraphHybridAdapter {
        store: &store,
        id_to_slug: &id_to_slug,
        link_graph: &link_graph,
        title_index: &title_index,
        model_label: kind.as_str().to_owned(),
        adapter_name: "graph-hybrid-bge".to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
    };
    run_adapter(&adapter, &fixture.queries).await
}

#[cfg(feature = "openai")]
async fn run_hybrid_openai_adapter(
    fixture: &Fixture,
    cache: Arc<Mutex<EmbeddingCache>>,
) -> anyhow::Result<AdapterResults> {
    use crate::cached_embedder::CachedEmbedder;
    use cairn_embeddings_openai::OpenAiEmbedder;
    let kind = EmbeddingModelKind::OpenAiTextEmbedding3Large;
    // Keep both a concrete handle (for the batch endpoint) and a trait
    // object (for the cache wrapper). The vec0 table is now resized at
    // open time to match `embedder.dim()` (`crates/cairn-store-sqlite/
    // src/open.rs::resize_record_vectors`), so we use the model's
    // native 1536-dim output without MRL truncation.
    let oai_concrete = Arc::new(OpenAiEmbedder::from_env(kind)?);
    let raw: Arc<dyn EmbeddingModel> = Arc::clone(&oai_concrete) as Arc<dyn EmbeddingModel>;
    let cached = Arc::new(CachedEmbedder::new(
        Arc::clone(&raw),
        Arc::clone(&cache),
        kind.as_str().to_owned(),
    ));

    // Pre-warm via the batch endpoint to avoid 240 sequential calls
    // (which trip OpenAI's per-second rate limit on lower tiers).
    // Must hash the EXACT same text `ingest_pages` will embed at upsert
    // time — otherwise the cache keys diverge and the prewarm dodge is
    // a no-op (caught by Codex round-1 finding).
    let bodies_owned: Vec<String> = fixture.pages.iter().map(ingest_text_for).collect();
    let cached_for_warm = Arc::clone(&cached);
    let oai_for_batch = Arc::clone(&oai_concrete);
    let inserted = tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = bodies_owned.iter().map(String::as_str).collect();
        cached_for_warm.prewarm_with(&refs, |texts| {
            oai_for_batch
                .embed_documents(texts)
                .map_err(cairn_embeddings_local::EmbeddingError::from)
        })
    })
    .await
    .map_err(|e| anyhow::anyhow!("prewarm task: {e}"))?
    .map_err(|e| anyhow::anyhow!("prewarm: {e}"))?;
    println!("hybrid-openai-rrf: prewarmed {inserted} new embeddings");

    let embedder: Arc<dyn EmbeddingModel> = cached;
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
