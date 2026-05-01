//! Load `BrainBench` world-v1 fixture: pages, queries, upstream baseline.
//!
//! The fixture root is expected to contain:
//!
//! - `pages/*.json` — one [`Page`] per file
//! - `queries.json` — array of [`Query`]
//! - `upstream-baseline.json` (optional) — pre-computed [`UpstreamBaseline`]
//!
//! Pages are sorted by slug after load so downstream runs are
//! reproducible regardless of filesystem iteration order.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// One `BrainBench` corpus page.
#[derive(Debug, Clone, Deserialize)]
pub struct Page {
    /// Stable identifier for the page; used as the relevance key.
    pub slug: String,
    /// Page title.
    pub title: String,
    /// Page body (raw text).
    pub body: String,
    /// Free-form metadata used by upstream's query-derivation logic.
    /// Treated as opaque here.
    #[serde(default, rename = "_facts")]
    pub facts: serde_json::Value,
}

/// One `BrainBench` evaluation query with its relevance judgements.
#[derive(Debug, Clone, Deserialize)]
pub struct Query {
    /// Query identifier.
    pub id: String,
    /// Natural-language query text.
    pub query: String,
    /// Slugs that are relevant to this query.
    #[serde(default)]
    pub relevant: Vec<String>,
    /// Per-slug grade (1, 2, 3 = increasing relevance). Defaults to 1
    /// for any slug present in `relevant` but missing here.
    #[serde(default)]
    pub grades: BTreeMap<String, u32>,
}

/// Pre-computed upstream metric snapshot used as a reference column.
#[derive(Debug, Clone, Deserialize)]
pub struct UpstreamBaseline {
    /// Map of adapter name to per-adapter metric snapshot.
    pub adapters: BTreeMap<String, AdapterBaseline>,
}

/// One adapter's headline aggregate + per-query metrics.
#[derive(Debug, Clone, Deserialize)]
pub struct AdapterBaseline {
    /// Headline aggregate metrics.
    pub aggregate: AggregateMetrics,
    /// Per-query metric snapshots.
    #[serde(default)]
    pub per_query: Vec<QueryResult>,
}

/// Headline aggregate IR metrics for a single adapter.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AggregateMetrics {
    /// Precision at 5.
    pub p_at_5: f64,
    /// Recall at 5.
    pub r_at_5: f64,
    /// Mean reciprocal rank.
    pub mrr: f64,
    /// Normalized discounted cumulative gain at 5.
    pub ndcg_at_5: f64,
}

/// Per-query metric snapshot.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryResult {
    /// Query identifier (matches [`Query::id`]).
    pub query_id: String,
    /// Precision at 5.
    pub p_at_5: f64,
    /// Recall at 5.
    pub r_at_5: f64,
    /// Mean reciprocal rank.
    pub mrr: f64,
    /// Normalized discounted cumulative gain at 5.
    pub ndcg_at_5: f64,
}

/// Loaded fixture: pages + queries + optional upstream baseline.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// Corpus pages, sorted by slug.
    pub pages: Vec<Page>,
    /// Evaluation queries.
    pub queries: Vec<Query>,
    /// Optional reference baseline for the report's "upstream" column.
    pub upstream: Option<UpstreamBaseline>,
}

/// Load a fixture from `root`.
///
/// # Errors
///
/// Returns an error if `root/pages` is missing, any page or query JSON
/// fails to parse, or `upstream-baseline.json` exists but is malformed.
pub fn load(root: &Path) -> Result<Fixture> {
    // Pages.
    let pages_dir = root.join("pages");
    let mut pages = Vec::new();
    let read_dir = std::fs::read_dir(&pages_dir)
        .with_context(|| format!("read pages dir at {}", pages_dir.display()))?;
    for entry in read_dir {
        let entry = entry.with_context(|| format!("iterate {}", pages_dir.display()))?;
        if !entry.file_name().to_string_lossy().ends_with(".json") {
            continue;
        }
        let path = entry.path();
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let page: Page =
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
        pages.push(page);
    }
    pages.sort_by(|a, b| a.slug.cmp(&b.slug));

    // Queries.
    let q_path = root.join("queries.json");
    let q_raw = std::fs::read_to_string(&q_path)
        .with_context(|| format!("read queries at {}", q_path.display()))?;
    let queries: Vec<Query> = serde_json::from_str(&q_raw).context("parse queries.json")?;

    // Upstream baseline (optional).
    let baseline_path = root.join("upstream-baseline.json");
    let upstream = if baseline_path.exists() {
        let raw = std::fs::read_to_string(&baseline_path)
            .with_context(|| format!("read {}", baseline_path.display()))?;
        Some(serde_json::from_str(&raw).context("parse upstream-baseline.json")?)
    } else {
        None
    };

    Ok(Fixture {
        pages,
        queries,
        upstream,
    })
}
