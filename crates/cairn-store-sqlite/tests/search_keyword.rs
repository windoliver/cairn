//! End-to-end tests for `MemoryStore::search_keyword` (issue #47, brief §5.1, §8.0.d).
//!
//! Covers the acceptance criteria:
//!
//! - Keyword queries return only visible, in-scope, non-tombstoned records.
//! - Filter combinations are validated and compiled to parameterized SQL.
//! - Ranking input fields are deterministic for a given DB state.
//!
//! Each test seeds a fresh in-memory store so cross-test interference is
//! impossible. The shared `seed` helper writes three records with
//! distinct kinds, tags, and tombstone status — the same fixtures the
//! filter SQL exec tests in `cairn-core` use, adapted to the store
//! schema's `record_json`-driven projection.

use cairn_core::contract::memory_store::{
    Edge, EdgeKind, KeywordCursor, KeywordSearchArgs, MemoryStore, TombstoneReason,
};
use cairn_core::domain::filter::validate_filter;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::domain::{MemoryRecord, RecordId, ScopeTuple, TargetId};
use cairn_core::generated::verbs::search::SearchArgsFilters;
use cairn_store_sqlite::{SqliteMemoryStore, open_in_memory};

/// Build a record from a seed character so the body has stable, distinct
/// FTS5 tokens per row. The seed also drives `id` and `target_id` so two
/// records with the same seed are the same target.
fn record_with(seed: char, body: &str, kind: MemoryKind) -> MemoryRecord {
    let mut r = cairn_core::domain::record::tests_export::sample_record();
    let mut id = String::from("01HQZX9F5N000000000000000");
    id.push(seed.to_ascii_uppercase());
    r.id = RecordId::parse(id.clone()).expect("valid id");
    r.target_id = TargetId::parse(id).expect("valid target");
    body.clone_into(&mut r.body);
    r.kind = kind;
    r
}

async fn seed() -> SqliteMemoryStore {
    let store = open_in_memory().await.expect("open");
    let r1 = record_with('1', "postgres migration strategy notes", MemoryKind::User);
    let r2 = record_with('2', "user prefers dark mode at night", MemoryKind::Feedback);
    let r3 = record_with(
        '3',
        "always use postgres for nightly backups",
        MemoryKind::Rule,
    );
    store.upsert(&r1).await.expect("seed r1");
    store.upsert(&r2).await.expect("seed r2");
    store.upsert(&r3).await.expect("seed r3");
    store
}

/// Default args helper: pin the visibility allowlist to `private` (the
/// fixture record's tier) so the filter never gates the test trivially.
fn args(query: &str, limit: usize) -> KeywordSearchArgs<'static> {
    KeywordSearchArgs {
        query: query.to_owned(),
        filter: None,
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit,
        cursor: None,
    }
}

// ── Acceptance: keyword + freshness/visibility ────────────────────────────────

#[tokio::test]
async fn matches_body_token() {
    let store = seed().await;
    let page = store
        .search_keyword(&args("postgres", 10))
        .await
        .expect("search");
    assert_eq!(page.candidates.len(), 2, "two records mention postgres");
    for c in &page.candidates {
        assert!(
            c.bm25 < 0.0,
            "FTS5 bm25() returns negative for relevant rows"
        );
    }
}

#[tokio::test]
async fn excludes_tombstoned_rows() {
    let store = seed().await;
    let r3_id = RecordId::parse("01HQZX9F5N0000000000000003").expect("valid id");
    store
        .tombstone(&r3_id, TombstoneReason::Forget)
        .await
        .expect("tombstone");
    let page = store
        .search_keyword(&args("postgres", 10))
        .await
        .expect("search");
    assert_eq!(page.candidates.len(), 1, "tombstoned r3 must be excluded");
    assert_eq!(
        page.candidates[0].record_id.as_str(),
        "01HQZX9F5N0000000000000001"
    );
}

#[tokio::test]
async fn visibility_allowlist_filters_results() {
    let store = seed().await;
    let mut a = args("postgres", 10);
    // No visibility tier matches private → empty page.
    a.visibility_allowlist = vec![MemoryVisibility::Public];
    let page = store.search_keyword(&a).await.expect("search");
    assert!(page.candidates.is_empty(), "no public records");
}

#[tokio::test]
async fn empty_visibility_allowlist_means_no_filter() {
    let store = seed().await;
    let mut a = args("postgres", 10);
    a.visibility_allowlist.clear();
    let page = store.search_keyword(&a).await.expect("search");
    assert_eq!(page.candidates.len(), 2);
}

// ── Acceptance: filter compile + parameterized SQL ────────────────────────────

#[tokio::test]
async fn filter_kind_eq_narrows_results() {
    let store = seed().await;
    let raw = serde_json::json!({"field": "kind", "op": "eq", "value": "rule"});
    let parsed: SearchArgsFilters = serde_json::from_value(raw).expect("filter parse");
    let validated = validate_filter(&parsed).expect("filter valid");
    let mut a = args("postgres", 10);
    a.filter = Some(validated);
    let page = store.search_keyword(&a).await.expect("search");
    assert_eq!(page.candidates.len(), 1);
    assert_eq!(page.candidates[0].kind, MemoryKind::Rule);
}

#[tokio::test]
async fn filter_kind_in_widens_then_narrows() {
    let store = seed().await;
    let raw = serde_json::json!({"field": "kind", "op": "in", "value": ["user", "rule"]});
    let parsed: SearchArgsFilters = serde_json::from_value(raw).expect("filter parse");
    let validated = validate_filter(&parsed).expect("filter valid");
    let mut a = args("postgres", 10);
    a.filter = Some(validated);
    let page = store.search_keyword(&a).await.expect("search");
    assert_eq!(page.candidates.len(), 2);
}

#[tokio::test]
async fn filter_tags_array_contains() {
    let store = seed().await;
    // Sample record carries tags = ["pref"], so this matches all three rows.
    let raw = serde_json::json!({"field": "tags", "op": "array_contains", "value": "pref"});
    let parsed: SearchArgsFilters = serde_json::from_value(raw).expect("filter parse");
    let validated = validate_filter(&parsed).expect("filter valid");
    let mut a = args("postgres", 10);
    a.filter = Some(validated);
    let page = store.search_keyword(&a).await.expect("search");
    assert_eq!(page.candidates.len(), 2);
}

#[tokio::test]
async fn filter_value_with_sql_metacharacters_is_parameterized() {
    let store = seed().await;
    // A value containing a single quote and a SQL keyword would break the
    // query if interpolated; compile_filter binds it as a parameter, so
    // the search just returns no rows.
    let raw = serde_json::json!({
        "field": "kind",
        "op": "eq",
        "value": "rule'; DROP TABLE records; --"
    });
    let parsed: SearchArgsFilters = serde_json::from_value(raw).expect("filter parse");
    let validated = validate_filter(&parsed).expect("filter valid");
    let mut a = args("postgres", 10);
    a.filter = Some(validated);
    let page = store.search_keyword(&a).await.expect("search");
    assert!(page.candidates.is_empty());

    // Records table must still be present and queryable.
    let probe = store
        .search_keyword(&args("postgres", 10))
        .await
        .expect("search after attempted injection");
    assert_eq!(probe.candidates.len(), 2);
}

#[tokio::test]
async fn filter_is_static_false_matches_default_records() {
    let store = seed().await;
    // The projection writes is_static = 0 for every record, so this
    // filter must match every keyword hit. Pre-fix this regressed
    // because field_col routed `is_static` through extra_frontmatter,
    // which the projection never populates.
    let raw = serde_json::json!({"field": "is_static", "op": "eq", "value": false});
    let parsed: SearchArgsFilters = serde_json::from_value(raw).expect("filter parse");
    let validated = validate_filter(&parsed).expect("filter valid");
    let mut a = args("postgres", 10);
    a.filter = Some(validated);
    let page = store.search_keyword(&a).await.expect("search");
    assert_eq!(page.candidates.len(), 2);
}

#[tokio::test]
async fn filter_active_true_matches_live_rows() {
    let store = seed().await;
    // `active=1` is the floor for search; an explicit `active=true`
    // filter should match every keyword hit. Regression guard for the
    // physical-column routing in `field_col`.
    let raw = serde_json::json!({"field": "active", "op": "eq", "value": true});
    let parsed: SearchArgsFilters = serde_json::from_value(raw).expect("filter parse");
    let validated = validate_filter(&parsed).expect("filter valid");
    let mut a = args("postgres", 10);
    a.filter = Some(validated);
    let page = store.search_keyword(&a).await.expect("search");
    assert_eq!(page.candidates.len(), 2);
}

#[tokio::test]
async fn filter_path_string_contains_narrows_by_projected_path() {
    let store = seed().await;
    // `path` is set by `projection::derive_path` to
    // `vault/<scope>/<id>.md` — every test record carries one.
    let raw = serde_json::json!({
        "field": "path",
        "op": "string_contains",
        "value": "vault/",
    });
    let parsed: SearchArgsFilters = serde_json::from_value(raw).expect("filter parse");
    let validated = validate_filter(&parsed).expect("filter valid");
    let mut a = args("postgres", 10);
    a.filter = Some(validated);
    let page = store.search_keyword(&a).await.expect("search");
    assert_eq!(
        page.candidates.len(),
        2,
        "all projected paths share `vault/`"
    );
}

// ── Supersession: `updates`-edge dst rows are hidden from search ─────────────

#[tokio::test]
async fn updates_edge_dst_is_hidden_from_search() {
    let store = open_in_memory().await.expect("open");
    // Two records on distinct target_ids; same body so both match the
    // FTS query. Then declare r2 supersedes r1 via an `updates` edge.
    let r1 = record_with('1', "shared body keyword needle", MemoryKind::Fact);
    let r2 = record_with('2', "shared body keyword needle", MemoryKind::Fact);
    store.upsert(&r1).await.expect("r1");
    store.upsert(&r2).await.expect("r2");
    let r1_id = RecordId::parse("01HQZX9F5N0000000000000001").expect("valid");
    let r2_id = RecordId::parse("01HQZX9F5N0000000000000002").expect("valid");
    store
        .put_edge(&Edge {
            src: r2_id.clone(),
            dst: r1_id.clone(),
            kind: EdgeKind::Updates,
            weight: None,
        })
        .await
        .expect("supersede");

    // r1 is the dst of an `updates` edge → hidden by `records_latest`
    // semantics. Search must respect the same exclusion.
    let page = store
        .search_keyword(&args("needle", 10))
        .await
        .expect("search");
    assert_eq!(page.candidates.len(), 1, "superseded record stays hidden");
    assert_eq!(page.candidates[0].record_id, r2_id);
}

// ── FTS column-filter syntax surfaces as FtsQuery, not generic SQL ───────────

#[tokio::test]
async fn fts_column_filter_against_unindexed_column_is_typed() {
    // `records_fts` only indexes `body`, so `title:postgres` triggers
    // FTS5's column-filter parser to error with `no such column: title`.
    // The verb layer needs that surfaced as `StoreError::FtsQuery` so it
    // can return an actionable error instead of a generic SQL failure.
    let store = seed().await;
    let result = store.search_keyword(&args("title:postgres", 10)).await;
    let err = result.expect_err("malformed FTS column filter must error");
    let msg = err.to_string();
    assert!(
        msg.contains("FTS5 query parse error"),
        "expected typed FtsQuery error, got: {msg}",
    );
}

// ── Cross-scope isolation via the scope_* filter fields ──────────────────────

#[tokio::test]
async fn cross_scope_records_isolate_via_scope_user_filter() {
    let store = open_in_memory().await.expect("open");
    // Two records, same body and visibility, two distinct `scope.user`
    // identities. Pre-fix this would have leaked rows across scopes
    // because the filter DSL had no `scope_*` predicates.
    let mut alice = record_with('A', "shared body keyword needle", MemoryKind::User);
    alice.scope = ScopeTuple {
        user: Some("usr:alice".to_owned()),
        ..ScopeTuple::default()
    };
    let mut bob = record_with('B', "shared body keyword needle", MemoryKind::User);
    bob.scope = ScopeTuple {
        user: Some("usr:bob".to_owned()),
        ..ScopeTuple::default()
    };
    store.upsert(&alice).await.expect("alice");
    store.upsert(&bob).await.expect("bob");

    // Filter that only Alice's scope passes.
    let raw = serde_json::json!({
        "field": "scope_user",
        "op": "eq",
        "value": "usr:alice",
    });
    let parsed: SearchArgsFilters = serde_json::from_value(raw).expect("filter parse");
    let validated = validate_filter(&parsed).expect("filter valid");
    let mut a = args("needle", 10);
    a.filter = Some(validated);
    let page = store.search_keyword(&a).await.expect("search");
    assert_eq!(page.candidates.len(), 1, "scope_user filter must isolate");
    let scope = &page.candidates[0].scope;
    assert_eq!(scope.user.as_deref(), Some("usr:alice"));
}

#[tokio::test]
async fn omitting_scope_filter_returns_every_scope() {
    // Companion to `cross_scope_records_isolate_via_scope_user_filter`:
    // pins the documented invariant that the store does NOT enforce
    // scope on its own. A caller that forgets to add a scope predicate
    // gets every matching row — this is the boundary the verb layer in
    // #62 is responsible for closing in production.
    let store = open_in_memory().await.expect("open");
    let mut alice = record_with('A', "shared body keyword needle", MemoryKind::User);
    alice.scope = ScopeTuple {
        user: Some("usr:alice".to_owned()),
        ..ScopeTuple::default()
    };
    let mut bob = record_with('B', "shared body keyword needle", MemoryKind::User);
    bob.scope = ScopeTuple {
        user: Some("usr:bob".to_owned()),
        ..ScopeTuple::default()
    };
    store.upsert(&alice).await.expect("alice");
    store.upsert(&bob).await.expect("bob");

    let page = store
        .search_keyword(&args("needle", 10))
        .await
        .expect("search");
    assert_eq!(
        page.candidates.len(),
        2,
        "without a scope_* filter both rows surface — verb layer must add one",
    );
}

// ── Acceptance: deterministic ranking inputs ──────────────────────────────────

#[tokio::test]
async fn ranking_inputs_are_populated() {
    let store = seed().await;
    let page = store
        .search_keyword(&args("postgres", 10))
        .await
        .expect("search");
    for c in &page.candidates {
        // FTS5 BM25 is negative for relevant rows.
        assert!(c.bm25 < 0.0);
        // Recency / staleness derive from epoch deltas — never negative.
        assert!(c.recency_seconds >= 0);
        assert!(c.staleness_seconds >= 0);
        // Confidence/salience were 0.7 / 0.5 in the sample record.
        assert!((0.0..=1.0).contains(&c.confidence));
        assert!((0.0..=1.0).contains(&c.salience));
        assert!(!c.snippet.is_empty(), "snippet always set for FTS hits");
        assert!(!c.record_json.is_empty(), "record_json always populated");
    }
    // Ordering: ascending bm25 (best first) with record_id tiebreak.
    let bm25s: Vec<f64> = page.candidates.iter().map(|c| c.bm25).collect();
    let mut sorted = bm25s.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("non-NaN bm25"));
    assert_eq!(bm25s, sorted, "results must be sorted by bm25 ASC");
}

#[tokio::test]
async fn pagination_via_cursor_walks_pages() {
    let store = seed().await;
    let page1 = store
        .search_keyword(&args("postgres", 1))
        .await
        .expect("page1");
    assert_eq!(page1.candidates.len(), 1);
    assert!(page1.next_cursor.is_some());

    let mut a = args("postgres", 1);
    a.cursor = page1.next_cursor.clone();
    let page2 = store.search_keyword(&a).await.expect("page2");
    assert_eq!(page2.candidates.len(), 1);
    assert_ne!(page1.candidates[0].record_id, page2.candidates[0].record_id);
    // Final page should drain without a cursor.
    assert!(page2.next_cursor.is_none());
}

#[tokio::test]
async fn cursor_skips_already_returned_rows() {
    let store = seed().await;
    // Build a synthetic cursor at the absolute floor — every row should
    // appear after it.
    let cursor = KeywordCursor {
        bm25: f64::MIN,
        record_id: RecordId::parse("01HQZX9F5N0000000000000000").expect("valid"),
    };
    let mut a = args("postgres", 10);
    a.cursor = Some(cursor);
    let page = store.search_keyword(&a).await.expect("search");
    assert_eq!(page.candidates.len(), 2);
}

// ── FTS5 query parse error mapping ────────────────────────────────────────────

#[tokio::test]
async fn malformed_fts_query_surfaces_typed_error() {
    let store = seed().await;
    // Unbalanced quote: FTS5 raises a syntax error.
    let result = store.search_keyword(&args("postgres \"open", 10)).await;
    let err = result.expect_err("malformed query must error");
    let msg = err.to_string();
    assert!(
        msg.contains("FTS5 query parse error") || msg.to_lowercase().contains("fts5"),
        "expected FtsQuery error, got: {msg}",
    );
}

// ── No-match guard ────────────────────────────────────────────────────────────

#[tokio::test]
async fn unmatched_query_returns_empty_page() {
    let store = seed().await;
    let page = store
        .search_keyword(&args("zzz_nothing_matches_this_token_xyz", 10))
        .await
        .expect("search");
    assert!(page.candidates.is_empty());
    assert!(page.next_cursor.is_none());
}
