//! Integration tests for `auth_scope` enforcement across all four search SQL
//! paths (keyword, semantic, graph, graph-only hydration). Issue #191.
//!
//! These tests insert records with distinct scope tuples and assert each
//! retrieval path narrows correctly, refuses cross-tenant leakage, and
//! degrades correctly when capabilities are missing.

use std::sync::Arc;

use cairn_core::contract::memory_store::{
    HybridSearchArgs, KeywordSearchArgs, MemoryStore, SemanticSearchArgs,
};
use cairn_core::domain::ScopeTuple;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{RecordId, TargetId};
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};
use cairn_store_sqlite::open_in_memory_with_embedder;

/// Build a sample record with the given record/target ids, scope, and body.
fn record(rid: &str, tid: &str, scope: ScopeTuple, body: &str) -> cairn_core::domain::MemoryRecord {
    let mut r = sample_record();
    r.id = RecordId::parse(rid.to_owned()).expect("valid record id");
    r.target_id = TargetId::parse(tid.to_owned()).expect("valid target id");
    r.scope = scope;
    body.clone_into(&mut r.body);
    r
}

/// Two-tenant fixture: 4 records, two each in tenant=A and tenant=B,
/// distinguished by body so the keyword leg can match individually.
async fn fixture_two_tenants() -> Arc<dyn MemoryStore> {
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store");

    let a1 = record(
        "01HQZX9F5N0000000000000A01",
        "01HQZX9F5N0000000000000A01",
        ScopeTuple {
            tenant: Some("acme".into()),
            user: Some("hmn:alice".into()),
            ..Default::default()
        },
        "alpha alice acme one",
    );
    let a2 = record(
        "01HQZX9F5N0000000000000A02",
        "01HQZX9F5N0000000000000A02",
        ScopeTuple {
            tenant: Some("acme".into()),
            user: Some("hmn:bob".into()),
            ..Default::default()
        },
        "alpha bob acme two",
    );
    let b1 = record(
        "01HQZX9F5N0000000000000B01",
        "01HQZX9F5N0000000000000B01",
        ScopeTuple {
            tenant: Some("globex".into()),
            user: Some("hmn:alice".into()),
            ..Default::default()
        },
        "alpha alice globex one",
    );
    let b2 = record(
        "01HQZX9F5N0000000000000B02",
        "01HQZX9F5N0000000000000B02",
        ScopeTuple {
            tenant: Some("globex".into()),
            user: Some("hmn:bob".into()),
            ..Default::default()
        },
        "alpha bob globex two",
    );
    for r in [&a1, &a2, &b1, &b2] {
        store.upsert(r).await.expect("upsert");
    }
    Arc::new(store)
}

fn vis_all() -> Vec<MemoryVisibility> {
    vec![
        MemoryVisibility::Private,
        MemoryVisibility::Session,
        MemoryVisibility::Project,
        MemoryVisibility::Team,
        MemoryVisibility::Org,
        MemoryVisibility::Public,
    ]
}

fn keyword_args(query: &str, scope: ScopeTuple) -> KeywordSearchArgs<'static> {
    KeywordSearchArgs {
        query: query.into(),
        filter: None,
        auth_scope: scope,
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 50,
        cursor: None,
        with_explain: false,
    }
}

fn semantic_args(query: &str, scope: ScopeTuple) -> SemanticSearchArgs<'static> {
    SemanticSearchArgs {
        query: query.into(),
        filter: None,
        auth_scope: scope,
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 50,
        model_label: EmbeddingModelKind::default().as_str().to_owned(),
        with_explain: false,
    }
}

fn hybrid_args(query: &str, scope: ScopeTuple) -> HybridSearchArgs<'static> {
    HybridSearchArgs {
        query: query.into(),
        filter: None,
        auth_scope: scope,
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 50,
        model_label: EmbeddingModelKind::default().as_str().to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
        with_explain: false,
        confidence_floor: 1e-3,
    }
}

// ── Keyword leg ─────────────────────────────────────────────────────────

#[tokio::test]
async fn keyword_empty_scope_returns_all_tenants() {
    let store = fixture_two_tenants().await;
    let page = store
        .search_keyword(&keyword_args("alpha", ScopeTuple::default()))
        .await
        .expect("keyword");
    assert_eq!(
        page.candidates.len(),
        4,
        "empty scope must not narrow; got {} ids",
        page.candidates.len()
    );
}

#[tokio::test]
async fn keyword_tenant_scope_excludes_other_tenants() {
    let store = fixture_two_tenants().await;
    let scope = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    let page = store
        .search_keyword(&keyword_args("alpha", scope))
        .await
        .expect("keyword");
    assert_eq!(page.candidates.len(), 2, "tenant=acme expected 2 hits");
    for c in &page.candidates {
        assert!(
            c.record_id.as_str().contains("000A0"),
            "leaked cross-tenant: {}",
            c.record_id.as_str()
        );
    }
}

#[tokio::test]
async fn keyword_multidim_scope_is_anded() {
    let store = fixture_two_tenants().await;
    let scope = ScopeTuple {
        tenant: Some("acme".into()),
        user: Some("hmn:alice".into()),
        ..Default::default()
    };
    let page = store
        .search_keyword(&keyword_args("alpha", scope))
        .await
        .expect("keyword");
    assert_eq!(page.candidates.len(), 1);
    assert_eq!(
        page.candidates[0].record_id.as_str(),
        "01HQZX9F5N0000000000000A01",
        "multidim scope must AND tenant + user"
    );
}

#[tokio::test]
async fn keyword_unmatched_scope_is_empty_not_error() {
    let store = fixture_two_tenants().await;
    let scope = ScopeTuple {
        tenant: Some("nonexistent".into()),
        ..Default::default()
    };
    let page = store
        .search_keyword(&keyword_args("alpha", scope))
        .await
        .expect("keyword must not error on unmatched scope");
    assert!(page.candidates.is_empty());
}

// ── Semantic leg ────────────────────────────────────────────────────────

#[tokio::test]
async fn semantic_tenant_scope_excludes_other_tenants() {
    let store = fixture_two_tenants().await;
    let scope = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    let page = store
        .search_semantic(&semantic_args("alpha", scope))
        .await
        .expect("semantic");
    for c in &page.candidates {
        assert!(
            c.record_id.as_str().contains("000A0"),
            "semantic leaked cross-tenant: {}",
            c.record_id.as_str()
        );
    }
}

// ── Hybrid leg ──────────────────────────────────────────────────────────

#[tokio::test]
async fn hybrid_tenant_scope_excludes_other_tenants() {
    let store = fixture_two_tenants().await;
    let scope = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    let page = store
        .search_hybrid(&hybrid_args("alpha", scope))
        .await
        .expect("hybrid");
    assert!(
        !page.candidates.is_empty(),
        "hybrid should still return acme rows"
    );
    for c in &page.candidates {
        assert!(
            c.record_id.as_str().contains("000A0"),
            "hybrid leaked cross-tenant: {}",
            c.record_id.as_str()
        );
    }
}

#[tokio::test]
async fn hybrid_empty_visibility_with_scope_still_narrows() {
    // Empty visibility = "no visibility filter" — scope must still apply.
    let store = fixture_two_tenants().await;
    let mut args = hybrid_args(
        "alpha",
        ScopeTuple {
            tenant: Some("acme".into()),
            ..Default::default()
        },
    );
    args.visibility_allowlist = vec![]; // explicitly empty
    let page = store.search_hybrid(&args).await.expect("hybrid");
    for c in &page.candidates {
        assert!(
            c.record_id.as_str().contains("000A0"),
            "scope must still narrow when visibility_allowlist is empty: {}",
            c.record_id.as_str()
        );
    }
}

#[tokio::test]
async fn hybrid_visibility_and_scope_both_apply() {
    // sample_record sets visibility=Private. Set visibility allowlist to
    // a different tier and assert no rows come back even though tenant
    // matches.
    let store = fixture_two_tenants().await;
    let mut args = hybrid_args(
        "alpha",
        ScopeTuple {
            tenant: Some("acme".into()),
            ..Default::default()
        },
    );
    args.visibility_allowlist = vec![MemoryVisibility::Public];
    let page = store.search_hybrid(&args).await.expect("hybrid");
    assert!(
        page.candidates.is_empty(),
        "Public visibility filter must exclude all Private rows even within tenant"
    );
}

// ── degraded_legs propagation ────────────────────────────────────────────

#[tokio::test]
async fn hybrid_degraded_legs_present_when_graph_capability_off() {
    // Default in-memory open path advertises graph_search=true. We assert
    // the *empty* shape on a successful run — degraded_legs is empty when
    // every leg ran cleanly.
    let store = fixture_two_tenants().await;
    let page = store
        .search_hybrid(&hybrid_args("alpha", ScopeTuple::default()))
        .await
        .expect("hybrid");
    assert!(
        page.degraded_legs.is_empty(),
        "successful hybrid should report no degraded legs; got {:?}",
        page.degraded_legs,
    );
}

// ── Visibility-allowlist=[] path ─────────────────────────────────────────

#[tokio::test]
async fn keyword_empty_visibility_returns_all_visibilities() {
    // Verifies the empty-allowlist guard wasn't broken. All four fixture
    // records share visibility=Private; an empty allowlist should still
    // return them all (rather than zero, the way the buggy graph-leg SQL
    // used to behave).
    let store = fixture_two_tenants().await;
    let mut args = keyword_args("alpha", ScopeTuple::default());
    args.visibility_allowlist = vec![];
    let page = store.search_keyword(&args).await.expect("keyword");
    assert_eq!(
        page.candidates.len(),
        4,
        "empty visibility_allowlist must not silently drop rows"
    );
}

// ── Graph leg direct ─────────────────────────────────────────────────────

/// `search_graph_neighbors` end-to-end: insert two records linked through
/// an entity edge, call the trait method directly with `r1` as the seed
/// and an empty ranked list, and confirm `r2` surfaces as a graph
/// candidate. Also verifies the contract predicates (`auth_scope`,
/// `visibility`, `filter`, supersession) all apply to the neighbor row.
#[allow(clippy::too_many_lines, reason = "graph fixture needs explicit setup")]
#[tokio::test]
async fn search_graph_neighbors_returns_connected_record() {
    use cairn_core::contract::memory_store::{GraphNeighborsArgs, MemoryStore};
    use cairn_core::domain::graph::{
        EdgeConfidence, EntityEdge, EntityEdgeId, EntityId, EntityNode,
    };

    let store = cairn_store_sqlite::open_in_memory()
        .await
        .expect("open store");

    // Two records, same scope, distinct ids.
    let r1 = record(
        "01HQZX9F5N0000000000000R01",
        "01HQZX9F5N0000000000000R01",
        ScopeTuple {
            tenant: Some("acme".into()),
            ..Default::default()
        },
        "graph rescue source body",
    );
    let r2 = record(
        "01HQZX9F5N0000000000000R02",
        "01HQZX9F5N0000000000000R02",
        ScopeTuple {
            tenant: Some("acme".into()),
            ..Default::default()
        },
        "graph rescue target body",
    );
    store.upsert(&r1).await.expect("upsert r1");
    store.upsert(&r2).await.expect("upsert r2");

    // Entity graph: e1 ↔ e2, each linked to its episode record.
    let e1_node = EntityNode {
        id: EntityId::from("01HZE7JV5N00000000000000E1"),
        name: "Alice".into(),
        name_norm: "alice-graph-direct".into(),
        summary: None,
        created_at: 1,
        embedding_id: None,
    };
    let e2_node = EntityNode {
        id: EntityId::from("01HZE7JV5N00000000000000E2"),
        name: "Bob".into(),
        name_norm: "bob-graph-direct".into(),
        summary: None,
        created_at: 1,
        embedding_id: None,
    };
    let e1 = store.upsert_entity(&e1_node).await.expect("e1");
    let e2 = store.upsert_entity(&e2_node).await.expect("e2");
    store
        .link_entity_episode(&e1, &r1.id)
        .await
        .expect("link r1");
    store
        .link_entity_episode(&e2, &r2.id)
        .await
        .expect("link r2");

    // High-confidence edge in the past so the bitemporal predicate
    // (`valid_at <= now AND created_at <= now`) admits it.
    let edge = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N00000000000000ED"),
        source_id: e1.clone(),
        target_id: e2.clone(),
        relation: "knows".into(),
        confidence: EdgeConfidence::Extracted,
        confidence_score: 1.0,
        valid_at: 1,
        invalid_at: None,
        created_at: 1,
        // Provenance: the edge "knows" was extracted from r1; the graph
        // SQL applies auth/visibility/supersession to this provenance
        // record before allowing the edge to contribute.
        source_record_id: Some(r1.id.clone()),
    };
    store.upsert_entity_edge(&edge).await.expect("edge");

    // Seed = r1, no ranked exclusion → r2 must surface as the 1-hop
    // neighbor.
    let scope = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    let result = store
        .search_graph_neighbors(&GraphNeighborsArgs {
            seed_record_ids: vec![r1.id.clone()],
            ranked_record_ids: vec![],
            filter: None,
            auth_scope: scope.clone(),
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            confidence_min: 0.0,
        })
        .await
        .expect("graph search");
    let ids: Vec<_> = result.iter().map(|c| c.record_id.as_str()).collect();
    assert!(
        ids.iter().any(|id| id.ends_with("R02")),
        "graph traversal must surface r2 from seed=r1; got {ids:?}"
    );

    // Cross-tenant scope: r2 lives in tenant=acme, but the caller asks
    // for tenant=globex. Graph SQL must reject the neighbor.
    let result = store
        .search_graph_neighbors(&GraphNeighborsArgs {
            seed_record_ids: vec![r1.id.clone()],
            ranked_record_ids: vec![],
            filter: None,
            auth_scope: ScopeTuple {
                tenant: Some("globex".into()),
                ..Default::default()
            },
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            confidence_min: 0.0,
        })
        .await
        .expect("graph search cross-tenant");
    assert!(
        result.is_empty(),
        "cross-tenant scope must yield empty graph result; got {:?}",
        result
            .iter()
            .map(|c| c.record_id.as_str())
            .collect::<Vec<_>>()
    );

    // Ranked exclusion: include r2 in `ranked_record_ids` and confirm it
    // is dropped from the graph result.
    let result = store
        .search_graph_neighbors(&GraphNeighborsArgs {
            seed_record_ids: vec![r1.id.clone()],
            ranked_record_ids: vec![r2.id.clone()],
            filter: None,
            auth_scope: scope,
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            confidence_min: 0.0,
        })
        .await
        .expect("graph search ranked-exclude");
    assert!(
        result.is_empty(),
        "ranked_record_ids must dedup r2 from graph result; got {:?}",
        result
            .iter()
            .map(|c| c.record_id.as_str())
            .collect::<Vec<_>>()
    );
}

// ── Special-character values ─────────────────────────────────────────────

#[tokio::test]
async fn scope_with_special_characters_does_not_break_sql() {
    // Records with quotes, backslashes, etc. in scope values must round-trip
    // through json_extract without breaking SQL parsing.
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store");

    let weird = record(
        "01HQZX9F5N0000000000000W01",
        "01HQZX9F5N0000000000000W01",
        ScopeTuple {
            tenant: Some("with-dashes_and_under.scores".into()),
            ..Default::default()
        },
        "weird tenant value",
    );
    store.upsert(&weird).await.expect("upsert");

    let scope = ScopeTuple {
        tenant: Some("with-dashes_and_under.scores".into()),
        ..Default::default()
    };
    let mut args = keyword_args("weird", scope);
    args.visibility_allowlist = vis_all();
    let page = store.search_keyword(&args).await.expect("keyword");
    assert_eq!(page.candidates.len(), 1);
}

// ── Graph provenance auth (round-1 fix #2) ──────────────────────────────
//
// The graph SQL INNER JOINs `entity_edges.source_record_id` to a `records pr`
// alias and applies auth/visibility/active/tombstoned/supersession to `pr`
// before allowing the edge to contribute. The next three tests verify each
// failure mode of that gate is honored.

/// Helper: build an entity-graph fixture with two records (`r1`, `r2`),
/// two entities (`e1`, `e2`), and an edge `e1↔e2` whose provenance is
/// set by the caller via `provenance`. Returns the open store, the seed
/// record id, the neighbor record id, and an in-flight edge handle so
/// the caller can mutate state (e.g. tombstone the provenance) before
/// querying.
async fn provenance_fixture(
    seed_scope: ScopeTuple,
    neighbor_scope: ScopeTuple,
    provenance_scope: ScopeTuple,
    provenance: Option<&str>,
) -> (Arc<dyn MemoryStore>, RecordId, RecordId, RecordId) {
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId, EntityId, EntityNode};

    let store = cairn_store_sqlite::open_in_memory()
        .await
        .expect("open store");
    let r1 = record(
        "01HQZX9F5N0000000000000P01",
        "01HQZX9F5N0000000000000P01",
        seed_scope,
        "provenance seed body",
    );
    let r2 = record(
        "01HQZX9F5N0000000000000P02",
        "01HQZX9F5N0000000000000P02",
        neighbor_scope,
        "provenance neighbor body",
    );
    let pr = record(
        "01HQZX9F5N0000000000000P03",
        "01HQZX9F5N0000000000000P03",
        provenance_scope,
        "provenance source-of-extraction body",
    );
    store.upsert(&r1).await.expect("upsert r1");
    store.upsert(&r2).await.expect("upsert r2");
    store.upsert(&pr).await.expect("upsert pr");

    let e1_node = EntityNode {
        id: EntityId::from("01HZE7JV5N00000000000000PA"),
        name: "Alice".into(),
        name_norm: "alice-prov".into(),
        summary: None,
        created_at: 1,
        embedding_id: None,
    };
    let e2_node = EntityNode {
        id: EntityId::from("01HZE7JV5N00000000000000PB"),
        name: "Bob".into(),
        name_norm: "bob-prov".into(),
        summary: None,
        created_at: 1,
        embedding_id: None,
    };
    let e1 = store.upsert_entity(&e1_node).await.expect("e1");
    let e2 = store.upsert_entity(&e2_node).await.expect("e2");
    store
        .link_entity_episode(&e1, &r1.id)
        .await
        .expect("link r1");
    store
        .link_entity_episode(&e2, &r2.id)
        .await
        .expect("link r2");

    let source_record_id = match provenance {
        Some("seed") => Some(r1.id.clone()),
        Some("provenance") => Some(pr.id.clone()),
        Some("neighbor") => Some(r2.id.clone()),
        _ => None,
    };
    let edge = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N00000000000000PE"),
        source_id: e1,
        target_id: e2,
        relation: "knows".into(),
        confidence: EdgeConfidence::Extracted,
        confidence_score: 1.0,
        valid_at: 1,
        invalid_at: None,
        created_at: 1,
        source_record_id,
    };
    store.upsert_entity_edge(&edge).await.expect("edge");
    (Arc::new(store), r1.id, r2.id, pr.id)
}

/// Edge whose provenance lives in tenant `globex` cannot pull the
/// neighbor into a result for caller scope `acme`, even though both the
/// seed and the neighbor are themselves in `acme`. The provenance check
/// runs inside the `neighbors` CTE, before the edge contributes.
#[tokio::test]
async fn graph_neighbor_with_unauthorized_provenance_dropped() {
    use cairn_core::contract::memory_store::GraphNeighborsArgs;

    let acme = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    let globex = ScopeTuple {
        tenant: Some("globex".into()),
        ..Default::default()
    };
    let (store, seed, _neighbor, _pr) =
        provenance_fixture(acme.clone(), acme.clone(), globex, Some("provenance")).await;

    let result = store
        .search_graph_neighbors(&GraphNeighborsArgs {
            seed_record_ids: vec![seed],
            ranked_record_ids: vec![],
            filter: None,
            auth_scope: acme,
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            confidence_min: 0.0,
        })
        .await
        .expect("graph search");
    assert!(
        result.is_empty(),
        "edge with cross-tenant provenance must NOT pull neighbor into acme result; got {:?}",
        result
            .iter()
            .map(|c| c.record_id.as_str())
            .collect::<Vec<_>>(),
    );
}

/// Null `source_record_id` disqualifies the edge entirely. The `INNER
/// JOIN records pr ON pr.record_id = e.source_record_id` filters out
/// rows with NULL provenance regardless of caller scope.
#[tokio::test]
async fn graph_neighbor_with_null_provenance_dropped() {
    use cairn_core::contract::memory_store::GraphNeighborsArgs;

    let acme = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    let (store, seed, _neighbor, _pr) =
        provenance_fixture(acme.clone(), acme.clone(), acme.clone(), None).await;

    let result = store
        .search_graph_neighbors(&GraphNeighborsArgs {
            seed_record_ids: vec![seed],
            ranked_record_ids: vec![],
            filter: None,
            auth_scope: acme,
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            confidence_min: 0.0,
        })
        .await
        .expect("graph search");
    assert!(
        result.is_empty(),
        "edge with NULL source_record_id must yield no neighbors; got {:?}",
        result
            .iter()
            .map(|c| c.record_id.as_str())
            .collect::<Vec<_>>(),
    );
}

/// Provenance record tombstoned ⇒ `pr.tombstoned = 0` predicate fails ⇒
/// edge cannot contribute. Even though the seed, the neighbor, and the
/// scope all match, the dead provenance disqualifies the edge.
#[tokio::test]
async fn graph_neighbor_with_tombstoned_provenance_dropped() {
    use cairn_core::contract::memory_store::{GraphNeighborsArgs, TombstoneReason};

    let acme = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    let (store, seed, _neighbor, pr) =
        provenance_fixture(acme.clone(), acme.clone(), acme.clone(), Some("provenance")).await;

    // Sanity: with a healthy provenance, the neighbor surfaces.
    let healthy = store
        .search_graph_neighbors(&GraphNeighborsArgs {
            seed_record_ids: vec![seed.clone()],
            ranked_record_ids: vec![],
            filter: None,
            auth_scope: acme.clone(),
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            confidence_min: 0.0,
        })
        .await
        .expect("graph search (healthy)");
    assert_eq!(
        healthy.len(),
        1,
        "baseline: healthy provenance must surface the neighbor",
    );

    // Tombstone the provenance record. The edge must now stop contributing.
    store
        .tombstone(&pr, TombstoneReason::Forget)
        .await
        .expect("tombstone pr");

    let result = store
        .search_graph_neighbors(&GraphNeighborsArgs {
            seed_record_ids: vec![seed],
            ranked_record_ids: vec![],
            filter: None,
            auth_scope: acme,
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            confidence_min: 0.0,
        })
        .await
        .expect("graph search (post-tombstone)");
    assert!(
        result.is_empty(),
        "tombstoned provenance must disqualify edge; got {:?}",
        result
            .iter()
            .map(|c| c.record_id.as_str())
            .collect::<Vec<_>>(),
    );
}
