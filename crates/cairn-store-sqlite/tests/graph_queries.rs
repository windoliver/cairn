//! Integration tests for [`cairn_store_sqlite::entity_graph::queries::GraphQueries`].
//!
//! Tasks 1–10: scope-clause, CTE, BFS/DFS, get_entity, get_neighbors,
//! timeline, surprising_connections, token-budget.
//! Task 21: five adversarial cross-tool tests for §3.0a-bis invariants.

use cairn_core::domain::scope::ScopeTuple;
use cairn_store_sqlite::entity_graph::queries::{GraphEdge, GraphQueries};

#[test]
fn scope_clause_emits_six_coalesce_pairs_per_tuple() {
    let tuples = vec![ScopeTuple::default()];
    let (sql, n) = GraphQueries::scope_match_clause(&tuples);
    // Six dimensions × one tuple = six placeholder pairs OR-joined into
    // a single EXISTS arm. Bind count must match.
    assert_eq!(n, 6);
    assert_eq!(sql.matches("coalesce(json_extract").count(), 6);
    assert!(!sql.contains(" OR ")); // single tuple → no OR
}

#[test]
fn scope_clause_two_tuples_or_joins_arms() {
    let tuples = vec![ScopeTuple::default(), ScopeTuple::default()];
    let (sql, n) = GraphQueries::scope_match_clause(&tuples);
    assert_eq!(n, 12);
    assert_eq!(sql.matches(" OR ").count(), 1);
}

#[test]
fn cte_prefix_emits_three_named_ctes_and_counts_binds() {
    let tuples = vec![ScopeTuple::default()];
    let (sql, prefix_binds) = GraphQueries::cte_prefix(&tuples);
    assert!(sql.contains("visible_edges_raw AS"));
    assert!(sql.contains("visible_nodes AS"));
    assert!(sql.contains("visible_edges AS"));
    // visible_edges_raw: valid_at <= ? AND invalid_at > ?  → 2 :now
    // past-invalidated branch: valid_at <= ? AND invalid_at <= ? → 2 :now
    // Total :now = 4. Three scope blocks × 6 params each = 18.
    assert_eq!(prefix_binds.now_count, 4);
    assert_eq!(prefix_binds.scope_count, 18);
}

#[tokio::test(flavor = "current_thread")]
async fn get_entity_by_id_returns_id_and_live_edge_count() {
    let f = cairn_test_fixtures::graph::tiny_graph().await;
    let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
    let hit = q.get_entity_by_id(f.node_a.clone()).await.unwrap().unwrap();
    assert_eq!(hit.id, f.node_a);
    assert_eq!(hit.edge_count, 2); // A→B and A→C in scope_a
}

#[tokio::test(flavor = "current_thread")]
async fn get_entity_by_id_out_of_scope_returns_none() {
    let f = cairn_test_fixtures::graph::tiny_graph().await;
    let q = GraphQueries::new(f.store.clone(), vec![f.scope_b.clone()], f.now);
    assert!(q.get_entity_by_id(f.node_a.clone()).await.unwrap().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn get_entity_by_name_normalizes_and_echoes_input() {
    let f = cairn_test_fixtures::graph::tiny_graph().await;
    let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
    let hit = q
        .get_entity_by_name("Auth Service (v2)".to_owned())
        .await
        .unwrap()
        .expect("found");
    assert_eq!(hit.id, f.node_auth_service);
    // echoed_name is the literal input, NOT a read of entity_nodes.name
    assert_eq!(hit.echoed_name.as_deref(), Some("Auth Service (v2)"));
}

#[tokio::test(flavor = "current_thread")]
async fn bfs_two_hops_returns_depth_stratified_set() {
    let f = cairn_test_fixtures::graph::tiny_graph().await;
    let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
    let res = q.query_bfs(f.node_a.clone(), 2, 64).await.unwrap();
    assert_eq!(res.nodes[0].id, f.node_a); // seed first
    assert!(res.nodes.iter().any(|n| n.id == f.node_b));
    // depth_of cap respected
    assert!(res.depth_of.values().all(|&d| d <= 2));
}

#[tokio::test(flavor = "current_thread")]
async fn query_dfs_reorders_bfs_via_parent_of() {
    let f = cairn_test_fixtures::graph::tiny_graph().await;
    let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
    let bfs = q.query_bfs(f.node_a.clone(), 3, 64).await.unwrap();
    let dfs = q.query_dfs(f.node_a.clone(), 3, 64).await.unwrap();
    // Same edge set, possibly different order
    assert_eq!(
        bfs.edges.iter().map(|e| &e.id).collect::<std::collections::HashSet<_>>(),
        dfs.edges.iter().map(|e| &e.id).collect::<std::collections::HashSet<_>>(),
    );
    // DFS visits a child before any sibling's subtree
    assert_eq!(dfs.nodes[0].id, f.node_a);
}

#[tokio::test(flavor = "current_thread")]
async fn get_neighbors_filters_by_relation_and_confidence() {
    let f = cairn_test_fixtures::graph::tiny_graph().await;
    let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
    let edges = q
        .get_neighbors(f.node_a.clone(), Some("calls".to_owned()), Some(0.7))
        .await
        .unwrap();
    assert!(edges.iter().all(|e| e.relation == "calls"));
    assert!(edges.iter().all(|e| e.confidence_score >= 0.7));
}

#[tokio::test(flavor = "current_thread")]
async fn timeline_default_excludes_future_and_expired() {
    let f = cairn_test_fixtures::graph::timeline_fixture().await;
    let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
    let entries = q.timeline(f.node_a.clone(), false, false).await.unwrap();
    assert!(entries.iter().all(|e| e.expired_at.is_none()));
    assert!(entries.iter().all(|e| e.valid_at <= f.now));
}

#[tokio::test(flavor = "current_thread")]
async fn timeline_include_history_surfaces_future_dated() {
    let f = cairn_test_fixtures::graph::timeline_fixture().await;
    let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
    let entries = q.timeline(f.node_a.clone(), true, false).await.unwrap();
    assert!(entries.iter().any(|e| e.valid_at > f.now));
}

#[tokio::test(flavor = "current_thread")]
async fn surprising_connections_scores_cross_scope_above_same_scope() {
    // 3 in-scope edges in S_a, 1 cross-scope edge in S_b — both
    // resolver entries authorize the caller.
    let f = cairn_test_fixtures::graph::surprise_fixture().await;
    let q = GraphQueries::new(
        f.store.clone(),
        vec![f.scope_a.clone(), f.scope_b.clone()],
        f.now,
    );
    let hits = q.surprising_connections(vec![
        f.node_a.clone(), f.node_b.clone(), f.node_c.clone(),
    ], 10).await.unwrap();
    // Highest score must be the S_b edge (modal is S_a).
    // `source_record_id` lives on `SurpriseHit`, not on the inner
    // `GraphEdge` — the public edge type is id-only by design (§3.1).
    assert_eq!(hits[0].source_record_id.as_str(), &f.rec_b[..]);
    assert!((hits[0].score - 2.0 * hits[0].edge.confidence_score).abs() < 1e-9);
}

#[test]
fn token_budget_truncates_at_byte_threshold() {
    let edges: Vec<GraphEdge> = (0..100)
        .map(|i| GraphEdge {
            id: format!("e{i}"),
            source_id: "s".into(),
            target_id: "t".into(),
            relation: "r".into(),
            confidence_score: 1.0,
            valid_at: 0,
        })
        .collect();
    let truncated = GraphQueries::truncate_to_token_budget(&edges, 200);
    let s = serde_json::to_string(&truncated).unwrap();
    assert!(s.len() <= 200 + 64); // soft cap with single-element overshoot
    assert!(truncated.len() < edges.len());
}

// ── Task 21: adversarial cross-tool tests ─────────────────────────────────────

/// Tombstoned endpoint — node B has `expired_at` set.
///
/// All five tools must hide any edge involving the tombstoned node, and
/// `get_entity_by_id` must return `None` for it.
#[tokio::test(flavor = "current_thread")]
async fn tombstoned_endpoint_hidden_from_all_five_tools() {
    let f = cairn_test_fixtures::graph::endpoint_tombstone_fixture().await;
    let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);

    // get_neighbors from node_a must return nothing (only edge goes to tombstoned B).
    let neighbors = q
        .get_neighbors(f.node_a.clone(), None, None)
        .await
        .unwrap();
    assert!(
        neighbors.is_empty(),
        "get_neighbors must hide edge to tombstoned B; got {} edge(s)",
        neighbors.len()
    );

    // timeline for node_a has no live edges → empty.
    let timeline = q
        .timeline(f.node_a.clone(), true, true)
        .await
        .unwrap();
    assert!(
        timeline.is_empty(),
        "timeline must hide edge to tombstoned B; got {} entry(s)",
        timeline.len()
    );

    // BFS from node_a must never include tombstoned_b.
    let bfs = q.query_bfs(f.node_a.clone(), 5, 64).await.unwrap();
    assert!(
        bfs.nodes.iter().all(|n| n.id != f.tombstoned_b),
        "BFS must not return tombstoned node B; node ids: {:?}",
        bfs.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
    );

    // surprising_connections must return nothing (only tombstoned target).
    let surprises = q
        .surprising_connections(vec![f.node_a.clone(), f.tombstoned_b.clone()], 16)
        .await
        .unwrap();
    assert!(
        surprises.is_empty(),
        "surprising_connections must return nothing when only edge target is tombstoned; got {} hit(s)",
        surprises.len()
    );

    // Direct lookup of tombstoned node B returns None.
    assert!(
        q.get_entity_by_id(f.tombstoned_b.clone())
            .await
            .unwrap()
            .is_none(),
        "get_entity_by_id must return None for tombstoned node B"
    );
}

/// Lineage rescope — immutable provenance.
///
/// The edge's source record has `scope=scope_a` (active=0).  The active head
/// (`r_new`) has `scope=scope_b`.  Scope_a must see the edge; scope_b must not.
#[tokio::test(flavor = "current_thread")]
async fn lineage_rescope_keys_off_immutable_provenance() {
    let f = cairn_test_fixtures::graph::lineage_rescope_fixture().await;

    // scope_a query: edge is visible (provenance record is scope_a).
    let q_a = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
    let neighbors_a = q_a
        .get_neighbors(f.node_a.clone(), None, None)
        .await
        .unwrap();
    assert!(
        !neighbors_a.is_empty(),
        "scope_a must see the edge sourced from its record; got 0 neighbors"
    );

    // scope_b query: active head's scope does not re-authorize the old edge.
    let q_b = GraphQueries::new(f.store.clone(), vec![f.scope_b.clone()], f.now);
    let neighbors_b = q_b
        .get_neighbors(f.node_a.clone(), None, None)
        .await
        .unwrap();
    assert!(
        neighbors_b.is_empty(),
        "scope_b must NOT see the edge: provenance is scope_a, not scope_b; got {} neighbor(s)",
        neighbors_b.len()
    );
}

/// Future-only provenance — edge not visible until clock advances.
///
/// At `now` the edge has `valid_at > now` → invisible.
/// At `future_valid_at + 1` it becomes visible.
#[tokio::test(flavor = "current_thread")]
async fn future_only_provenance_is_not_found_until_clock_advances() {
    let f = cairn_test_fixtures::graph::future_provenance_fixture().await;

    // At fixture `now`: node_future is not visible.
    let q_now = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
    assert!(
        q_now
            .get_entity_by_id(f.node_future.clone())
            .await
            .unwrap()
            .is_none(),
        "node_future must not be visible at fixture now (edge valid_at is in the future)"
    );

    // At future_valid_at + 1: node_future becomes visible.
    let q_future = GraphQueries::new(
        f.store.clone(),
        vec![f.scope_a.clone()],
        f.future_valid_at + 1,
    );
    assert!(
        q_future
            .get_entity_by_id(f.node_future.clone())
            .await
            .unwrap()
            .is_some(),
        "node_future must be visible once clock advances past future_valid_at"
    );
}

/// Episode via tombstoned record — entity must be hidden.
///
/// The node's only provenance is an `entity_episodes` row whose
/// `episode_id` points to a tombstoned record.  `visible_nodes` requires
/// `r_active.tombstoned = 0`, so the node must not appear.
#[tokio::test(flavor = "current_thread")]
async fn episode_via_tombstoned_record_hides_entity() {
    let f = cairn_test_fixtures::graph::episode_tombstone_fixture().await;
    let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
    assert!(
        q.get_entity_by_id(f.node_via_episode.clone())
            .await
            .unwrap()
            .is_none(),
        "node linked only via tombstoned episode record must be hidden from get_entity_by_id"
    );
}

/// `ScopeTuple` has no wildcard on the `user` dimension.
///
/// A record with `scope.user = "bob"` must NOT be visible to a query with
/// `ScopeTuple { tenant: Some("a"), user: None, .. }`.
/// `user=None` binds `''` via coalesce; `'bob' != ''` → no match.
#[tokio::test(flavor = "current_thread")]
async fn scope_tuple_has_no_wildcard_user_dimension() {
    let f = cairn_test_fixtures::graph::scope_user_fixture().await;

    // Query with user=None (no wildcard) — must NOT see the edge with user=bob.
    let scope_no_user = ScopeTuple {
        tenant: Some("a".to_owned()),
        user: None,
        ..ScopeTuple::default()
    };
    let q = GraphQueries::new(f.store.clone(), vec![scope_no_user], f.now);
    let edges = q
        .get_neighbors(f.node_a.clone(), None, None)
        .await
        .unwrap();
    assert!(
        edges.iter().all(|e| e.id != f.edge_with_user_bob),
        "edge sourced from user=bob record must not be visible to user=None query; \
         got {} edge(s), ids: {:?}",
        edges.len(),
        edges.iter().map(|e| &e.id).collect::<Vec<_>>()
    );
}
