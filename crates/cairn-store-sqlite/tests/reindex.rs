//! Reindex drain tests.

use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore;
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};

use cairn_store_sqlite::{drain_once, open_in_memory};
use cairn_test_fixtures::sample_record;

#[tokio::test]
async fn drain_once_embeds_pending_record() {
    let embedder: Arc<dyn EmbeddingModel> =
        Arc::new(MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5));

    // Open WITHOUT embedder so upsert doesn't embed — simulates opt-in-backfill.
    let store = open_in_memory().await.unwrap();
    let r = sample_record(1);
    let outcome = store.upsert(&r).await.unwrap();
    let rid = outcome.record_id.as_str().to_owned();

    // Manually enqueue in pending_embeddings.
    let conn = store.raw_conn().unwrap().clone();
    let rid2 = rid.clone();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
               VALUES (?, 'opt_in_backfill', 0, strftime('%s','now'))",
            rusqlite::params![rid2],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .unwrap();

    // Run drain_once.
    let stats = drain_once(conn.clone(), Arc::clone(&embedder))
        .await
        .unwrap();
    assert_eq!(stats.drained, 1, "should drain one row");
    assert_eq!(stats.failed, 0);
    assert_eq!(stats.remaining, 0);

    // Vector row must now exist.
    let rid3 = rid.clone();
    let count: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?",
                rusqlite::params![rid3],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(count, 1, "record_vectors should have a row after drain");
}

#[tokio::test]
async fn drain_once_bumps_attempt_count_on_failure() {
    use cairn_embeddings_local::error::EmbeddingError;

    struct AlwaysFail;
    impl EmbeddingModel for AlwaysFail {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::BgeSmallEnV1_5
        }
        fn dim(&self) -> usize {
            384
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::Tokenizer("fail".into()))
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::Tokenizer("fail".into()))
        }
    }

    let store = open_in_memory().await.unwrap();
    let r = sample_record(2);
    let outcome = store.upsert(&r).await.unwrap();
    let rid = outcome.record_id.as_str().to_owned();

    let conn = store.raw_conn().unwrap().clone();
    let rid2 = rid.clone();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
               VALUES (?, 'embed_failed', 0, strftime('%s','now'))",
            rusqlite::params![rid2],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .unwrap();

    let stats = drain_once(
        conn.clone(),
        Arc::new(AlwaysFail) as Arc<dyn EmbeddingModel>,
    )
    .await
    .unwrap();

    assert_eq!(stats.failed, 1);
    assert_eq!(stats.drained, 0);

    let rid3 = rid.clone();
    let attempt: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT attempt_count FROM pending_embeddings WHERE record_id = ?",
                rusqlite::params![rid3],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(attempt, 1, "attempt_count must be bumped");
}

#[tokio::test]
async fn drain_once_skips_poison_pill_rows() {
    use cairn_embeddings_local::error::EmbeddingError;

    struct AlwaysFail;
    impl EmbeddingModel for AlwaysFail {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::BgeSmallEnV1_5
        }
        fn dim(&self) -> usize {
            384
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::Tokenizer("fail".into()))
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::Tokenizer("fail".into()))
        }
    }

    let store = open_in_memory().await.unwrap();
    let r = sample_record(3);
    let outcome = store.upsert(&r).await.unwrap();
    let rid = outcome.record_id.as_str().to_owned();

    let conn = store.raw_conn().unwrap().clone();
    let rid2 = rid.clone();
    conn.call(move |c| {
        // attempt_count = 6 > max 5 — should be skipped
        c.execute(
            "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
               VALUES (?, 'embed_failed', 6, strftime('%s','now'))",
            rusqlite::params![rid2],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .unwrap();

    let stats = drain_once(
        conn.clone(),
        Arc::new(AlwaysFail) as Arc<dyn EmbeddingModel>,
    )
    .await
    .unwrap();

    assert_eq!(stats.drained, 0);
    assert_eq!(stats.failed, 0);
    // Row still there, untouched.
    let rid3 = rid.clone();
    let still: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM pending_embeddings WHERE record_id = ?",
                rusqlite::params![rid3],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(still, 1, "poison-pill row must remain");
}
