//! Destructive-fixture integration tests for `rebuild_from_db`.
//!
//! Covers:
//!   1. Both FTS5 and vector indexes are restored after deliberate destruction.
//!   2. `rebuild_from_db` is idempotent: calling it twice yields identical
//!      `RebuildStats`.

use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};
use cairn_store_sqlite::{drain_once, open_in_memory_with_embedder, rebuild_from_db};

// ---------------------------------------------------------------------------
// Shared fixture
// ---------------------------------------------------------------------------

fn make_three_records() -> Vec<cairn_core::domain::record::MemoryRecord> {
    let mut a = cairn_core::domain::record::tests_export::sample_record();
    "alice chen worked at novapay".clone_into(&mut a.body);

    let mut b = cairn_core::domain::record::tests_export::sample_record();
    b.id = cairn_core::domain::RecordId::parse("01HQZX9F5N00000000000000B1".to_owned())
        .expect("valid rid b");
    b.target_id = cairn_core::domain::TargetId::parse("01HQZX9F5N00000000000000B2".to_owned())
        .expect("valid tid b");
    "carol nakamura runs mindbridge".clone_into(&mut b.body);

    let mut c = cairn_core::domain::record::tests_export::sample_record();
    c.id = cairn_core::domain::RecordId::parse("01HQZX9F5N00000000000000C1".to_owned())
        .expect("valid rid c");
    c.target_id = cairn_core::domain::TargetId::parse("01HQZX9F5N00000000000000C2".to_owned())
        .expect("valid tid c");
    "dave evergreen tracks system metrics".clone_into(&mut c.body);

    vec![a, b, c]
}

// ---------------------------------------------------------------------------
// Test 1: both indexes are restored after deliberate destruction
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_from_db_restores_both_indexes() {
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store");

    // Seed 3 records via embed-on-write path.
    for r in &make_three_records() {
        store.upsert(r).await.expect("upsert");
    }

    let conn = store.raw_conn_for_admin().expect("admin conn").clone();

    // Drive the background drain until the pending queue is empty so that
    // record_vectors is fully populated before we destroy it.
    for _ in 0..100 {
        let stats = drain_once(Arc::clone(&conn), Arc::clone(&embedder))
            .await
            .expect("drain");
        if stats.remaining == 0 {
            break;
        }
    }

    // Sanity: both indexes are populated before destruction.
    let fts_before: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM records_fts", [], |r| r.get(0))
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("count fts before");
    let vec_before: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM record_vectors", [], |r| r.get(0))
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("count vec before");
    assert_eq!(fts_before, 3, "FTS5 should have 3 rows before destruction");
    assert!(
        vec_before >= 3,
        "record_vectors should have ≥ 3 rows before destruction, got {vec_before}",
    );

    // Deliberate destruction of both indexes.
    conn.call(|c| {
        c.execute("DELETE FROM records_fts", [])?;
        c.execute("DELETE FROM record_vectors", [])?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("destroy indexes");

    // Verify indexes are truly empty after destruction.
    let fts_destroyed: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM records_fts", [], |r| r.get(0))
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("count fts after destroy");
    assert_eq!(fts_destroyed, 0, "FTS5 must be empty after DELETE");

    // Rebuild FTS5 + re-enqueue for vectors.
    let stats = rebuild_from_db(Arc::clone(&conn))
        .await
        .expect("rebuild_from_db");
    assert_eq!(stats.fts_rebuilt, 3, "rebuild must restore 3 FTS rows");
    assert_eq!(
        stats.enqueued, 3,
        "rebuild must enqueue 3 records for re-embedding"
    );

    // Drive drain again to refill record_vectors.
    for _ in 0..1000 {
        let s = drain_once(Arc::clone(&conn), Arc::clone(&embedder))
            .await
            .expect("drain after rebuild");
        if s.remaining == 0 {
            break;
        }
    }

    let fts_after: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM records_fts", [], |r| r.get(0))
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("count fts after rebuild");
    let vec_after: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM record_vectors", [], |r| r.get(0))
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("count vec after rebuild");

    assert_eq!(fts_after, 3, "FTS5 must have 3 rows after rebuild");
    assert!(
        vec_after >= 3,
        "record_vectors must have ≥ 3 rows after drain, got {vec_after}",
    );
}

// ---------------------------------------------------------------------------
// Test 2: rebuild_from_db is idempotent
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_from_db_is_idempotent() {
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store");

    for r in &make_three_records() {
        store.upsert(r).await.expect("upsert");
    }

    let conn = store.raw_conn_for_admin().expect("admin conn").clone();

    let s1 = rebuild_from_db(Arc::clone(&conn)).await.expect("rebuild 1");
    let s2 = rebuild_from_db(Arc::clone(&conn)).await.expect("rebuild 2");

    assert_eq!(
        s1, s2,
        "RebuildStats from two consecutive rebuilds must be identical",
    );
}
