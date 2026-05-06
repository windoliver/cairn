//! Integration tests for [`cairn_mcp::graph_tools`].
//!
//! Task 11: registry names and `GetEntityArgs` deserialisation.
//! Task 12: `dispatch` routes `graph.get_entity` to a real store.

use cairn_mcp::graph_tools::{GetEntityArgs, GRAPH_TOOLS};
use cairn_store_sqlite::entity_graph::queries::GraphQueries;
use cairn_test_fixtures::graph::tiny_graph as tiny_graph_async;

// ----- Task 11 tests -------------------------------------------------------

#[test]
fn graph_tools_registry_lists_five_tools() {
    let names: Vec<_> = GRAPH_TOOLS.iter().map(|d| d.name).collect();
    assert_eq!(
        names,
        vec![
            "graph.query",
            "graph.get_entity",
            "graph.get_neighbors",
            "graph.timeline",
            "graph.surprising_connections",
        ]
    );
}

#[test]
fn get_entity_args_rejects_both_id_and_name() {
    let v = serde_json::json!({"id": "x", "name": "y"});
    assert!(serde_json::from_value::<GetEntityArgs>(v).is_err());
}

#[test]
fn get_entity_args_accepts_id_only() {
    let v = serde_json::json!({"id": "x"});
    let parsed: GetEntityArgs = serde_json::from_value(v).unwrap();
    assert!(matches!(parsed, GetEntityArgs::ById { .. }));
}

// ----- Task 12 tests -------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn dispatch_get_entity_by_id_returns_callable_result() {
    let f = tiny_graph_async().await;
    let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
    let args = serde_json::json!({"id": f.node_a});
    let res = cairn_mcp::graph_tools::dispatch(
        &q,
        "graph.get_entity",
        Some(args.as_object().unwrap().clone()),
    )
    .await;
    assert!(!res.is_error.unwrap_or(false));
}
