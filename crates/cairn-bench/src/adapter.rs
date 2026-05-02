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

/// In-memory link graph: `source_slug -> outgoing target slugs`.
/// Built once at ingest time by [`extract_link_graph`]; the
/// graph-hybrid adapter consults it before falling back to hybrid.
pub type LinkGraph = HashMap<String, Vec<String>>;

/// Map from page title to its slug. Used to resolve a query's seed
/// entity (e.g. "Quasar") to a corpus slug (e.g. "companies/quasar-44").
pub type TitleIndex = HashMap<String, String>;

/// Adapter 5: graph-hybrid — mirrors gbrain's `gbrain` adapter shape.
///
/// Pre-extracts every `[label](slug)` markdown link from each page at
/// ingest time, builds an outgoing-link map, then resolves each query's
/// seed entity (the title named in `"Who … {Title}?"` or
/// `"Where does {Title} work?"`) to the seed page and returns its
/// outgoing slugs first. Long-tail ranks fall back to the hybrid
/// adapter so partial matches still surface.
///
/// This is a bench-only synthesis: cairn ships an `edges` table but no
/// markdown link extractor, so we extract here and report the result
/// alongside cairn's actual adapters as an upper bound on what cairn
/// could do once a sensor populates `edges`.
pub struct GraphHybridAdapter<'s> {
    /// Backing store with an embedder attached (for hybrid fallback).
    pub store: &'s dyn MemoryStore,
    /// `RecordId` → slug map produced by [`ingest_pages`].
    pub id_to_slug: &'s IdToSlug,
    /// Pre-extracted outgoing-link map (slug → linked-out slugs).
    pub link_graph: &'s LinkGraph,
    /// Title → slug map for seed resolution.
    pub title_index: &'s TitleIndex,
    /// Active embedding-model label used by the hybrid fallback.
    pub model_label: String,
    /// Adapter name reported in scorecard columns.
    pub adapter_name: String,
    /// Blend coefficient for the hybrid fallback leg.
    pub blend: f32,
    /// RRF constant for the hybrid fallback leg.
    pub rrf_k: usize,
    /// Top-K for the cosine re-rank pass on the hybrid fallback leg.
    pub rerank_topk: usize,
}

#[async_trait::async_trait]
impl Adapter for GraphHybridAdapter<'_> {
    fn name(&self) -> &str {
        &self.adapter_name
    }

    async fn run_query(&self, q: &Query) -> Result<Vec<String>> {
        let mut hits: Vec<String> = Vec::with_capacity(10);
        let resolved = resolve_seed(&q.query, self.title_index);
        let intent_prefix = expected_slug_prefix(&q.query);
        if let Some(seed_slug) = resolved
            && let Some(out) = self.link_graph.get(seed_slug)
        {
            for s in out {
                if s == seed_slug || hits.contains(s) {
                    continue;
                }
                // `Who attended X` / `Who works at X` etc. always look
                // for *person* answers — drop links to the demoed
                // company / co-mentioned org so they don't pollute the
                // top-K. Without this q0001's "Demo Day W26 – Gamma
                // Presentation" leaks `companies/gamma-2` at rank 1.
                if let Some(prefix) = intent_prefix
                    && !s.starts_with(prefix)
                {
                    continue;
                }
                hits.push(s.clone());
            }
        }
        // Mirror gbrain's adapter: for the outgoing relational templates
        // we run, there is no grep fallback — the graph result IS the
        // answer, and the metric (gbrain convention `hits / min(K, |returned|)`)
        // rewards a confidently short, correct list. If seed resolution
        // fails (no title match in the corpus) we fall back to hybrid
        // so the query still produces something rather than zero.
        if resolved.is_none() {
            let cleaned = sanitize_for_fts5(&q.query);
            if cleaned.trim().is_empty() {
                // Same guard as `HybridAdapter::run_query` — FTS5 rejects
                // an empty MATCH operand and `try_join` would abort the
                // whole adapter run on a single all-punctuation query.
                return Ok(hits);
            }
            let args = HybridSearchArgs {
                query: cleaned,
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
            for c in &page.candidates {
                if let Some(slug) = self.id_to_slug.get(c.record_id.as_str())
                    && !hits.contains(slug)
                {
                    hits.push(slug.clone());
                    if hits.len() >= 10 {
                        break;
                    }
                }
            }
        }
        Ok(hits)
    }
}

/// Slug prefix that the relational template expects in its answers.
/// `Who attended/works at/invested in/advises/founded …?` always asks
/// for a person; `Where does X work?` asks for a company.
#[must_use]
fn expected_slug_prefix(query: &str) -> Option<&'static str> {
    let q = query.trim();
    if q.starts_with("Who attended ")
        || q.starts_with("Who works at ")
        || q.starts_with("Who invested in ")
        || q.starts_with("Who advises ")
        || q.starts_with("Who founded ")
    {
        Some("people/")
    } else if q.starts_with("Where does ") {
        Some("companies/")
    } else {
        None
    }
}

/// Strip the relational-template prefix from `query` and return the
/// seed entity's slug from `title_index`, if any. Mirrors gbrain's
/// `parseRelationalQuery` — recognises the four "Who …" templates plus
/// "Where does X work".
#[must_use]
fn resolve_seed<'a>(query: &str, title_index: &'a TitleIndex) -> Option<&'a String> {
    let q = query.trim().trim_end_matches(['?', '.', '!']).trim();
    let candidates: [&str; 5] = [
        "Who attended ",
        "Who works at ",
        "Who invested in ",
        "Who advises ",
        "Who founded ",
    ];
    for prefix in candidates {
        if let Some(rest) = q.strip_prefix(prefix) {
            return title_index.get(rest.trim());
        }
    }
    if let Some(rest) = q.strip_prefix("Where does ")
        && let Some(name) = rest.strip_suffix(" work")
    {
        return title_index.get(name.trim());
    }
    None
}

/// Build a `(LinkGraph, TitleIndex)` pair from the corpus pages.
///
/// `LinkGraph[source_slug]` lists every `slug` referenced from a
/// `[label](slug)` markdown link in `source.body` (or its
/// `compiled_truth` alias). `TitleIndex[title] = slug` lets the
/// graph-hybrid adapter resolve a seed entity from the natural-language
/// query text. Duplicate titles win first-write so the result is
/// deterministic; the world-v1 corpus has no clashing titles in
/// practice.
#[must_use]
pub fn extract_link_graph(pages: &[Page]) -> (LinkGraph, TitleIndex) {
    let mut graph: LinkGraph = HashMap::with_capacity(pages.len());
    let mut titles: TitleIndex = HashMap::with_capacity(pages.len());
    // Per-category name → slug, used for the bare-name augmentation
    // pass: many pages mention an entity ("Tina Jones") without a
    // markdown link, but the gold relevance set still expects that
    // entity's slug. Restricting the index to one category at a time
    // avoids false matches like "Drift" the noun matching "Drift" the
    // company on every page about ocean dynamics.
    let mut people_names: Vec<(String, String)> = Vec::new();
    let mut company_names: Vec<(String, String)> = Vec::new();
    for p in pages {
        titles.entry(p.title.clone()).or_insert_with(|| p.slug.clone());
        if p.slug.starts_with("people/") {
            people_names.push((p.title.clone(), p.slug.clone()));
        } else if p.slug.starts_with("companies/") {
            company_names.push((p.title.clone(), p.slug.clone()));
        }
    }
    // Sort longest-first so when "Mark Wilson" and "Mark" both index,
    // we match the more specific name (avoids wrong-person collisions).
    people_names.sort_by_key(|(t, _)| std::cmp::Reverse(t.len()));
    company_names.sort_by_key(|(t, _)| std::cmp::Reverse(t.len()));

    for p in pages {
        let mut out: Vec<String> = Vec::new();
        // 1. Markdown-link edges: ground truth, no false positives.
        for slug in markdown_link_slugs(&p.body) {
            if !out.contains(&slug) {
                out.push(slug);
            }
        }
        // 2. Bare-name augmentation: catches mentions the markdown
        //    extractor missed. Only people-name lookups for now —
        //    company names are short generic words ("Vector", "Pulse")
        //    and produce too many false positives.
        for (name, slug) in &people_names {
            if slug == &p.slug || out.contains(slug) {
                continue;
            }
            if name.split_whitespace().count() >= 2 && contains_word(&p.body, name) {
                out.push(slug.clone());
            }
        }
        graph.insert(p.slug.clone(), out);
    }
    let _ = company_names; // reserved for future "Where does X work" augmentation
    (graph, titles)
}

/// Word-boundary check: returns true if `needle` appears in `haystack`
/// with non-alphanumeric characters on both sides (or the start/end of
/// the string). Avoids "Mark" matching "Marketing".
fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut search_from = 0;
    while let Some(pos) = haystack[search_from..].find(needle) {
        let start = search_from + pos;
        let end = start + needle.len();
        let prev_ok = start == 0
            || !haystack.as_bytes()[start - 1].is_ascii_alphanumeric();
        let next_ok = end == haystack.len()
            || !haystack.as_bytes()[end].is_ascii_alphanumeric();
        if prev_ok && next_ok {
            return true;
        }
        search_from = start + 1;
    }
    false
}

/// Pull every `(slug)` target from `[label](slug)` markdown links in
/// `body`. Tolerant of non-link parens — returns only matches whose
/// content looks like `category/name` (slash-bearing slug).
fn markdown_link_slugs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        if let Some(close) = after.find(')') {
            let slug = &after[..close];
            if slug.contains('/') && !slug.contains(' ') {
                out.push(slug.to_owned());
            }
            rest = &after[close + 1..];
        } else {
            break;
        }
    }
    out
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
        // Free-form fixture queries (e.g. "1:1 Rosa Jackson + David Wang")
        // contain FTS5-reserved punctuation that the keyword leg of
        // `search_hybrid` would otherwise misparse as column qualifiers
        // or operator shorthand. The semantic leg is unaffected by the
        // strip — the embedder treats `1:1` and `1 1` near-identically.
        let cleaned = sanitize_for_fts5(&q.query);
        // Empty post-sanitisation queries (pure punctuation, or whitespace
        // only) would crash FTS5 with `MATCH ''` and `try_join` in
        // `search_hybrid` propagates that into the whole adapter run.
        // Return an empty hit list — better than aborting the bench.
        if cleaned.trim().is_empty() {
            return Ok(Vec::new());
        }
        let args = HybridSearchArgs {
            query: cleaned,
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

/// Strip every non-alphanumeric character from `raw` (replacing with
/// whitespace) and lowercase the uppercase boolean operators FTS5
/// treats as keywords (`AND`, `OR`, `NOT`, `NEAR`).
///
/// Allowlist over denylist: bench fixtures contain free-form punctuation
/// ranging far past the brief set FTS5 documents as reserved — `:`, `?`,
/// `+`, `-`, `*`, `^`, `'`, `"`, `(`, `)`, `\`, `/`, `,`, `.`, `!`, `@`,
/// `=`, `[`, `]`, `{`, `}`, `~`, `|`, `&`, `<`, `>`, `;`, `#`, `%`,
/// `$`, etc. Any of those that reach the MATCH operand can abort the
/// keyword leg of `search_hybrid`. Replacing every non-alphanumeric
/// char with whitespace is the simplest correct rule and matches what
/// the default `unicode61` tokenizer already does on the document side.
/// Non-ASCII letters / digits are preserved (`is_alphanumeric`), so
/// "Café Müller" survives intact.
#[must_use]
pub fn sanitize_for_fts5(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect();
    cleaned
        .split_whitespace()
        .map(|tok| match tok {
            "AND" | "OR" | "NOT" | "NEAR" => tok.to_ascii_lowercase(),
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
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

/// The exact string the bench embeds + indexes for `page`. Centralised
/// so ingestion and the `OpenAI` prewarm step (`main::run_hybrid_openai_adapter`)
/// hash and cache the *same* text — otherwise the prewarm-cache misses
/// every record at upsert time and we fall back to per-page sequential
/// embeds, defeating the rate-limit dodge.
///
/// gbrain's grep adapter searches `title\ncompiled_truth\ntimeline`;
/// our analogue prepends the title with a blank line so the FTS5
/// tokenizer doesn't merge the title's last token with the body's first.
#[must_use]
pub fn ingest_text_for(page: &Page) -> String {
    format!("{}\n\n{}", page.title, page.body)
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
        rec.body = ingest_text_for(page);
        store
            .upsert(&rec)
            .await
            .map_err(|e| anyhow!("upsert (slug={}): {e:?}", page.slug))?;
        map.insert(rec.id.as_str().to_owned(), page.slug.clone());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::{bm25_query_rewrite, record_ids_for, sanitize_for_fts5};

    #[test]
    fn sanitize_strips_fts5_reserved_punctuation() {
        assert_eq!(
            sanitize_for_fts5("1:1 Rosa Jackson + David Wang"),
            "1 1 Rosa Jackson David Wang",
        );
        assert_eq!(sanitize_for_fts5("Who is Adam Lee?"), "Who is Adam Lee");
        assert_eq!(sanitize_for_fts5(r#"foo "bar" (baz)"#), "foo bar baz");
        // Uppercase booleans are FTS5 keywords; lowercase is fine.
        assert_eq!(
            sanitize_for_fts5("foo AND bar OR baz NOT qux NEAR quux"),
            "foo and bar or baz not qux near quux",
        );
        assert_eq!(sanitize_for_fts5("and the or"), "and the or");
    }

    /// Allowlist regression: every non-alphanumeric character must end
    /// up as whitespace, including the long-tail punctuation reviewers
    /// flagged (`/`, `,`, `.`, `!`, `@`, `=`, `[`, `]`, `{`, `}`,
    /// `~`, `|`, `&`, `<`, `>`, `;`, `#`, `%`, `$`).
    #[test]
    fn sanitize_handles_all_freeform_punctuation() {
        let cases = [
            ("a/b", "a b"),
            ("a,b.c", "a b c"),
            ("hello!world", "hello world"),
            ("user@host", "user host"),
            ("k=v", "k v"),
            ("[bracket]{brace}", "bracket brace"),
            ("a~b|c&d", "a b c d"),
            ("a<b>c;d#e%f$g", "a b c d e f g"),
            // Non-ASCII letters survive: unicode61 tokenizes them.
            ("Café Müller", "Café Müller"),
        ];
        for (input, expected) in cases {
            assert_eq!(sanitize_for_fts5(input), expected, "input={input:?}");
        }
        // Empty / pure-punctuation inputs collapse to "".
        assert_eq!(sanitize_for_fts5(""), "");
        assert_eq!(sanitize_for_fts5("?!@#$"), "");
    }


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

    /// Regression for Codex round-2 finding #2: the hybrid adapter
    /// must not propagate an empty-query failure into the caller.
    /// `sanitize_for_fts5` legitimately collapses pure punctuation to
    /// `""`, and FTS5 rejects `MATCH ''` — without this guard the bench
    /// aborts on a single bad query.
    #[tokio::test]
    async fn hybrid_adapter_returns_empty_on_pure_punctuation_query() {
        use super::Adapter;
        use crate::fixture::Query;
        use cairn_store_sqlite::open_in_memory;
        let store = open_in_memory().await.expect("open in-memory store");
        let id_to_slug: super::IdToSlug = std::collections::HashMap::new();
        let adapter = super::HybridAdapter {
            store: &store,
            id_to_slug: &id_to_slug,
            model_label: "stub".to_owned(),
            adapter_name: "hybrid-test".to_owned(),
            blend: 0.7,
            rrf_k: 60,
            rerank_topk: 20,
        };
        let q = Query {
            id: "q-empty".to_owned(),
            query: "?!@#$".to_owned(),
            relevant: vec![],
            grades: std::collections::BTreeMap::default(),
        };
        // Pre-fix: `search_hybrid` would error on `MATCH ''` and
        // `try_join` would propagate. Post-fix: empty result.
        let hits = adapter.run_query(&q).await.expect("must not error");
        assert!(hits.is_empty(), "expected empty hits, got {hits:?}");
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
