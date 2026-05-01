//! End-to-end smoke test: real on-disk `cairn.db`, ingest via the
//! `MemoryStore` trait, search via `search_keyword`, reopen, search again.
//!
//! Sits one layer below the CLI envelope (the `cairn search` verb is a
//! stub at this commit; verb dispatch lands in #62 / epic #9). Pinning
//! the trait-level e2e here means that when verb dispatch wires through,
//! the only delta is the envelope layer — the store path is already
//! covered by this test.
//!
//! Why not in-memory: this test deliberately exercises `open(path)` so
//! the migration set, FTS5 triggers, and PRAGMA chain run against a
//! disk-backed file the same way a real `cairn.db` would. Reopen
//! verifies persistence + idempotent migration replay.

use cairn_core::contract::memory_store::{KeywordSearchArgs, MemoryStore};
use cairn_core::domain::filter::validate_filter;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use cairn_core::generated::verbs::search::SearchArgsFilters;
use tempfile::TempDir;

fn record(seed: char, body: &str, kind: MemoryKind) -> MemoryRecord {
    let mut r = cairn_core::domain::record::tests_export::sample_record();
    let mut id = String::from("01HQZX9F5N000000000000000");
    id.push(seed.to_ascii_uppercase());
    r.id = RecordId::parse(id.clone()).expect("valid id");
    r.target_id = TargetId::parse(id).expect("valid target");
    body.clone_into(&mut r.body);
    r.kind = kind;
    r
}

fn args(query: &str, limit: usize) -> KeywordSearchArgs<'static> {
    KeywordSearchArgs {
        query: query.to_owned(),
        filter: None,
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit,
        cursor: None,
    }
}

#[tokio::test]
async fn ingest_then_search_round_trips_through_real_file() {
    let dir = TempDir::new().expect("tempdir");
    let db = dir.path().join("cairn.db");

    // ── Phase 1: ingest ────────────────────────────────────────────────────
    {
        let store = cairn_store_sqlite::open(&db).await.expect("open #1");
        store
            .upsert(&record(
                '1',
                "postgres migration playbook drafted by team",
                MemoryKind::Playbook,
            ))
            .await
            .expect("upsert r1");
        store
            .upsert(&record(
                '2',
                "postgres backup retention policy",
                MemoryKind::Rule,
            ))
            .await
            .expect("upsert r2");
        store
            .upsert(&record('3', "user prefers dark mode", MemoryKind::User))
            .await
            .expect("upsert r3");
    } // store dropped → DB closed

    // ── Phase 2: reopen + search ──────────────────────────────────────────
    let store = cairn_store_sqlite::open(&db).await.expect("reopen");
    let page = store
        .search_keyword(&args("postgres", 10))
        .await
        .expect("search after reopen");
    assert_eq!(
        page.candidates.len(),
        2,
        "two records persist across reopen and match `postgres`",
    );
    for c in &page.candidates {
        assert!(c.bm25 < 0.0, "FTS5 bm25 negative for relevant rows");
        assert!(!c.snippet.is_empty(), "snippet always set");
        assert!(!c.record_json.is_empty(), "record_json hydrated");
    }

    // ── Phase 3: filter narrows post-reopen ────────────────────────────────
    let raw_filter = serde_json::json!({"field": "kind", "op": "eq", "value": "rule"});
    let parsed: SearchArgsFilters = serde_json::from_value(raw_filter).expect("parse");
    let validated = validate_filter(&parsed).expect("validate");
    let mut a = args("postgres", 10);
    a.filter = Some(validated);
    let page = store.search_keyword(&a).await.expect("filtered search");
    assert_eq!(page.candidates.len(), 1);
    assert_eq!(page.candidates[0].kind, MemoryKind::Rule);

    // ── Phase 4: capability advertised on the real adapter ────────────────
    assert!(
        store.capabilities().fts,
        "fts capability flag must be true after #47",
    );
}
