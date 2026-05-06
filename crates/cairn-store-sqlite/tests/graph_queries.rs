//! Integration tests for [`cairn_store_sqlite::entity_graph::queries::GraphQueries`].

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
