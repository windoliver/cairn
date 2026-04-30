//! Verify embed-on-write behaviour in upsert.

use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore;
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};

use cairn_store_sqlite::open_in_memory_with_embedder;
use cairn_test_fixtures::sample_record;

#[tokio::test]
async fn upsert_with_embedder_writes_vector_row() {
    let embedder: Arc<dyn EmbeddingModel> =
        Arc::new(MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .unwrap();
    let r = sample_record(1);
    let outcome = store.upsert(&r).await.unwrap();
    assert!(outcome.content_changed);

    let rid = outcome.record_id.as_str().to_owned();
    let conn = store.raw_conn().unwrap().clone();
    let found: bool = conn
        .call(move |c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?",
                rusqlite::params![rid],
                |row| row.get(0),
            )?;
            Ok::<_, tokio_rusqlite::Error>(n > 0)
        })
        .await
        .unwrap();
    assert!(
        found,
        "record_vectors must have a row after upsert with embedder"
    );
}

#[tokio::test]
async fn upsert_without_embedder_no_vector_row() {
    let store = open_in_memory_with_embedder(None).await.unwrap();
    let r = sample_record(2);
    let outcome = store.upsert(&r).await.unwrap();

    let rid = outcome.record_id.as_str().to_owned();
    let conn = store.raw_conn().unwrap().clone();
    let count: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?",
                rusqlite::params![rid],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(count, 0, "no vector row when no embedder");
}

#[tokio::test]
async fn upsert_embed_failure_queues_pending() {
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
            Err(EmbeddingError::Tokenizer("simulated failure".into()))
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::Tokenizer("simulated failure".into()))
        }
    }

    let embedder: Arc<dyn EmbeddingModel> = Arc::new(AlwaysFail);
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .unwrap();
    let r = sample_record(3);
    // Upsert must SUCCEED even when embedding fails.
    let outcome = store.upsert(&r).await.unwrap();
    assert!(outcome.content_changed);

    let rid = outcome.record_id.as_str().to_owned();
    let conn = store.raw_conn().unwrap().clone();
    // Vector row must NOT exist.
    let vec_count: i64 = conn
        .call({
            let rid = rid.clone();
            move |c| {
                c.query_row(
                    "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?",
                    rusqlite::params![rid],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            }
        })
        .await
        .unwrap();
    assert_eq!(vec_count, 0, "no vector row when embed failed");

    // Pending embeddings row MUST exist.
    let pending_count: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM pending_embeddings WHERE record_id = ?",
                rusqlite::params![rid],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(
        pending_count, 1,
        "pending_embeddings must have a row when embed failed"
    );
}
