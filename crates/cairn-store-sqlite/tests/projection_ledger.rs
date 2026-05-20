#![allow(missing_docs)]

use std::path::Path;

use cairn_core::{
    contract::memory_store::{
        Bm25sPreference, MemoryStore, ProjectionApplyItem, RankingSignalName, SearchMode,
        SearchRequest,
    },
    domain::{
        projection::{
            ProjectionCursor, ProjectionItemState, ProjectionLedgerRow, ProjectionTarget,
        },
        record::RecordId,
    },
};
use cairn_store_sqlite::SqliteMemoryStore;

fn record_id(raw: &str) -> RecordId {
    RecordId::parse(raw).expect("valid test ULID")
}

fn open_store(path: &Path) -> SqliteMemoryStore {
    SqliteMemoryStore::open(path).expect("open sqlite store")
}

#[tokio::test]
async fn sqlite_search_returns_fts_signal_without_projection_mutation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(&dir.path().join("cairn.db"));
    store
        .insert_test_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "projection search keeps sqlite authoritative",
            7,
            "sha256:record-a",
        )
        .expect("insert test record");

    let response = store
        .search(SearchRequest {
            query: "projection".to_owned(),
            mode: SearchMode::Keyword,
            limit: 10,
            bm25s: Bm25sPreference::Disabled,
        })
        .await
        .expect("search");

    assert_eq!(response.hits.len(), 1);
    assert_eq!(
        response.hits[0].record_id.as_str(),
        "01ARZ3NDEKTSV4RRFFQ69G5FAV"
    );
    assert_eq!(
        response.hits[0].ranking_signals[0].name,
        RankingSignalName::SqliteFts5
    );
}

#[tokio::test]
async fn projection_ledger_counts_stale_and_current_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(&dir.path().join("cairn.db"));
    store
        .insert_test_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "first record",
            1,
            "sha256:record-a",
        )
        .expect("insert first");
    store
        .insert_test_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAW",
            "second record",
            2,
            "sha256:record-b",
        )
        .expect("insert second");

    store
        .apply_projection_items(vec![ProjectionApplyItem {
            row: ProjectionLedgerRow {
                target: ProjectionTarget::Bm25sLexical,
                cursor: ProjectionCursor {
                    record_id: record_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
                    wal_sequence: 1,
                    record_hash: "sha256:record-a".to_owned(),
                    source_hash: None,
                },
                state: ProjectionItemState::Current,
                updated_at: "2026-05-19T12:00:00Z".to_owned(),
            },
        }])
        .await
        .expect("apply projection");

    let summaries = store.projection_summaries().await.expect("summaries");
    let bm25s = summaries
        .iter()
        .find(|summary| summary.target == ProjectionTarget::Bm25sLexical)
        .expect("bm25s summary");
    assert_eq!(bm25s.total_authoritative_items, 2);
    assert_eq!(bm25s.current_items, 1);
    assert_eq!(bm25s.lagging_items, 1);
}

#[tokio::test]
async fn projection_summary_ignores_superseded_record_hash_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(&dir.path().join("cairn.db"));
    store
        .insert_test_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "updated record",
            2,
            "sha256:record-b",
        )
        .expect("insert current record");

    store
        .apply_projection_items(vec![
            ProjectionApplyItem {
                row: ProjectionLedgerRow {
                    target: ProjectionTarget::Bm25sLexical,
                    cursor: ProjectionCursor {
                        record_id: record_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
                        wal_sequence: 1,
                        record_hash: "sha256:record-a".to_owned(),
                        source_hash: None,
                    },
                    state: ProjectionItemState::Current,
                    updated_at: "2026-05-19T11:00:00Z".to_owned(),
                },
            },
            ProjectionApplyItem {
                row: ProjectionLedgerRow {
                    target: ProjectionTarget::Bm25sLexical,
                    cursor: ProjectionCursor {
                        record_id: record_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
                        wal_sequence: 2,
                        record_hash: "sha256:record-b".to_owned(),
                        source_hash: None,
                    },
                    state: ProjectionItemState::Current,
                    updated_at: "2026-05-19T12:00:00Z".to_owned(),
                },
            },
        ])
        .await
        .expect("apply projection rows");

    let summaries = store.projection_summaries().await.expect("summaries");
    let bm25s = summaries
        .iter()
        .find(|summary| summary.target == ProjectionTarget::Bm25sLexical)
        .expect("bm25s summary");

    assert_eq!(bm25s.total_authoritative_items, 1);
    assert_eq!(bm25s.current_items, 1);
    assert_eq!(bm25s.lagging_items, 0);
}
