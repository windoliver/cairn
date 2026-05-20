//! Hybrid search integration test using a deterministic mock embedder.
//!
//! Covers the end-to-end hybrid path: open store with embedder, upsert two
//! distinguishable rows so embed-on-write populates `record_vectors`, then
//! issue a hybrid query and assert the keyword-matching row outranks the
//! non-matching one.

use std::sync::Arc;

use cairn_core::contract::memory_store::{HybridSearchArgs, MemoryStore, SemanticSearchArgs};
use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId, EntityId, EntityNode};
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{RecordId, TargetId};
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};
use cairn_store_sqlite::open_in_memory_with_embedder;

async fn hot_salience(store: &cairn_store_sqlite::SqliteMemoryStore, record_id: &RecordId) -> f64 {
    let conn = store.raw_conn().expect("conn").clone();
    let id = record_id.as_str().to_owned();
    conn.call(move |c| {
        c.query_row(
            "SELECT salience FROM records WHERE record_id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    })
    .await
    .expect("hot salience")
}

async fn remove_vector(store: &cairn_store_sqlite::SqliteMemoryStore, record_id: &RecordId) {
    let conn = store.raw_conn().expect("conn").clone();
    let id = record_id.as_str().to_owned();
    conn.call(move |c| {
        c.execute(
            "DELETE FROM record_vectors WHERE record_id = ?1",
            rusqlite::params![id],
        )
        .map_err(Into::into)
        .map(|_| ())
    })
    .await
    .expect("remove vector");
}

#[tokio::test]
async fn hybrid_returns_results() {
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store with embedder");
    assert!(
        store.capabilities().vector,
        "store must have vector capability when embedder is wired",
    );

    // Two records with distinguishable bodies so the keyword leg can
    // distinguish them. Both share visibility = Private (sample_record default)
    // so the visibility allowlist below admits both.
    let mut a = cairn_core::domain::record::tests_export::sample_record();
    "alice chen worked at novapay".clone_into(&mut a.body);

    let mut b = cairn_core::domain::record::tests_export::sample_record();
    b.id = RecordId::parse("01HQZX9F5N00000000000000B1".to_owned()).expect("valid id");
    b.target_id = TargetId::parse("01HQZX9F5N00000000000000B2".to_owned()).expect("valid target");
    "carol nakamura runs mindbridge".clone_into(&mut b.body);

    store.upsert(&a).await.expect("upsert a");
    store.upsert(&b).await.expect("upsert b");

    // Sanity: semantic search with an embedder produces results too — guards
    // against silent misconfiguration of the test fixture.
    let sem = store
        .search_semantic(&SemanticSearchArgs {
            query: "alice".into(),
            filter: None,
            auth_scope: cairn_core::domain::ScopeTuple::default(),
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 5,
            model_label: kind.as_str().to_owned(),
            with_explain: false,
        })
        .await
        .expect("semantic search");
    assert!(
        !sem.candidates.is_empty(),
        "semantic leg should produce candidates after embed-on-write",
    );

    let args = HybridSearchArgs {
        query: "alice".into(),
        filter: None,
        auth_scope: cairn_core::domain::ScopeTuple::default(),
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 5,
        model_label: kind.as_str().to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
        with_explain: false,
        confidence_floor: 1e-3,
        graph_confidence_min: 0.0,
    };
    let page = store.search_hybrid(&args).await.expect("hybrid search");
    assert!(!page.candidates.is_empty(), "hybrid returned 0 results");
    assert_eq!(
        page.candidates[0].record_id,
        a.id,
        "record A (matches keyword 'alice') must rank first; got {:?}",
        page.candidates
            .iter()
            .map(|c| c.record_id.as_str())
            .collect::<Vec<_>>(),
    );
}

#[tokio::test]
async fn hybrid_graph_only_result_carries_canonical_record_json() {
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store with embedder");

    let mut seed = cairn_core::domain::record::tests_export::sample_record();
    seed.id = RecordId::parse("01HQZX9F5N00000000000000D1".to_owned()).expect("valid seed id");
    seed.target_id =
        TargetId::parse("01HQZX9F5N00000000000000D1".to_owned()).expect("valid seed target");
    "alice graph seed lexical anchor".clone_into(&mut seed.body);

    let mut neighbor = cairn_core::domain::record::tests_export::sample_record();
    neighbor.id =
        RecordId::parse("01HQZX9F5N00000000000000D2".to_owned()).expect("valid neighbor id");
    neighbor.target_id =
        TargetId::parse("01HQZX9F5N00000000000000D2".to_owned()).expect("valid neighbor target");
    "graph-only neighbor should carry json".clone_into(&mut neighbor.body);

    store.upsert(&seed).await.expect("upsert seed");
    store.upsert(&neighbor).await.expect("upsert neighbor");
    remove_vector(&store, &neighbor.id).await;

    let seed_entity = EntityNode {
        id: EntityId::from("01HZE7JV5N00000000000000D1"),
        name: "Seed".to_owned(),
        name_norm: "seed-graph-only-json".to_owned(),
        summary: None,
        created_at: 1,
        embedding_id: None,
    };
    let neighbor_entity = EntityNode {
        id: EntityId::from("01HZE7JV5N00000000000000D2"),
        name: "Neighbor".to_owned(),
        name_norm: "neighbor-graph-only-json".to_owned(),
        summary: None,
        created_at: 1,
        embedding_id: None,
    };
    let seed_entity_id = store
        .upsert_entity(&seed_entity)
        .await
        .expect("seed entity");
    let neighbor_entity_id = store
        .upsert_entity(&neighbor_entity)
        .await
        .expect("neighbor entity");
    store
        .link_entity_episode(&seed_entity_id, &seed.id)
        .await
        .expect("link seed");
    store
        .link_entity_episode(&neighbor_entity_id, &neighbor.id)
        .await
        .expect("link neighbor");
    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000DE"),
            source_id: seed_entity_id,
            target_id: neighbor_entity_id,
            relation: "related".to_owned(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 1,
            invalid_at: None,
            created_at: 1,
            source_record_id: Some(seed.id.clone()),
        })
        .await
        .expect("entity edge");

    let page = store
        .search_hybrid(&HybridSearchArgs {
            query: "alice".into(),
            filter: None,
            auth_scope: cairn_core::domain::ScopeTuple::default(),
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 5,
            model_label: kind.as_str().to_owned(),
            blend: 0.7,
            rrf_k: 60,
            rerank_topk: 20,
            with_explain: true,
            confidence_floor: 1e-3,
            graph_confidence_min: 0.0,
        })
        .await
        .expect("hybrid search");

    let graph_only = page
        .candidates
        .iter()
        .find(|candidate| candidate.record_id == neighbor.id)
        .expect("graph-only neighbor should survive hybrid hydration");
    let hydrated: cairn_core::domain::MemoryRecord =
        serde_json::from_str(&graph_only.record_json).expect("canonical record_json");
    assert_eq!(hydrated.id, neighbor.id);
    assert_eq!(hydrated.body, neighbor.body);
    assert!(
        page.explain
            .as_ref()
            .expect("explain")
            .iter()
            .any(|entry| entry.record_id == neighbor.id),
        "graph-only candidate should have an explain row"
    );
}

#[tokio::test]
async fn hybrid_tracks_access_for_final_semantic_only_results() {
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store with embedder");

    let mut rec = cairn_core::domain::record::tests_export::sample_record();
    "semantic only hybrid access target".clone_into(&mut rec.body);
    store.upsert(&rec).await.expect("upsert");

    let args = HybridSearchArgs {
        query: "nomatchinglexicaltoken".into(),
        filter: None,
        auth_scope: cairn_core::domain::ScopeTuple::default(),
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 5,
        model_label: kind.as_str().to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
        with_explain: false,
        confidence_floor: 1e-3,
        graph_confidence_min: 0.0,
    };
    let page = store.search_hybrid(&args).await.expect("hybrid search");

    assert!(
        page.candidates
            .iter()
            .any(|candidate| candidate.record_id == rec.id),
        "semantic-only hybrid result should include seeded record",
    );
    assert!(
        hot_salience(&store, &rec.id).await > f64::from(rec.salience),
        "hybrid should strengthen final returned records even without keyword hits",
    );
}

#[tokio::test]
async fn hybrid_does_not_track_access_for_graph_seed_only_records() {
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store with embedder");

    let mut a = cairn_core::domain::record::tests_export::sample_record();
    a.salience = 0.4;
    "alice chen worked at novapay".clone_into(&mut a.body);

    let mut b = cairn_core::domain::record::tests_export::sample_record();
    b.id = RecordId::parse("01HQZX9F5N00000000000000C1".to_owned()).expect("valid id");
    b.target_id = TargetId::parse("01HQZX9F5N00000000000000C2".to_owned()).expect("valid target");
    b.salience = 0.3;
    "carol nakamura runs mindbridge".clone_into(&mut b.body);

    store.upsert(&a).await.expect("upsert a");
    store.upsert(&b).await.expect("upsert b");

    let args = HybridSearchArgs {
        query: "alice".into(),
        filter: None,
        auth_scope: cairn_core::domain::ScopeTuple::default(),
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 1,
        model_label: kind.as_str().to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
        with_explain: false,
        confidence_floor: 1e-3,
        graph_confidence_min: 0.0,
    };
    let page = store.search_hybrid(&args).await.expect("hybrid search");

    assert_eq!(page.candidates.len(), 1);
    assert_eq!(page.candidates[0].record_id, a.id);
    assert!(
        hot_salience(&store, &a.id).await > f64::from(a.salience),
        "hybrid should strengthen the returned record",
    );
    let b_salience = hot_salience(&store, &b.id).await;
    assert!(
        (b_salience - f64::from(b.salience)).abs() < f64::EPSILON,
        "hybrid must not strengthen records used only for graph seeding: got {b_salience}",
    );
}

#[tokio::test]
async fn hybrid_capability_unavailable_without_embedder() {
    let store = open_in_memory_with_embedder(None)
        .await
        .expect("open store without embedder");
    assert!(
        !store.capabilities().vector,
        "store without embedder must not advertise vector capability",
    );

    let args = HybridSearchArgs {
        query: "anything".into(),
        filter: None,
        auth_scope: cairn_core::domain::ScopeTuple::default(),
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 5,
        model_label: EmbeddingModelKind::default().as_str().to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
        with_explain: false,
        confidence_floor: 1e-3,
        graph_confidence_min: 0.0,
    };
    let result = store.search_hybrid(&args).await;
    assert!(result.is_err(), "expected CapabilityUnavailable");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("capability") || msg.contains("vector"),
        "expected CapabilityUnavailable, got: {msg}",
    );
}
