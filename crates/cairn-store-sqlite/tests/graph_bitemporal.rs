//! Bitemporal as-of corner cases for the graph 1-hop traversal SQL
//! (issue #191, round-2 fix in `graph_search.rs:103-106`).
//!
//! The graph predicate is:
//!   `valid_at   <= now AND (invalid_at IS NULL OR invalid_at > now)`
//!   `created_at <= now AND (expired_at IS NULL OR expired_at > now)`
//!   `confidence_score >= ?`
//!
//! These tests cover each branch of the disjunction. The
//! "live with future `invalid_at`" case is the regression guard for the
//! round-2 bug, where a future-bounded edge was being dropped.

#![allow(missing_docs)]

use cairn_core::contract::memory_store::{GraphNeighborsArgs, MemoryStore};
use cairn_core::domain::ScopeTuple;
use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId, EntityId, EntityNode};
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{RecordId, TargetId};
use cairn_store_sqlite::{SqliteMemoryStore, open_in_memory};

const FAR_FUTURE_MS: i64 = 99_999_999_999_999; // ~year 5138, > wall clock

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

fn node(id: &str, name_norm: &str) -> EntityNode {
    EntityNode {
        id: EntityId::from(id),
        name: name_norm.into(),
        name_norm: name_norm.into(),
        summary: None,
        created_at: 1,
        embedding_id: None,
    }
}

/// Build a fixture: r1↔r2 linked through entities e1↔e2 by an edge with
/// caller-supplied bitemporal stamps. Returns the store and the seed/neighbor
/// record ids so the test can assert presence/absence of the neighbor.
async fn build_fixture(
    edge_valid_at: i64,
    edge_invalid_at: Option<i64>,
    edge_created_at: i64,
    edge_confidence: f32,
    edge_id_suffix: &str,
) -> (SqliteMemoryStore, RecordId, RecordId) {
    let store = open_in_memory().await.expect("open store");

    let r1 = record("01HQZX9F5N0000000000000R01", "graph seed body");
    let r2 = record("01HQZX9F5N0000000000000R02", "graph neighbor body");
    store.upsert(&r1).await.expect("upsert r1");
    store.upsert(&r2).await.expect("upsert r2");

    let e1 = node("01HZE7JV5N00000000000000E1", "alice-bitemp");
    let e2 = node("01HZE7JV5N00000000000000E2", "bob-bitemp");
    let e1 = store.upsert_entity(&e1).await.expect("e1");
    let e2 = store.upsert_entity(&e2).await.expect("e2");
    store
        .link_entity_episode(&e1, &r1.id)
        .await
        .expect("link r1");
    store
        .link_entity_episode(&e2, &r2.id)
        .await
        .expect("link r2");

    let edge = EntityEdge {
        id: EntityEdgeId::from(format!("01HZE7JV5N00000000000000E{edge_id_suffix}").as_str()),
        source_id: e1.clone(),
        target_id: e2.clone(),
        relation: "knows".into(),
        confidence: EdgeConfidence::Extracted,
        confidence_score: edge_confidence,
        valid_at: edge_valid_at,
        invalid_at: edge_invalid_at,
        created_at: edge_created_at,
        source_record_id: Some(r1.id.clone()),
    };
    store.upsert_entity_edge(&edge).await.expect("edge");

    (store, r1.id, r2.id)
}

async fn search(store: &SqliteMemoryStore, seed: &RecordId, confidence_min: f32) -> Vec<RecordId> {
    let scope = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    let result = store
        .search_graph_neighbors(&GraphNeighborsArgs {
            seed_record_ids: vec![seed.clone()],
            ranked_record_ids: vec![],
            filter: None,
            auth_scope: scope,
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            confidence_min,
        })
        .await
        .expect("graph search");
    result.into_iter().map(|c| c.record_id).collect()
}

// ── Bitemporal as-of corner cases ────────────────────────────────────────

#[tokio::test]
async fn live_edge_never_invalidated_is_included() {
    let (store, seed, neighbor) = build_fixture(1, None, 1, 1.0, "01").await;
    let hits = search(&store, &seed, 0.0).await;
    assert!(
        hits.contains(&neighbor),
        "live edge (valid_at=1, invalid_at=None) must surface neighbor; got {hits:?}",
    );
}

/// Regression guard for round-2 fix: an edge with `invalid_at` set to a
/// future timestamp is still live, and must be admitted by the
/// `(invalid_at IS NULL OR invalid_at > now)` branch.
#[tokio::test]
async fn live_edge_with_future_invalid_at_is_included() {
    let (store, seed, neighbor) = build_fixture(1, Some(FAR_FUTURE_MS), 1, 1.0, "02").await;
    let hits = search(&store, &seed, 0.0).await;
    assert!(
        hits.contains(&neighbor),
        "edge with future invalid_at must remain live; got {hits:?}",
    );
}

#[tokio::test]
async fn invalidated_edge_in_past_is_excluded() {
    // valid_at=1, invalid_at=2 — both before wall-clock now → edge is
    // historically invalidated and must not surface.
    let (store, seed, _neighbor) = build_fixture(1, Some(2), 1, 1.0, "03").await;
    let hits = search(&store, &seed, 0.0).await;
    assert!(
        hits.is_empty(),
        "past-invalidated edge must not surface neighbor; got {hits:?}",
    );
}

#[tokio::test]
async fn not_yet_valid_edge_is_excluded() {
    // valid_at far in the future → fails `valid_at <= now`.
    let (store, seed, _neighbor) = build_fixture(FAR_FUTURE_MS, None, 1, 1.0, "04").await;
    let hits = search(&store, &seed, 0.0).await;
    assert!(
        hits.is_empty(),
        "edge with future valid_at must be excluded; got {hits:?}",
    );
}

#[tokio::test]
async fn not_yet_created_edge_is_excluded() {
    // created_at far in the future → fails `created_at <= now`. This is
    // the round-2 guard for the ingestion-time half of the bitemporal
    // predicate.
    let (store, seed, _neighbor) = build_fixture(1, None, FAR_FUTURE_MS, 1.0, "05").await;
    let hits = search(&store, &seed, 0.0).await;
    assert!(
        hits.is_empty(),
        "edge with future created_at must be excluded; got {hits:?}",
    );
}

// ── Confidence floor boundary ────────────────────────────────────────────

#[tokio::test]
async fn confidence_min_zero_includes_low_confidence_edge() {
    let (store, seed, neighbor) = build_fixture(1, None, 1, 0.1, "06").await;
    let hits = search(&store, &seed, 0.0).await;
    assert!(
        hits.contains(&neighbor),
        "confidence_min=0.0 must include 0.1-confidence edge; got {hits:?}",
    );
}

#[tokio::test]
async fn confidence_min_at_boundary_includes_equal_edge() {
    // SQL uses `confidence_score >= ?`, so an edge at exactly the floor
    // surfaces. Locks the inclusive comparison.
    let (store, seed, neighbor) = build_fixture(1, None, 1, 0.3, "07").await;
    let hits = search(&store, &seed, 0.3).await;
    assert!(
        hits.contains(&neighbor),
        "confidence_min=0.3 must include edge at exactly 0.3 (inclusive); got {hits:?}",
    );
}

#[tokio::test]
async fn confidence_min_above_edge_excludes_edge() {
    let (store, seed, _neighbor) = build_fixture(1, None, 1, 0.3, "08").await;
    let hits = search(&store, &seed, 0.5).await;
    assert!(
        hits.is_empty(),
        "confidence_min=0.5 must exclude 0.3-confidence edge; got {hits:?}",
    );
}

#[tokio::test]
async fn confidence_min_one_excludes_below_one() {
    // Floor at 1.0 only admits edges at exactly 1.0. A 0.9 edge is
    // dropped — confirms the floor is the binding predicate.
    let (store, seed, _neighbor) = build_fixture(1, None, 1, 0.9, "09").await;
    let hits = search(&store, &seed, 1.0).await;
    assert!(
        hits.is_empty(),
        "confidence_min=1.0 must exclude 0.9-confidence edge; got {hits:?}",
    );
}
