//! Integration tests for [`cairn_store_sqlite::entity_graph::queries::GraphQueries`].

use cairn_core::domain::scope::ScopeTuple;
use cairn_store_sqlite::entity_graph::queries::GraphQueries;

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
