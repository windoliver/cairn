//! Integration tests for [`cairn_mcp::graph_tools`].
//!
//! Task 11: registry names and `GetEntityArgs` deserialisation.
//! Task 12: `dispatch` routes `graph.get_entity` to a real store.
//! Task 13: `call_tool` routes graph tools through `materialize_graph_request`.
//! Task 14: `list_tools` advertises graph tools when available.
//! Task 20: manifest snapshot matrix (6-cell §2.1 table).
#![allow(missing_docs)]

use cairn_mcp::graph_tools::{GRAPH_TOOLS, GetEntityArgs};
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

// ----- Task 13 & 14 helpers ------------------------------------------------

use cairn_core::config::CairnConfig;
use cairn_core::domain::ScopeTuple;
use cairn_core::mcp_auth::{McpAuthContext, McpSessionScope, ScopeResolutionError};
use cairn_mcp::CairnMcpHandler;
use rmcp::ServiceExt as _;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A `McpSessionScope` that always returns a fixed set of scopes.
struct StaticScope {
    allowed: Vec<ScopeTuple>,
}

impl StaticScope {
    fn new(allowed: Vec<ScopeTuple>) -> Self {
        Self { allowed }
    }
}

impl McpSessionScope for StaticScope {
    fn allowed_scopes(
        &self,
        _ctx: &McpAuthContext<'_>,
    ) -> Result<Vec<ScopeTuple>, ScopeResolutionError> {
        Ok(self.allowed.clone())
    }
}

/// A `McpSessionScope` that always returns an empty scope list.
struct EmptyScope;

impl McpSessionScope for EmptyScope {
    fn allowed_scopes(
        &self,
        _ctx: &McpAuthContext<'_>,
    ) -> Result<Vec<ScopeTuple>, ScopeResolutionError> {
        Ok(vec![])
    }
}

/// A `McpSessionScope` that always returns an error.
struct ErrorScope;

impl McpSessionScope for ErrorScope {
    fn allowed_scopes(
        &self,
        _ctx: &McpAuthContext<'_>,
    ) -> Result<Vec<ScopeTuple>, ScopeResolutionError> {
        Err(ScopeResolutionError::Other {
            reason: "transient resolver failure".to_owned(),
        })
    }
}

/// Build a handler with single-tenant mode on, `graph_edges` capability, and a
/// working scope resolver — the "graph capable" baseline.
async fn build_handler_single_tenant_graph_capable() -> CairnMcpHandler {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> = Arc::new(StaticScope::new(vec![f.scope_a.clone()]));

    // The trait-object store (verb path) wraps the same sqlite store.
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();

    CairnMcpHandler::with_store_scope_and_sqlite(store_dyn, f.store, scope, cfg, f.scope_a)
}

/// Build a handler where the scope resolver returns an empty Vec.
async fn build_handler_resolver_returns_empty() -> CairnMcpHandler {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> = Arc::new(EmptyScope);
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();

    CairnMcpHandler::with_store_scope_and_sqlite(store_dyn, f.store, scope, cfg, f.scope_a)
}

/// Build a handler where the scope resolver always errors.
async fn build_handler_resolver_errors() -> CairnMcpHandler {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> = Arc::new(ErrorScope);
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();

    CairnMcpHandler::with_store_scope_and_sqlite(store_dyn, f.store, scope, cfg, f.scope_a)
}

/// Write one newline-terminated JSON-RPC frame.
async fn send_frame(writer: &mut (impl AsyncWriteExt + Unpin), json: &str) {
    writer
        .write_all(json.as_bytes())
        .await
        .expect("write frame");
    writer.write_all(b"\n").await.expect("write newline");
    writer.flush().await.expect("flush");
}

/// Read one newline-terminated JSON line and parse it.
async fn recv_frame(
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read frame line");
    serde_json::from_str(line.trim()).expect("parse frame as JSON")
}

/// Perform the MCP initialize handshake.
async fn do_initialize(
    writer: &mut (impl AsyncWriteExt + Unpin),
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
) {
    send_frame(
        writer,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"graph-test","version":"0.0.0"}}}"#,
    )
    .await;
    recv_frame(reader).await;
    send_frame(
        writer,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await;
}

// ----- Task 13 tests -------------------------------------------------------

#[tokio::test]
async fn call_tool_routes_graph_tool_when_available() {
    let handler = build_handler_single_tenant_graph_capable().await;
    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        handler
            .serve(server_half)
            .await
            .expect("server init")
            .waiting()
            .await
            .ok();
    });

    let (client_read, mut client_write) = tokio::io::split(client_half);
    let mut client_reader = BufReader::new(client_read);
    do_initialize(&mut client_write, &mut client_reader).await;

    send_frame(
        &mut client_write,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"graph.get_entity","arguments":{"id":"test-id"}}}"#,
    )
    .await;

    let resp = recv_frame(&mut client_reader).await;
    assert!(
        resp.get("result").is_some(),
        "graph.get_entity must yield a result frame; got: {resp}"
    );
    // The handler must NOT return capability_unavailable — it must have
    // dispatched to the store (even if the entity is absent).
    let content_text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        !content_text.contains("capability unavailable"),
        "expected dispatch to reach store, got: {content_text}"
    );
}

#[tokio::test]
async fn call_tool_returns_capability_unavailable_when_resolver_empty() {
    let handler = build_handler_resolver_returns_empty().await;
    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        handler
            .serve(server_half)
            .await
            .expect("server init")
            .waiting()
            .await
            .ok();
    });

    let (client_read, mut client_write) = tokio::io::split(client_half);
    let mut client_reader = BufReader::new(client_read);
    do_initialize(&mut client_write, &mut client_reader).await;

    send_frame(
        &mut client_write,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"graph.get_entity","arguments":{"id":"a"}}}"#,
    )
    .await;

    let resp = recv_frame(&mut client_reader).await;
    let is_error = resp
        .pointer("/result/isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(
        is_error,
        "empty-resolver handler must return isError=true; got: {resp}"
    );
}

#[tokio::test]
async fn call_tool_returns_capability_unavailable_when_resolver_errors() {
    // Resolver that succeeds in any preflight check would still be re-called;
    // this test asserts a transient resolver Err becomes a denied response,
    // never a panic.
    let handler = build_handler_resolver_errors().await;
    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        handler
            .serve(server_half)
            .await
            .expect("server init")
            .waiting()
            .await
            .ok();
    });

    let (client_read, mut client_write) = tokio::io::split(client_half);
    let mut client_reader = BufReader::new(client_read);
    do_initialize(&mut client_write, &mut client_reader).await;

    send_frame(
        &mut client_write,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"graph.get_entity","arguments":{"id":"a"}}}"#,
    )
    .await;

    let resp = recv_frame(&mut client_reader).await;
    let is_error = resp
        .pointer("/result/isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(
        is_error,
        "error-resolver handler must return isError=true; got: {resp}"
    );
}

// ----- Task 14 tests -------------------------------------------------------

#[tokio::test]
async fn list_tools_lists_graph_when_resolver_returns_non_empty() {
    let handler = build_handler_single_tenant_graph_capable().await;
    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        handler
            .serve(server_half)
            .await
            .expect("server init")
            .waiting()
            .await
            .ok();
    });

    let (client_read, mut client_write) = tokio::io::split(client_half);
    let mut client_reader = BufReader::new(client_read);
    do_initialize(&mut client_write, &mut client_reader).await;

    send_frame(
        &mut client_write,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
    )
    .await;

    let resp = recv_frame(&mut client_reader).await;
    let tools = resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools/list result must contain a tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(serde_json::Value::as_str))
        .collect();
    assert!(
        names.contains(&"graph.query"),
        "expected graph.query in tool list, got: {names:?}"
    );
}

// ----- Task 20: manifest snapshot matrix (§2.1 table) ----------------------
//
// Six cells; only the bottom row (Stdio + single_tenant=true + graph_edges=true
// + resolver=OkNonEmpty) must advertise graph.* tools.  All others must list
// only the eight core verbs.
//
// Each helper drives the handler through the wire-protocol duplex approach so
// we exercise exactly the same code-path as a real client: initialize
// handshake → tools/list → snapshot the sorted tool-name list.

/// Drive `handler` through initialize + tools/list and return the sorted list
/// of tool names.
async fn tools_list_via_wire(handler: CairnMcpHandler) -> Vec<String> {
    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        handler
            .serve(server_half)
            .await
            .expect("server init")
            .waiting()
            .await
            .ok();
    });

    let (client_read, mut client_write) = tokio::io::split(client_half);
    let mut client_reader = BufReader::new(client_read);
    do_initialize(&mut client_write, &mut client_reader).await;

    send_frame(
        &mut client_write,
        r#"{"jsonrpc":"2.0","id":99,"method":"tools/list"}"#,
    )
    .await;

    let resp = recv_frame(&mut client_reader).await;
    let tools = resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools/list must return tools array");
    let mut names: Vec<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(serde_json::Value::as_str))
        .map(ToOwned::to_owned)
        .collect();
    names.sort();
    names
}

/// Cell 1 — `single_tenant=false` (graph tools must be absent regardless of
/// other conditions).
#[tokio::test]
async fn manifest_matrix_single_tenant_off() {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = false; // gate fails here
    // No principal required when single_tenant=false.

    let scope: Arc<dyn McpSessionScope> = Arc::new(StaticScope::new(vec![f.scope_a.clone()]));
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();

    let handler =
        CairnMcpHandler::with_store_scope_and_sqlite(store_dyn, f.store, scope, cfg, f.scope_a);

    let names = tools_list_via_wire(handler).await;
    insta::assert_json_snapshot!("manifest_single_tenant_off", names);
}

/// Cell 2 — `single_tenant=true` but store does NOT advertise `graph_edges`
/// (`FixtureStore` has `graph_edges=false`).
#[tokio::test]
async fn manifest_matrix_graph_edges_false() {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> = Arc::new(StaticScope::new(vec![f.scope_a.clone()]));
    // Use FixtureStore (graph_edges=false) as the capability-bearing store.
    let store_no_graph: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
        Arc::new(cairn_test_fixtures::FixtureStore::default());

    let handler = CairnMcpHandler::with_store_scope_and_sqlite(
        store_no_graph,
        f.store,
        scope,
        cfg,
        f.scope_a,
    );

    let names = tools_list_via_wire(handler).await;
    insta::assert_json_snapshot!("manifest_graph_edges_false", names);
}

/// Cell 3 — `single_tenant=true`, `graph_edges=true`, but no scope resolver wired
/// (Absent). `materialize_graph_request` short-circuits on `scope.is_none()`.
#[tokio::test]
async fn manifest_matrix_resolver_absent() {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    // Build the handler WITHOUT a scope resolver by using `with_store` (no scope).
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();
    let handler = CairnMcpHandler::with_store(store_dyn, cfg);

    let names = tools_list_via_wire(handler).await;
    insta::assert_json_snapshot!("manifest_resolver_absent", names);
}

/// Cell 4 — `single_tenant=true`, `graph_edges=true`, resolver always errors.
#[tokio::test]
async fn manifest_matrix_resolver_err() {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> = Arc::new(ErrorScope);
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();

    let handler =
        CairnMcpHandler::with_store_scope_and_sqlite(store_dyn, f.store, scope, cfg, f.scope_a);

    let names = tools_list_via_wire(handler).await;
    insta::assert_json_snapshot!("manifest_resolver_err", names);
}

/// Cell 5 — `single_tenant=true`, `graph_edges=true`, resolver returns empty `Vec`.
#[tokio::test]
async fn manifest_matrix_resolver_ok_empty() {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> = Arc::new(EmptyScope);
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();

    let handler =
        CairnMcpHandler::with_store_scope_and_sqlite(store_dyn, f.store, scope, cfg, f.scope_a);

    let names = tools_list_via_wire(handler).await;
    insta::assert_json_snapshot!("manifest_resolver_ok_empty", names);
}

/// Cell 6 — all conditions met: Stdio + `single_tenant=true` + `graph_edges=true`
/// + `resolver=OkNonEmpty`.  Must advertise all five graph.* tools.
#[tokio::test]
async fn manifest_matrix_all_conditions_met() {
    let handler = build_handler_single_tenant_graph_capable().await;
    let names = tools_list_via_wire(handler).await;
    insta::assert_json_snapshot!("manifest_all_conditions_met", names);
    // Invariant: all five graph.* tools must be present.
    for tool in &[
        "graph.get_entity",
        "graph.get_neighbors",
        "graph.query",
        "graph.surprising_connections",
        "graph.timeline",
    ] {
        assert!(
            names.contains(&tool.to_string()),
            "cell 6 must advertise {tool}; got: {names:?}"
        );
    }
}
