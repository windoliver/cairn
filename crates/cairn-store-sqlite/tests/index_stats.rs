//! Integration test for `MemoryStore::index_stats`.
//!
//! Verifies that active-record count and FTS5 row count both match the number
//! of records inserted into a fresh in-memory store.

use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::RecordId;
use cairn_core::domain::TargetId;
use cairn_store_sqlite::open_in_memory;

fn base() -> cairn_core::domain::MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

#[tokio::test]
async fn index_stats_matches_active_record_count() {
    let store = open_in_memory().await.expect("open in-memory store");

    let r1 = base();
    let mut r2 = base();
    r2.id = RecordId::parse("01HQZX9F5N0000000000000099").expect("valid ulid");
    r2.target_id = TargetId::parse("01HQZX9F5N0000000000000099").expect("valid ulid for target_id");

    store.upsert(&r1).await.expect("upsert r1");
    store.upsert(&r2).await.expect("upsert r2");

    let stats = store.index_stats().await.expect("index_stats");
    assert_eq!(stats.records_active, 2, "expected 2 active records");
    assert_eq!(stats.fts5_rows, 2, "expected 2 FTS5 rows");
}
