//! End-to-end fault-injection tests for hybrid partial-result behavior
//! (issue #191, round-4 fix).
//!
//! These tests open a real fixture vault, break a downstream dependency
//! mid-flight via the admin connection, and assert that hybrid search
//! still returns the surviving leg's candidates plus a `DegradedLeg`
//! entry — instead of hard-failing the whole request.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::{HybridSearchArgs, MemoryStore};
use cairn_core::domain::ScopeTuple;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{RecordId, TargetId};
use cairn_core::search::{DegradationReason, DegradedLeg};
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};
use cairn_store_sqlite::{SqliteMemoryStore, drain_once, open_in_memory_with_embedder};

fn record(rid: &str, body: &str) -> cairn_core::domain::MemoryRecord {
    let mut r = sample_record();
    r.id = RecordId::parse(rid.to_owned()).expect("valid record id");
    r.target_id = TargetId::parse(rid.to_owned()).expect("valid target id");
    r.scope = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    body.clone_into(&mut r.body);
    r
}

async fn drain_all(store: &SqliteMemoryStore, embedder: Arc<dyn EmbeddingModel>) {
    let conn = store.raw_conn_for_admin().expect("conn").clone();
    for _ in 0..1000 {
        let stats = drain_once(Arc::clone(&conn), Arc::clone(&embedder))
            .await
            .expect("drain");
        if stats.remaining == 0 {
            break;
        }
    }
}

fn hybrid_args(query: &str, model_label: &str) -> HybridSearchArgs<'static> {
    HybridSearchArgs {
        query: query.to_owned(),
        filter: None,
        auth_scope: ScopeTuple {
            tenant: Some("acme".into()),
            ..Default::default()
        },
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 10,
        model_label: model_label.to_owned(),
        blend: 0.5,
        rrf_k: 60,
        rerank_topk: 20,
        with_explain: false,
        confidence_floor: 1e-3,
    }
}

/// Healthy baseline: hybrid returns candidates and an empty
/// `degraded_legs`. Locks in that the test harness itself isn't
/// silently producing degradations the fixture would mask.
#[tokio::test]
async fn hybrid_baseline_no_degradation() {
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store");
    let model_label = kind.as_str().to_owned();

    for (i, body) in [
        "alpha lexical body one",
        "alpha lexical body two",
        "alpha lexical body three",
    ]
    .iter()
    .enumerate()
    {
        let suffix = format!("{i:02X}");
        let rid = format!("01HQZX9F5N0000000000000P{suffix}");
        store.upsert(&record(&rid, body)).await.expect("upsert");
    }
    drain_all(&store, Arc::clone(&embedder)).await;

    let page = store
        .search_hybrid(&hybrid_args("alpha", &model_label))
        .await
        .expect("hybrid");
    assert!(
        !page.candidates.is_empty(),
        "baseline must return candidates",
    );
    assert!(
        page.degraded_legs.is_empty(),
        "healthy hybrid must not emit degraded_legs; got {:?}",
        page.degraded_legs,
    );
}

/// Drop `record_vectors` mid-flight → semantic leg's SQL fails. The
/// round-4 fix converts that error into `DegradedLeg::Semantic` and
/// continues with keyword + graph. Caller must still receive
/// candidates and the per-leg degradation entry.
#[tokio::test]
async fn hybrid_semantic_failure_degrades_to_keyword() {
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store");
    let model_label = kind.as_str().to_owned();

    for (i, body) in [
        "beta keyword body one",
        "beta keyword body two",
        "beta keyword body three",
    ]
    .iter()
    .enumerate()
    {
        let suffix = format!("{i:02X}");
        let rid = format!("01HQZX9F5N0000000000000Q{suffix}");
        store.upsert(&record(&rid, body)).await.expect("upsert");
    }
    drain_all(&store, Arc::clone(&embedder)).await;

    // Break the semantic leg by dropping the vec0 virtual table. Any
    // subsequent ANN query fails with "no such table: record_vectors".
    let conn = store.raw_conn_for_admin().expect("conn").clone();
    conn.call(|c| {
        c.execute_batch("DROP TABLE record_vectors")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("drop record_vectors");

    let page = store
        .search_hybrid(&hybrid_args("beta", &model_label))
        .await
        .expect("hybrid must not hard-fail when semantic leg dies");

    assert!(
        !page.candidates.is_empty(),
        "keyword candidates must survive a semantic-leg failure",
    );
    assert!(
        page.degraded_legs.iter().any(|d| matches!(
            d,
            DegradedLeg::Semantic {
                reason: DegradationReason::SqlError
            }
        )),
        "semantic leg must surface as DegradedLeg::Semantic{{SqlError}}; got {:?}",
        page.degraded_legs,
    );
}
