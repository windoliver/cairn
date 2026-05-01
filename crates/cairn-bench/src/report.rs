//! `BrainBench` report writer: render the 8-column scorecard as
//! `report.md` plus a per-query JSONL artifact.
//!
//! The writer takes the live cairn adapter runs collected by the binary
//! plus the optional upstream baseline carried on the [`Fixture`] and
//! emits two files into `out_dir`:
//!
//! - `report.md` — markdown scorecard table (one row per adapter,
//!   averaged metrics) with a short header that records the corpus and
//!   query counts.
//! - `per-query.jsonl` — one JSON object per `(adapter, query)` row,
//!   carrying the ranked hits and per-query metrics for downstream
//!   diffing or regression analysis.

// k and the corpus size are tiny relative to f64's mantissa (2^53), so
// the usize-to-f64 cast in `aggregate_cairn` is lossless in practice.
#![allow(clippy::cast_precision_loss)]

use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::fixture::Fixture;
use crate::metrics::PerQueryMetrics;

/// One run for one query: `(query_id, ranked_hits, metrics)`. The triple
/// stays inline because per-query JSONL is the most natural form to hand
/// the writer.
pub type AdapterQueryRun = (String, Vec<String>, PerQueryMetrics);

/// All runs for one adapter: `(adapter_name, per_query_runs)`. Empty
/// `per_query_runs` is the convention used by `skipped(...)` in the
/// binary to mark adapters that were not executed for this run.
pub type AdapterResults = (String, Vec<AdapterQueryRun>);

/// Aggregate metric row for one adapter, as rendered in the scorecard.
///
/// Marked `#[non_exhaustive]` because new metric columns (latency, k=10
/// variants, …) are likely to grow this struct without breaking
/// downstream consumers.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct AggregateRow {
    /// Adapter name, e.g. `"bm25-only"`.
    pub adapter: String,
    /// Mean precision at 5 across graded queries.
    pub p_at_5: f64,
    /// Mean recall at 5 across graded queries.
    pub r_at_5: f64,
    /// Mean reciprocal rank across graded queries.
    pub mrr: f64,
    /// Mean nDCG at 5 across graded queries.
    pub ndcg_at_5: f64,
    /// Number of queries contributing to the averages above.
    pub graded_queries: usize,
}

/// One row of `per-query.jsonl`: the per-query metrics for one
/// `(adapter, query)` pair plus the adapter's ranked hits. Hoisted out
/// of the JSONL loop so the rustdoc and `Serialize` derive sit at module
/// scope rather than inside a function body.
#[derive(Debug, Serialize)]
struct PerQueryRow<'a> {
    adapter: &'a str,
    query_id: &'a str,
    hits: &'a [String],
    p_at_5: f64,
    r_at_5: f64,
    mrr: f64,
    ndcg_at_5: f64,
}

/// Write the `BrainBench` report under `out_dir`.
///
/// Renders `report.md` (the markdown scorecard) and `per-query.jsonl`
/// (one row per `(adapter, query)`). Upstream reference rows are
/// appended after the live cairn rows whenever
/// [`Fixture::upstream`] is `Some`.
///
/// # Errors
///
/// Returns the underlying I/O error when `out_dir` is not writable, the
/// destination files cannot be created, or any write fails. JSON
/// serialization errors propagate from `serde_json` if a row contains
/// non-serializable data.
pub fn write_report(out_dir: &Path, fixture: &Fixture, all_runs: &[AdapterResults]) -> Result<()> {
    let aggregates = aggregate_cairn(all_runs);
    let mut all_rows: Vec<AggregateRow> = aggregates.clone();

    // Append upstream reference rows when present.
    if let Some(up) = &fixture.upstream {
        for (name, ad) in &up.adapters {
            all_rows.push(AggregateRow {
                adapter: name.clone(),
                p_at_5: ad.aggregate.p_at_5,
                r_at_5: ad.aggregate.r_at_5,
                mrr: ad.aggregate.mrr,
                ndcg_at_5: ad.aggregate.ndcg_at_5,
                graded_queries: ad.per_query.len(),
            });
        }
    }

    let md_path = out_dir.join("report.md");
    let mut md = File::create(&md_path).context("create report.md")?;
    writeln!(md, "# BrainBench Scorecard")?;
    writeln!(md)?;
    writeln!(md, "Corpus: {} pages", fixture.pages.len())?;
    writeln!(md, "Queries: {} (graded)", fixture.queries.len())?;
    writeln!(md)?;
    writeln!(md, "| Adapter | P@5 | R@5 | MRR | nDCG@5 | n |")?;
    writeln!(md, "|---|---|---|---|---|---|")?;
    for row in &all_rows {
        writeln!(
            md,
            "| `{}` | {:.3} | {:.3} | {:.3} | {:.3} | {} |",
            row.adapter,
            row.p_at_5,
            row.r_at_5,
            row.mrr,
            row.ndcg_at_5,
            row.graded_queries,
        )?;
    }
    writeln!(md)?;
    writeln!(
        md,
        "_Cairn adapters reproduce live; upstream reference adapters captured once from gbrain-evals 8dab7f7._"
    )?;

    // JSONL: one row per (adapter, query).
    let jsonl_path = out_dir.join("per-query.jsonl");
    let mut jsonl = File::create(&jsonl_path).context("create per-query.jsonl")?;
    for (adapter, queries) in all_runs {
        for (qid, hits, m) in queries {
            let row = PerQueryRow {
                adapter,
                query_id: qid,
                hits,
                p_at_5: m.p_at_5,
                r_at_5: m.r_at_5,
                mrr: m.mrr,
                ndcg_at_5: m.ndcg_at_5,
            };
            serde_json::to_writer(&mut jsonl, &row)?;
            writeln!(jsonl)?;
        }
    }
    Ok(())
}

/// Average per-query metrics for each cairn adapter into one
/// [`AggregateRow`] per adapter. Empty per-query lists (skipped
/// adapters) collapse to a row of zeroes so the markdown table still
/// has an entry the reader can spot.
fn aggregate_cairn(runs: &[AdapterResults]) -> Vec<AggregateRow> {
    let mut out = Vec::with_capacity(runs.len());
    for (adapter, queries) in runs {
        if queries.is_empty() {
            out.push(AggregateRow {
                adapter: adapter.clone(),
                p_at_5: 0.0,
                r_at_5: 0.0,
                mrr: 0.0,
                ndcg_at_5: 0.0,
                graded_queries: 0,
            });
            continue;
        }
        let mut sum_p = 0.0;
        let mut sum_r = 0.0;
        let mut sum_m = 0.0;
        let mut sum_n = 0.0;
        for (_, _, m) in queries {
            sum_p += m.p_at_5;
            sum_r += m.r_at_5;
            sum_m += m.mrr;
            sum_n += m.ndcg_at_5;
        }
        let n = queries.len() as f64;
        out.push(AggregateRow {
            adapter: adapter.clone(),
            p_at_5: sum_p / n,
            r_at_5: sum_r / n,
            mrr: sum_m / n,
            ndcg_at_5: sum_n / n,
            graded_queries: queries.len(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{AdapterResults, aggregate_cairn, write_report};
    use crate::fixture::{
        AdapterBaseline, AggregateMetrics, Fixture, Page, Query, QueryResult, UpstreamBaseline,
    };
    use crate::metrics::PerQueryMetrics;

    fn synthetic_page(slug: &str) -> Page {
        Page {
            slug: slug.to_owned(),
            title: format!("title-{slug}"),
            body: format!("body for {slug}"),
            facts: serde_json::Value::Null,
        }
    }

    fn synthetic_query(id: &str, relevant: &[&str]) -> Query {
        let relevant: Vec<String> = relevant.iter().map(|s| (*s).to_owned()).collect();
        let grades: BTreeMap<String, u32> = relevant.iter().map(|s| (s.clone(), 1)).collect();
        Query {
            id: id.to_owned(),
            query: format!("q text {id}"),
            relevant,
            grades,
        }
    }

    fn synthetic_metrics(p: f64) -> PerQueryMetrics {
        PerQueryMetrics {
            p_at_5: p,
            r_at_5: p,
            mrr: p,
            ndcg_at_5: p,
        }
    }

    #[test]
    fn writes_report_md_and_jsonl_for_synthetic_runs() {
        let fixture = Fixture {
            pages: vec![synthetic_page("alice"), synthetic_page("bob")],
            queries: vec![synthetic_query("q1", &["alice"])],
            upstream: None,
        };
        let runs: Vec<AdapterResults> = vec![
            (
                "bm25-only".to_owned(),
                vec![(
                    "q1".to_owned(),
                    vec!["alice".to_owned()],
                    synthetic_metrics(0.5),
                )],
            ),
            (
                "vector-bge".to_owned(),
                vec![(
                    "q1".to_owned(),
                    vec!["bob".to_owned(), "alice".to_owned()],
                    synthetic_metrics(0.25),
                )],
            ),
        ];

        let dir = tempfile::tempdir().expect("tempdir");
        write_report(dir.path(), &fixture, &runs).expect("write_report");

        let md = std::fs::read_to_string(dir.path().join("report.md")).expect("read report.md");
        assert!(md.contains("# BrainBench Scorecard"));
        assert!(md.contains("Corpus: 2 pages"));
        assert!(md.contains("Queries: 1 (graded)"));
        assert!(md.contains("`bm25-only`"));
        assert!(md.contains("`vector-bge`"));
        assert!(md.contains("0.500"));
        assert!(md.contains("0.250"));

        let jsonl = std::fs::read_to_string(dir.path().join("per-query.jsonl"))
            .expect("read per-query.jsonl");
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 2, "one JSONL row per (adapter, query)");
        assert!(lines[0].contains("\"adapter\":\"bm25-only\""));
        assert!(lines[0].contains("\"query_id\":\"q1\""));
        assert!(lines[0].contains("\"hits\":[\"alice\"]"));
        assert!(lines[1].contains("\"adapter\":\"vector-bge\""));
        assert!(lines[1].contains("\"hits\":[\"bob\",\"alice\"]"));
    }

    #[test]
    fn upstream_baseline_rows_appear_in_report() {
        let mut adapters = BTreeMap::new();
        adapters.insert(
            "gbrain-grep-only".to_owned(),
            AdapterBaseline {
                aggregate: AggregateMetrics {
                    p_at_5: 0.20,
                    r_at_5: 1.00,
                    mrr: 1.00,
                    ndcg_at_5: 0.85,
                },
                per_query: vec![QueryResult {
                    query_id: "q1".to_owned(),
                    p_at_5: 0.20,
                    r_at_5: 1.00,
                    mrr: 1.00,
                    ndcg_at_5: 0.85,
                }],
            },
        );
        let fixture = Fixture {
            pages: vec![synthetic_page("alice")],
            queries: vec![synthetic_query("q1", &["alice"])],
            upstream: Some(UpstreamBaseline { adapters }),
        };
        let runs: Vec<AdapterResults> = vec![(
            "bm25-only".to_owned(),
            vec![(
                "q1".to_owned(),
                vec!["alice".to_owned()],
                synthetic_metrics(1.0),
            )],
        )];

        let dir = tempfile::tempdir().expect("tempdir");
        write_report(dir.path(), &fixture, &runs).expect("write_report");

        let md = std::fs::read_to_string(dir.path().join("report.md")).expect("read report.md");
        assert!(md.contains("`bm25-only`"));
        assert!(md.contains("`gbrain-grep-only`"));
        assert!(md.contains("0.200"));
        assert!(md.contains("0.850"));
        // Upstream baselines are reference-only; their per_query rows do
        // not show up in per-query.jsonl (which carries cairn live runs).
        let jsonl = std::fs::read_to_string(dir.path().join("per-query.jsonl"))
            .expect("read per-query.jsonl");
        assert_eq!(jsonl.lines().count(), 1);
        assert!(jsonl.contains("\"adapter\":\"bm25-only\""));
    }

    #[test]
    fn aggregate_cairn_handles_empty_queries_as_zero_row() {
        let runs: Vec<AdapterResults> = vec![("hybrid-openai-rrf".to_owned(), Vec::new())];
        let rows = aggregate_cairn(&runs);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.adapter, "hybrid-openai-rrf");
        assert!((row.p_at_5 - 0.0).abs() < f64::EPSILON);
        assert!((row.r_at_5 - 0.0).abs() < f64::EPSILON);
        assert!((row.mrr - 0.0).abs() < f64::EPSILON);
        assert!((row.ndcg_at_5 - 0.0).abs() < f64::EPSILON);
        assert_eq!(row.graded_queries, 0);
    }
}
