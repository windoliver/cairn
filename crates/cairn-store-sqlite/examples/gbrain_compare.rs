// One-shot benchmark example. The pedantic-clippy lints below are
// either noisy or irrelevant for a small offline IR-metrics script
// (cast precision is fine because k and the corpus are tiny; the
// "similar names" complaints are between unrelated single-letter
// identifiers in tight numeric loops).
#![allow(
    clippy::cast_precision_loss,
    clippy::similar_names,
    clippy::struct_field_names,
    clippy::doc_markdown,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc
)]
//! gbrain search-quality benchmark on cairn's FTS5 keyword search.
//!
//! Loads `fixtures/v0/gbrain/search-bench.json` (extracted from
//! `garrytan/gbrain`'s `test/benchmark-search-quality.ts`), upserts each
//! page through `SqliteMemoryStore`, runs the 20 queries against
//! `search_keyword`, and prints per-query + aggregate IR metrics so we
//! can compare against gbrain's published numbers.
//!
//! Run with:
//!
//! ```text
//! cargo run --release -p cairn-store-sqlite --example gbrain_compare
//! ```
//!
//! ## Fairness caveat
//!
//! gbrain runs hybrid retrieval (pgvector cosine + Postgres FTS, fused
//! via reciprocal-rank fusion). Cairn at this branch implements only
//! the FTS5 keyword leg — vector search lands in a later issue. Treat
//! these numbers as the keyword-only baseline; gbrain's hybrid stack
//! will outperform on semantic queries (q03 "climate tech investing",
//! q12 "AI replacing designers"). On entity / lexical queries (q01
//! "Who is Alice Chen?", q07 "Alice Chen NovaPay") the two should be
//! roughly comparable.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use cairn_core::contract::memory_store::{KeywordSearchArgs, MemoryStore};
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use cairn_store_sqlite::open_in_memory;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    pages: Vec<Page>,
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Page {
    slug: String,
    body: String,
    #[allow(dead_code)]
    title: String,
}

#[derive(Deserialize)]
struct Query {
    id: String,
    query: String,
    relevant: Vec<String>,
    #[serde(default)]
    grades: BTreeMap<String, u32>,
}

/// Crockford base32 encoding for record-id index suffixes.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn record_id_for(idx: usize) -> (RecordId, TargetId) {
    let high = CROCKFORD[(idx >> 5) & 0x1F] as char;
    let low = CROCKFORD[idx & 0x1F] as char;
    let mut s = String::from("01HQZX9F5N00000000000000");
    s.push(high);
    s.push(low);
    let rid = RecordId::parse(s.clone()).expect("valid record id");
    let tid = TargetId::parse(s).expect("valid target id");
    (rid, tid)
}

fn build_record(idx: usize, body: &str) -> MemoryRecord {
    let mut r = cairn_core::domain::record::tests_export::sample_record();
    let (rid, tid) = record_id_for(idx);
    r.id = rid;
    r.target_id = tid;
    body.clone_into(&mut r.body);
    r
}

#[derive(Clone)]
struct QueryRun {
    id: String,
    query: String,
    expected_relevant: BTreeSet<String>,
    grades: BTreeMap<String, u32>,
    hits: Vec<String>, // result slugs in rank order
}

fn precision_at_k(hits: &[String], rel: &BTreeSet<String>, k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    let take = hits.iter().take(k).filter(|s| rel.contains(*s)).count();
    take as f64 / k as f64
}

fn recall_at_k(hits: &[String], rel: &BTreeSet<String>, k: usize) -> f64 {
    if rel.is_empty() {
        return f64::NAN;
    }
    let take = hits.iter().take(k).filter(|s| rel.contains(*s)).count();
    take as f64 / rel.len() as f64
}

fn mrr(hits: &[String], rel: &BTreeSet<String>) -> f64 {
    for (i, h) in hits.iter().enumerate() {
        if rel.contains(h) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

fn ndcg_at_k(
    hits: &[String],
    grades: &BTreeMap<String, u32>,
    rel: &BTreeSet<String>,
    k: usize,
) -> f64 {
    // gbrain's analyzeRun defaults missing grade to 1 for any relevant slug.
    let grade_of = |slug: &str| -> u32 {
        if let Some(g) = grades.get(slug) {
            *g
        } else {
            u32::from(rel.contains(slug))
        }
    };
    let mut dcg = 0.0;
    for (i, h) in hits.iter().take(k).enumerate() {
        let g = f64::from(grade_of(h));
        dcg += g / ((i as f64 + 2.0).log2());
    }
    let mut graded: Vec<u32> = rel.iter().map(|s| grade_of(s)).collect();
    graded.sort_unstable_by(|a, b| b.cmp(a));
    let mut idcg = 0.0;
    for (i, g) in graded.into_iter().take(k).enumerate() {
        idcg += f64::from(g) / ((i as f64 + 2.0).log2());
    }
    if idcg == 0.0 { 0.0 } else { dcg / idcg }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Locate fixture relative to repo root. Walk up from CARGO_MANIFEST_DIR.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .ancestors()
        .find(|p| p.join("fixtures/v0/gbrain/search-bench.json").exists())
        .ok_or("fixture not found from CARGO_MANIFEST_DIR ancestry")?;
    let fixture_path = repo_root.join("fixtures/v0/gbrain/search-bench.json");
    let raw = std::fs::read_to_string(&fixture_path)?;
    let fixture: Fixture = serde_json::from_str(&raw)?;

    println!(
        "loaded fixture: {} pages, {} queries (from {})",
        fixture.pages.len(),
        fixture.queries.len(),
        fixture_path.display(),
    );

    // Index every page.
    let store = open_in_memory().await?;
    let mut id_to_slug: HashMap<String, String> = HashMap::new();
    for (idx, page) in fixture.pages.iter().enumerate() {
        let rec = build_record(idx, &page.body);
        id_to_slug.insert(rec.id.as_str().to_owned(), page.slug.clone());
        store.upsert(&rec).await?;
    }

    // Run every query under two query-rewrite modes. FTS5 treats `?`,
    // `!`, etc. as syntax, and its default `MATCH` is implicit-AND —
    // "Who is Alice Chen" requires every token in the body. Cairn's
    // verb layer (issue #62) will own user→FTS rewriting; we evaluate
    // two rewrite strategies inline here:
    //
    //   * `and`: drop punctuation, AND-join non-stopword tokens. This
    //            mirrors what a naive verb implementation would emit
    //            and what most users get from "raw FTS".
    //   * `or` : same token cleanup, but OR-join the tokens. More
    //            forgiving for natural-language phrasing — closer to
    //            what BM25-driven IR systems actually do under the hood.
    //
    // Reporting both lets us show the headroom available to a smarter
    // verb layer without changing the store at all.
    let runs_and = run_queries(&store, &fixture.queries, &id_to_slug, RewriteMode::And).await?;
    let runs_or = run_queries(&store, &fixture.queries, &id_to_slug, RewriteMode::Or).await?;

    print_table("AND (implicit-AND, raw FTS5)", &runs_and);
    println!();
    print_table("OR  (term-OR rewrite)", &runs_or);

    println!();
    println!("query-level details (OR mode):");
    for run in &runs_or {
        let rel_in_top5 = run
            .hits
            .iter()
            .take(5)
            .filter(|s| run.expected_relevant.contains(*s))
            .count();
        println!(
            "  {} '{}' → top5={:?} (relevant_hit={}/{}, expected={:?})",
            run.id,
            truncate(&run.query, 50),
            run.hits.iter().take(5).collect::<Vec<_>>(),
            rel_in_top5,
            run.expected_relevant.len(),
            run.expected_relevant.iter().collect::<Vec<_>>(),
        );
    }
    Ok(())
}

#[derive(Copy, Clone)]
enum RewriteMode {
    And,
    Or,
}

const STOPWORDS: &[&str] = &[
    "a",
    "an",
    "and",
    "the",
    "is",
    "are",
    "was",
    "were",
    "do",
    "does",
    "did",
    "to",
    "of",
    "on",
    "in",
    "at",
    "for",
    "with",
    "about",
    "this",
    "that",
    "what",
    "who",
    "whom",
    "when",
    "where",
    "why",
    "how",
    "tell",
    "me",
    "give",
    "everything",
    "deep",
    "dive",
    "recent",
    "happened",
    "last",
    "year",
    "this",
    "thing",
    "stuff",
];

fn rewrite(query: &str, mode: RewriteMode) -> String {
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
        .filter(|t| !STOPWORDS.contains(&t.to_lowercase().as_str()))
        .map(str::to_owned)
        .collect();
    if tokens.is_empty() {
        return String::new();
    }
    match mode {
        RewriteMode::And => tokens.join(" "),
        RewriteMode::Or => tokens.join(" OR "),
    }
}

async fn run_queries(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    queries: &[Query],
    id_to_slug: &HashMap<String, String>,
    mode: RewriteMode,
) -> Result<Vec<QueryRun>, Box<dyn std::error::Error + Send + Sync>> {
    let mut runs = Vec::with_capacity(queries.len());
    for q in queries {
        let rewritten = rewrite(&q.query, mode);
        let hits: Vec<String> = if rewritten.is_empty() {
            Vec::new()
        } else {
            let args = KeywordSearchArgs {
                query: rewritten,
                filter: None,
                visibility_allowlist: vec![MemoryVisibility::Private],
                limit: 10,
                cursor: None,
            };
            let page = store.search_keyword(&args).await?;
            page.candidates
                .iter()
                .filter_map(|c| id_to_slug.get(c.record_id.as_str()).cloned())
                .collect()
        };
        runs.push(QueryRun {
            id: q.id.clone(),
            query: q.query.clone(),
            expected_relevant: q.relevant.iter().cloned().collect(),
            grades: q.grades.clone(),
            hits,
        });
    }
    Ok(runs)
}

fn print_table(label: &str, runs: &[QueryRun]) {
    println!("=== {label} ===");
    println!(
        "{:<5} {:<55} {:>5} {:>5} {:>5} {:>6} {:>6}",
        "qid", "query", "P@1", "P@5", "R@5", "MRR", "nDCG5",
    );
    println!("{}", "-".repeat(95));

    let mut sum_p1 = 0.0;
    let mut sum_p5 = 0.0;
    let mut sum_r5 = 0.0;
    let mut sum_mrr = 0.0;
    let mut sum_ndcg = 0.0;
    let mut graded = 0;
    for run in runs {
        if run.expected_relevant.is_empty() {
            println!(
                "{:<5} {:<55} {:>5} {:>5} {:>5} {:>6} {:>6}",
                run.id,
                truncate(&run.query, 55),
                "—",
                "—",
                "—",
                "—",
                "—",
            );
            continue;
        }
        let p1 = precision_at_k(&run.hits, &run.expected_relevant, 1);
        let p5 = precision_at_k(&run.hits, &run.expected_relevant, 5);
        let r5 = recall_at_k(&run.hits, &run.expected_relevant, 5);
        let mrr_v = mrr(&run.hits, &run.expected_relevant);
        let ndcg = ndcg_at_k(&run.hits, &run.grades, &run.expected_relevant, 5);
        sum_p1 += p1;
        sum_p5 += p5;
        sum_r5 += r5;
        sum_mrr += mrr_v;
        sum_ndcg += ndcg;
        graded += 1;
        println!(
            "{:<5} {:<55} {:>5.2} {:>5.2} {:>5.2} {:>6.3} {:>6.3}",
            run.id,
            truncate(&run.query, 55),
            p1,
            p5,
            r5,
            mrr_v,
            ndcg,
        );
    }
    println!("{}", "-".repeat(95));
    let n = f64::from(graded);
    println!(
        "{:<5} {:<55} {:>5.2} {:>5.2} {:>5.2} {:>6.3} {:>6.3}",
        "AVG",
        format!("({graded} graded queries)"),
        sum_p1 / n,
        sum_p5 / n,
        sum_r5 / n,
        sum_mrr / n,
        sum_ndcg / n,
    );
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}
