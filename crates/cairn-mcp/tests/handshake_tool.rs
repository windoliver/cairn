//! Wire-protocol tests for the `handshake` MCP prelude tool (issue #65,
//! brief §8.0.a).
//!
//! These tests drive a real `CairnMcpHandler` over a `tokio::io::duplex`
//! transport, exercising the same code path a real MCP client would.
//!
//! Integration-test files are not public API; doc comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::domain::ScopeTuple;
use cairn_core::mcp_auth::{McpAuthContext, McpSessionScope, ScopeResolutionError};
use cairn_mcp::CairnMcpHandler;
use cairn_test_fixtures::graph::tiny_graph as tiny_graph_async;
use rmcp::ServiceExt as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Resolver that returns a fixed allowed-scope list. Defined at module
/// scope so clippy's `items_after_statements` does not fire inside the
/// async helper below.
struct StaticScope(Vec<ScopeTuple>);

impl McpSessionScope for StaticScope {
    fn allowed_scopes(
        &self,
        _ctx: &McpAuthContext<'_>,
    ) -> Result<Vec<ScopeTuple>, ScopeResolutionError> {
        Ok(self.0.clone())
    }
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

async fn do_initialize(
    writer: &mut (impl AsyncWriteExt + Unpin),
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
) -> serde_json::Value {
    send_frame(
        writer,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"handshake-test","version":"0.0.0"}}}"#,
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

async fn build_handler_wired() -> (CairnMcpHandler, Arc<cairn_store_sqlite::SqliteMemoryStore>) {
    let f = tiny_graph_async().await;
    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> = Arc::new(StaticScope(vec![f.scope_a.clone()]));
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();
    let store_sqlite = f.store.clone();
    (
        CairnMcpHandler::with_store_scope_and_sqlite(store_dyn, f.store, scope, cfg, f.scope_a),
        store_sqlite,
    )
}

/// `tools/list` advertises `handshake` when a sqlite store is wired.
#[tokio::test]
async fn handshake_tool_listed_when_sqlite_store_wired() {
    let (handler, _store) = build_handler_wired().await;
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
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    )
    .await;
    let resp = recv_frame(&mut client_reader).await;
    let names: Vec<&str> = resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|t| t.get("name").and_then(serde_json::Value::as_str))
        .collect();
    assert!(
        names.contains(&"handshake"),
        "tools/list must include handshake; got: {names:?}"
    );
}

/// `tools/list` omits `handshake` when no sqlite store is wired —
/// brief §15 fail-closed.
#[tokio::test]
async fn handshake_tool_absent_when_no_sqlite_store() {
    let handler = CairnMcpHandler::new();
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
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    )
    .await;
    let resp = recv_frame(&mut client_reader).await;
    let names: Vec<&str> = resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|t| t.get("name").and_then(serde_json::Value::as_str))
        .collect();
    assert!(
        !names.contains(&"handshake"),
        "unwired handler must NOT advertise handshake; got: {names:?}"
    );
}

/// Two consecutive `handshake` calls must return distinct nonces (brief
/// §8.0.a invariant d).
#[tokio::test]
async fn handshake_returns_distinct_nonces_per_call() {
    let (handler, _store) = build_handler_wired().await;
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

    let mut nonces = Vec::new();
    for id in 2..=3 {
        send_frame(
            &mut client_write,
            &format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"handshake","arguments":{{"issuer":"hmn:tester"}}}}}}"#
            ),
        )
        .await;
        let resp = recv_frame(&mut client_reader).await;
        let text = resp
            .pointer("/result/content/0/text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("missing content text in response: {resp}"));
        let payload: serde_json::Value =
            serde_json::from_str(text).expect("handshake content must be valid JSON");
        let nonce = payload
            .pointer("/challenge/nonce")
            .and_then(serde_json::Value::as_str)
            .expect("response must carry challenge.nonce")
            .to_owned();
        nonces.push(nonce);
    }

    assert_eq!(nonces.len(), 2);
    assert_ne!(
        nonces[0], nonces[1],
        "two back-to-back handshake calls must return distinct nonces"
    );
}

/// `handshake` with a wired sqlite store persists the nonce to
/// `outstanding_challenges` keyed by issuer (brief §4.2 — nonce is
/// redeemable by the next signed envelope from the same issuer).
#[tokio::test]
async fn handshake_persists_nonce_to_outstanding_challenges() {
    let (handler, store) = build_handler_wired().await;
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
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"handshake","arguments":{"issuer":"hmn:tafeng"}}}"#,
    )
    .await;
    let resp = recv_frame(&mut client_reader).await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("missing content text in response: {resp}"));
    let _payload: serde_json::Value =
        serde_json::from_str(text).expect("handshake response must be valid JSON");

    // Purge all rows with `now_ms = i64::MAX` (every row is past-expiry
    // relative to the cutoff) and assert exactly one row was deleted —
    // confirming the handshake call inserted a single outstanding challenge.
    // This avoids depending on crate-private SQL helpers from an
    // integration-test target.
    let purged = store
        .with_tx(move |tx| tx.purge_expired_challenges(i64::MAX))
        .await
        .expect("purge outstanding challenges");

    assert_eq!(
        purged, 1,
        "handshake must persist exactly one outstanding challenge for the issuer"
    );
}

/// `handshake` without an `issuer` argument fails closed with a structured
/// invalid-args error (single-use challenges must be keyed by an identity).
#[tokio::test]
async fn handshake_without_issuer_returns_invalid_args() {
    let (handler, _store) = build_handler_wired().await;
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
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"handshake","arguments":{}}}"#,
    )
    .await;
    let resp = recv_frame(&mut client_reader).await;
    let is_error = resp
        .pointer("/result/isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(
        is_error,
        "handshake without issuer must return isError=true; got: {resp}"
    );
}

/// `handshake` on an unwired handler returns capability-unavailable when the
/// tool is somehow invoked anyway (e.g. a client that ignores `tools/list`
/// and dispatches by name). Belt-and-suspenders against the listing gate.
#[tokio::test]
async fn handshake_unwired_handler_returns_capability_unavailable() {
    let handler = CairnMcpHandler::new();
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
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"handshake","arguments":{"issuer":"hmn:x"}}}"#,
    )
    .await;
    let resp = recv_frame(&mut client_reader).await;
    let is_error = resp
        .pointer("/result/isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        is_error,
        "unwired handshake call must return isError=true; got: {resp}"
    );
    assert!(
        text.contains("capability unavailable"),
        "error text should mention capability unavailability; got: {text}"
    );
}
