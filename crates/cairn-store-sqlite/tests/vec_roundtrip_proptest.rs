//! Property tests: f32 vector round-trip precision + `drain_once` idempotence.
//!
//! Each drain idempotence iteration spins up a fresh tokio current-thread
//! runtime to drive the async store API from inside the synchronous proptest
//! body — same approach as `hot_columns_match_json.rs`.

use proptest::prelude::*;

proptest! {
    #[test]
    fn vec384_round_trips_within_tolerance(v in prop::collection::vec(
        prop::num::f32::NORMAL,
        384..=384usize
    )) {
        // Encode as LE bytes then decode — should be bit-exact.
        let bytes: Vec<u8> = v.iter().flat_map(|&f| f.to_le_bytes()).collect();
        let decoded: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        for (orig, dec) in v.iter().zip(decoded.iter()) {
            prop_assert!(
                (orig - dec).abs() < 1e-7_f32,
                "precision lost: {orig} → {dec}"
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn drain_once_idempotent_for_same_record(body in "[a-z ]{1,200}") {
        // Running drain_once twice for the same pending row produces the
        // same record_vectors bytes (deterministic MockEmbedder).
        //
        // We open the store WITHOUT an embedder to suppress embed-on-write;
        // the record is upserted bare, then manually queued, and drain_once
        // is called explicitly twice with a MockEmbedder.
        use std::sync::Arc;
        use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};
        use cairn_store_sqlite::{drain_once, open_in_memory_with_embedder};
        use cairn_core::contract::memory_store::MemoryStore;
        use cairn_test_fixtures::sample_record;

        let rt = tokio::runtime::Runtime::new().expect("runtime");

        rt.block_on(async {
            // Open without embedder: no auto-embed on upsert.
            let store = open_in_memory_with_embedder(None).await.expect("open");
            let mut r = sample_record(0);
            r.body = body.clone();
            let outcome = store.upsert(&r).await.expect("upsert");
            let rid = outcome.record_id.as_str().to_owned();

            let conn = Arc::clone(store.raw_conn().expect("conn"));
            let emb: Arc<dyn EmbeddingModel> =
                Arc::new(MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5));

            // Manually enqueue the record into pending_embeddings.
            let rid1 = rid.clone();
            conn.call(move |c| {
                c.execute(
                    "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
                       VALUES (?, 'opt_in_backfill', 0, 0)",
                    rusqlite::params![rid1],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await
            .expect("enqueue 1");

            // First drain: embeds the record, writes vector row.
            drain_once(Arc::clone(&conn), Arc::clone(&emb))
                .await
                .expect("drain 1");

            let rid2 = rid.clone();
            let bytes1: Vec<u8> = conn
                .call(move |c| {
                    c.query_row(
                        "SELECT embedding FROM record_vectors WHERE record_id = ?",
                        rusqlite::params![rid2],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
                })
                .await
                .expect("read bytes1");

            // Re-enqueue the same record (simulate a retry / re-indexing request).
            let rid3 = rid.clone();
            conn.call(move |c| {
                c.execute(
                    "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
                       VALUES (?, 'opt_in_backfill', 0, 0)
                       ON CONFLICT(record_id) DO UPDATE SET attempt_count = 0",
                    rusqlite::params![rid3],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await
            .expect("re-enqueue");

            // Second drain: should overwrite with identical bytes.
            drain_once(Arc::clone(&conn), Arc::clone(&emb))
                .await
                .expect("drain 2");

            let rid4 = rid.clone();
            let bytes2: Vec<u8> = conn
                .call(move |c| {
                    c.query_row(
                        "SELECT embedding FROM record_vectors WHERE record_id = ?",
                        rusqlite::params![rid4],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
                })
                .await
                .expect("read bytes2");

            prop_assert_eq!(
                bytes1,
                bytes2,
                "drain_once must be idempotent: same body+model → same bytes"
            );
            Ok(())
        })
        .expect("proptest body");
    }
}
