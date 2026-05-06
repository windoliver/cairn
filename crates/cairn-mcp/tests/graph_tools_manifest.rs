//! Integration tests for [`cairn_mcp::graph_tools`].
//!
//! Task 11: registry names and `GetEntityArgs` deserialisation.
//! Task 12: `dispatch` routes `graph.get_entity` to a real store.
//! Task 13: `call_tool` routes graph tools through `materialize_graph_request`.
//! Task 14: `list_tools` advertises graph tools when available.
#![allow(missing_docs)]

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

    let scope: Arc<dyn McpSessionScope> =
        Arc::new(StaticScope::new(vec![f.scope_a.clone()]));

    // The trait-object store (verb path) wraps the same sqlite store.
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();

    CairnMcpHandler::with_store_scope_and_sqlite(
        store_dyn,
        f.store,
        scope,
        cfg,
        f.scope_a,
    )
}

/// Build a handler where the scope resolver returns an empty Vec.
async fn build_handler_resolver_returns_empty() -> CairnMcpHandler {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> = Arc::new(EmptyScope);
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();

    CairnMcpHandler::with_store_scope_and_sqlite(
        store_dyn,
        f.store,
        scope,
        cfg,
        f.scope_a,
    )
}

/// Build a handler where the scope resolver always errors.
async fn build_handler_resolver_errors() -> CairnMcpHandler {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> = Arc::new(ErrorScope);
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();

    CairnMcpHandler::with_store_scope_and_sqlite(
        store_dyn,
        f.store,
        scope,
        cfg,
        f.scope_a,
    )
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
