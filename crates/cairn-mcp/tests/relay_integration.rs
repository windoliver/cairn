//! Integration tests for `serve_stdio_with_store_io` (Plan C Task 19).
//!
//! These tests drive the internal I/O helper directly so they can work
//! fully in-process without touching real stdin/stdout.  They verify:
//!
//! 1. A blank line followed by a valid `initialize` request completes the
//!    MCP handshake — i.e. the relay wiring works end-to-end.
//! 2. With a graph-capable store, `tools/list` includes `graph.*` entries.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::domain::ScopeTuple;
use cairn_core::mcp_auth::{McpAuthContext, McpSessionScope, ScopeResolutionError};
use cairn_mcp::serve_stdio_with_store_io;
use cairn_test_fixtures::graph::tiny_graph as tiny_graph_async;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// ── Shared helpers ───────────────────────────────────────────────────────────

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

/// Write one newline-terminated JSON-RPC frame.
async fn send_frame(writer: &mut (impl AsyncWriteExt + Unpin), json: &str) {
    writer.write_all(json.as_bytes()).await.expect("write frame");
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

/// Perform the MCP initialize handshake and return the `initialize` response.
///
/// Also sends a blank line before the initialize request to verify the relay
/// drops it (the main point of Task 19).
async fn do_initialize_with_blank(
    writer: &mut (impl AsyncWriteExt + Unpin),
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
) -> serde_json::Value {
    // Send a blank line first — relay must drop it, not forward to rmcp.
    writer.write_all(b"\n").await.expect("write blank line");
    writer.flush().await.expect("flush blank");

    send_frame(
        writer,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"relay-integration-test","version":"0.0.0"}}}"#,
    )
    .await;

    let resp = recv_frame(reader).await;

    send_frame(
        writer,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await;

    resp
}

// ── Test 1: initialize handshake through relay ───────────────────────────────

/// Pipe a blank line + initialize request through `serve_stdio_with_store_io`
/// via duplex streams; assert the server completes the handshake.
#[tokio::test(flavor = "current_thread")]
async fn serve_with_relay_completes_initialize_handshake() {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> =
        Arc::new(StaticScope::new(vec![f.scope_a.clone()]));
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();
    let principal = f.scope_a.clone();

    // server_input / server_output are the server-side pipe halves;
    // client_write / client_read are the client-side halves.
    let (server_input, mut client_write) = tokio::io::duplex(65_536);
    let (mut client_read_half, server_output) = tokio::io::duplex(65_536);

    let _server_task = tokio::spawn(async move {
        serve_stdio_with_store_io(
            store_dyn,
            f.store,
            scope,
            cfg,
            principal,
            server_input,
            server_output,
        )
        .await
        .ok();
    });

    let mut client_reader = BufReader::new(&mut client_read_half);

    let init_resp = do_initialize_with_blank(&mut client_write, &mut client_reader).await;

    // The server must return its name as "cairn".
    let name = init_resp
        .pointer("/result/serverInfo/name")
        .and_then(|v| v.as_str())
        .expect("result.serverInfo.name must be present");
    assert_eq!(name, "cairn", "server must identify itself as 'cairn'");

    // Tools capability must be advertised.
    assert!(
        init_resp.pointer("/result/capabilities/tools").is_some(),
        "initialize response must advertise tools capability; got: {init_resp}"
    );
}

// ── Test 2: tools/list contains graph.* entries with graph-capable store ─────

/// Send `tools/list` through `serve_stdio_with_store_io` using a graph-capable
/// store; assert the response includes at least one `graph.*` tool.
#[tokio::test(flavor = "current_thread")]
async fn serve_with_relay_tools_list_includes_graph_tools() {
    let f = tiny_graph_async().await;

    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> =
        Arc::new(StaticScope::new(vec![f.scope_a.clone()]));
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();
    let principal = f.scope_a.clone();

    let (server_input, mut client_write) = tokio::io::duplex(65_536);
    let (mut client_read_half, server_output) = tokio::io::duplex(65_536);

    let _server_task = tokio::spawn(async move {
        serve_stdio_with_store_io(
            store_dyn,
            f.store,
            scope,
            cfg,
            principal,
            server_input,
            server_output,
        )
        .await
        .ok();
    });

    let mut client_reader = BufReader::new(&mut client_read_half);

    do_initialize_with_blank(&mut client_write, &mut client_reader).await;

    // Send tools/list request.
    send_frame(
        &mut client_write,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    )
    .await;

    let tools_resp = recv_frame(&mut client_reader).await;
    let tools = tools_resp
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .expect("result.tools must be a JSON array");

    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();

    let has_graph_tool = names.iter().any(|n| n.starts_with("graph."));
    assert!(
        has_graph_tool,
        "tools/list must include at least one graph.* tool with a graph-capable store; got: {names:?}"
    );
}
