//! Cairn-side adapters: bm25-only, vector-bge, hybrid-bge-rrf, hybrid-openai-rrf.
//!
//! Each adapter wraps a configured [`MemoryStore`] and translates a single
//! [`Query`] into a slug-ranked hit list. The `bench` binary runs all four
//! against a freshly-ingested fixture so per-query metrics are directly
//! comparable.
//!
//! Slug attribution: every fixture page is upserted with a deterministic
//! `RecordId` derived from its index, and the inverse map
//! ([`IdToSlug`]) is consulted on each search hit to translate the
//! candidate's `RecordId` back to its corpus slug.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use cairn_core::contract::memory_store::{
    HybridSearchArgs, KeywordSearchArgs, MemoryStore, SemanticSearchArgs,
};
use cairn_core::domain::taxonomy::MemoryVisibility;

use crate::fixture::{Page, Query};

/// Mapping from the indexed `RecordId` (string) → corpus page slug.
///
/// Built once by [`ingest_pages`] and shared across adapter runs.
pub type IdToSlug = HashMap<String, String>;

/// Result for a single (adapter, query) pair.
///
/// `#[non_exhaustive]` because reporters may grow new fields (latency,
/// token cost) without that being a breaking change for the four
/// adapters that produce these.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AdapterRun {
    /// Adapter name (matches [`Adapter::name`]).
    pub adapter: String,
    /// Query identifier (mirrors [`Query::id`]).
    pub query_id: String,
    /// Raw query string evaluated.
    pub query: String,
    /// Page slugs in rank order.
    pub hits: Vec<String>,
}

/// One `BrainBench` adapter — wraps a configured store and a query rewrite
/// pipeline. Implementations are constructed once per fixture and reused
/// across queries.
#[async_trait::async_trait]
pub trait Adapter {
    /// Stable adapter name used in report columns.
    fn name(&self) -> &str;

    /// Run a single query through this adapter.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying store call fails.
    async fn run_query(&self, query: &Query) -> Result<Vec<String>>;
}

/// Adapter 1: bm25-only — keyword FTS5 leg with a naïve OR-rewrite.
pub struct Bm25Adapter<'s> {
    /// Backing store. Caller-owned.
    pub store: &'s dyn MemoryStore,
    /// `RecordId` → slug map produced by [`ingest_pages`].
    pub id_to_slug: &'s IdToSlug,
}

#[async_trait::async_trait]
impl Adapter for Bm25Adapter<'_> {
    // Trait sig is `&str` (other impls borrow from `self`); a `'static`
    // tightening would not match the trait, so this allow is local.
    #[allow(
        clippy::unnecessary_literal_bound,
        reason = "Adapter::name returns `&str` to allow borrowing from self in other impls"
    )]
    fn name(&self) -> &str {
        "bm25-only"
    }

    async fn run_query(&self, q: &Query) -> Result<Vec<String>> {
        let rewritten = bm25_query_rewrite(&q.query);
        if rewritten.is_empty() {
            return Ok(Vec::new());
        }
        let args = KeywordSearchArgs {
            query: rewritten,
            filter: None,
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            cursor: None,
        };
        let page = self
            .store
            .search_keyword(&args)
            .await
            .map_err(|e| anyhow!("search_keyword: {e}"))?;
        Ok(page
            .candidates
            .iter()
            .filter_map(|c| self.id_to_slug.get(c.record_id.as_str()).cloned())
            .collect())
    }
}

/// Adapter 2: vector-only — single-leg semantic search.
pub struct VectorAdapter<'s> {
    /// Backing store with an embedder attached.
    pub store: &'s dyn MemoryStore,
    /// `RecordId` → slug map produced by [`ingest_pages`].
    pub id_to_slug: &'s IdToSlug,
    /// Active embedding-model label.
    pub model_label: String,
    /// Adapter name reported in scorecard columns.
    pub adapter_name: String,
}

#[async_trait::async_trait]
impl Adapter for VectorAdapter<'_> {
    fn name(&self) -> &str {
        &self.adapter_name
    }

    async fn run_query(&self, q: &Query) -> Result<Vec<String>> {
        let args = SemanticSearchArgs {
            query: q.query.clone(),
            filter: None,
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            model_label: self.model_label.clone(),
        };
        let page = self
            .store
            .search_semantic(&args)
            .await
            .map_err(|e| anyhow!("search_semantic: {e}"))?;
        Ok(page
            .candidates
            .iter()
            .filter_map(|c| self.id_to_slug.get(c.record_id.as_str()).cloned())
            .collect())
    }
}

/// Adapter 3 / 4: hybrid (RRF + cosine re-rank).
///
/// Same struct serves the `BGE` and `OpenAI` columns — the only difference is
/// which embedder is attached to `store` and the `model_label` it agrees
/// with. RRF and re-rank knobs are owned here so the report can show the
/// exact configuration used.
pub struct HybridAdapter<'s> {
    /// Backing store with an embedder attached.
    pub store: &'s dyn MemoryStore,
    /// `RecordId` → slug map produced by [`ingest_pages`].
    pub id_to_slug: &'s IdToSlug,
    /// Active embedding-model label.
    pub model_label: String,
    /// Adapter name reported in scorecard columns.
    pub adapter_name: String,
    /// Blend coefficient (0.0–1.0). 1.0 skips cosine re-rank.
    pub blend: f32,
    /// RRF constant. Canonical default 60.
    pub rrf_k: usize,
    /// Top-K from RRF to second-pass re-rank with cosine. Canonical 20.
    pub rerank_topk: usize,
}

#[async_trait::async_trait]
impl Adapter for HybridAdapter<'_> {
    fn name(&self) -> &str {
        &self.adapter_name
    }

    async fn run_query(&self, q: &Query) -> Result<Vec<String>> {
        let args = HybridSearchArgs {
            query: q.query.clone(),
            filter: None,
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            model_label: self.model_label.clone(),
            blend: self.blend,
            rrf_k: self.rrf_k,
            rerank_topk: self.rerank_topk,
        };
        let page = self
            .store
            .search_hybrid(&args)
            .await
            .map_err(|e| anyhow!("search_hybrid: {e}"))?;
        Ok(page
            .candidates
            .iter()
            .filter_map(|c| self.id_to_slug.get(c.record_id.as_str()).cloned())
            .collect())
    }
}

/// Stopwords stripped before BM25 query rewriting. Matches
/// `crates/cairn-store-sqlite/examples/gbrain_compare.rs` so numbers stay
/// comparable with the existing baseline harness.
const BM25_STOPWORDS: &[&str] = &[
    "a", "an", "and", "the", "is", "are", "was", "were", "do", "does", "did", "to", "of", "on",
    "in", "at", "for", "with", "about", "this", "that", "what", "who", "whom", "when", "where",
    "why", "how",
];

/// Naïve query rewrite used for the BM25 baseline.
///
/// Drops non-alphanumeric punctuation, strips a small stopword list, then
/// joins the surviving tokens with `OR` so FTS5's implicit-AND semantics
/// don't gate every hit on every term being present. Intentionally
/// permissive: a smarter rewrite is the verb layer's job (issue #62).
#[must_use]
pub fn bm25_query_rewrite(query: &str) -> String {
    let cleaned: String = query
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect();
    let tokens: Vec<String> = cleaned
        .split_whitespace()
        .filter(|t| !BM25_STOPWORDS.contains(&t.to_lowercase().as_str()))
        .map(str::to_owned)
        .collect();
    if tokens.is_empty() {
        return String::new();
    }
    tokens.join(" OR ")
}

/// Crockford base32 alphabet used to derive stable per-page record ids
/// from the page index. Matches `gbrain_compare`'s scheme so the two
/// harnesses produce identical record-id sequences for the same fixture.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Build a per-index `(RecordId, TargetId)` pair.
///
/// Both ULIDs share a fixed 24-char prefix and a 2-char Crockford-base32
/// suffix derived from `idx`, so the two harnesses produce comparable
/// id streams. Capacity is `32 * 32 = 1024`; world-v1 has ~24 pages.
fn record_ids_for(
    idx: usize,
) -> Result<(cairn_core::domain::RecordId, cairn_core::domain::TargetId)> {
    use cairn_core::domain::{RecordId, TargetId};
    let high = char::from(CROCKFORD[(idx >> 5) & 0x1F]);
    let low = char::from(CROCKFORD[idx & 0x1F]);
    let mut s = String::from("01HQZX9F5N00000000000000");
    s.push(high);
    s.push(low);
    debug_assert_eq!(s.len(), 26, "ULID record id must be 26 chars");
    let rid = RecordId::parse(s.clone()).context("derived record id")?;
    let tid = TargetId::parse(s).context("derived target id")?;
    Ok((rid, tid))
}

/// Build a record-id → slug map by upserting each page into a store.
///
/// # Errors
///
/// Returns an error if id derivation or `store.upsert` fails for any page.
pub async fn ingest_pages<S: MemoryStore + ?Sized>(store: &S, pages: &[Page]) -> Result<IdToSlug> {
    use cairn_core::domain::record::tests_export::sample_record;
    let mut map = HashMap::with_capacity(pages.len());
    for (idx, page) in pages.iter().enumerate() {
        let mut rec = sample_record();
        let (rid, tid) = record_ids_for(idx)?;
        rec.id = rid;
        rec.target_id = tid;
        rec.body.clone_from(&page.body);
        store
            .upsert(&rec)
            .await
            .map_err(|e| anyhow!("upsert: {e}"))?;
        map.insert(rec.id.as_str().to_owned(), page.slug.clone());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::{bm25_query_rewrite, record_ids_for};

    #[test]
    fn rewrite_drops_punctuation_and_stopwords() {
        let q = "Who is Alice Chen?";
        let r = bm25_query_rewrite(q);
        assert_eq!(r, "Alice OR Chen");
    }

    #[test]
    fn rewrite_empty_when_only_stopwords() {
        assert_eq!(bm25_query_rewrite("the a is"), "");
    }

    #[test]
    fn record_ids_parse_for_a_range() {
        for idx in [0_usize, 1, 31, 32, 1023] {
            let (rid, tid) = record_ids_for(idx).expect("derived ids");
            assert_eq!(rid.as_str().len(), 26);
            assert_eq!(tid.as_str().len(), 26);
        }
    }
}
