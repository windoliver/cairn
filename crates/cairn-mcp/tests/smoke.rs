//! MCP transport smoke tests.
//!
//! Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_mcp::error::TransportError;
use cairn_mcp::generated::TOOLS;
use cairn_mcp::handler::{CairnMcpHandler, dispatch_stub};

#[test]
fn transport_error_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TransportError>();
}

#[test]
fn transport_error_io_display() {
    let e = TransportError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "broken pipe",
    ));
    assert!(e.to_string().contains("stdio I/O error"));
}

#[test]
fn transport_error_service_display() {
    let e = TransportError::Service("service failed".to_string());
    assert!(e.to_string().contains("MCP service error"));
    assert!(e.to_string().contains("service failed"));
}

// ── CairnMcpHandler ─────────────────────────────────────────────────────────

#[test]
fn handler_is_default_and_new() {
    // Verify both construction paths compile and produce the same type.
    let a = CairnMcpHandler;
    let b = CairnMcpHandler::new();
    // Unit struct — Debug repr is identical.
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
}

#[test]
fn handler_get_info_advertises_tools() {
    use rmcp::ServerHandler as _;
    let h = CairnMcpHandler::new();
    let info = h.get_info();
    assert!(
        info.capabilities.tools.is_some(),
        "server info must advertise tools capability"
    );
    assert_eq!(info.server_info.name, "cairn");
}

#[test]
fn list_tools_returns_eight_verbs() {
    // TOOLS has 8 entries (one per verb).  The conversion logic in
    // `list_tools` iterates TOOLS, so verifying the count here is
    // sufficient without needing a live RequestContext.
    assert_eq!(TOOLS.len(), 8, "TOOLS must contain exactly 8 verbs");
}

#[test]
fn tools_list_matches_generated_tools_constant() {
    // The handler converts TOOLS 1-for-1; verify the verb names are
    // the canonical set from the design brief §8.
    let names: Vec<&str> = TOOLS.iter().map(|d| d.name).collect();
    for expected in &[
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
    ] {
        assert!(
            names.contains(expected),
            "TOOLS must include verb '{expected}'"
        );
    }
}

#[test]
fn dispatch_stub_is_error_result() {
    let result = dispatch_stub("ingest");
    assert_eq!(
        result.is_error,
        Some(true),
        "dispatch_stub must set is_error = true"
    );
    assert!(
        !result.content.is_empty(),
        "dispatch_stub must include content"
    );
    let text = format!("{:?}", result.content);
    assert!(
        text.contains("ingest"),
        "dispatch_stub content must mention the verb name"
    );
    assert!(
        text.contains("not yet implemented"),
        "dispatch_stub content must mention the placeholder message"
    );
}

// ── Wire-protocol tests (Approach A: tokio::io::duplex + raw JSON-RPC frames) ──
//
// rmcp's `transport-async-rw` feature (included with `server`) implements
// `IntoTransport` for any `AsyncRead + AsyncWrite`, so `tokio::io::DuplexStream`
// works as an in-process transport.  We write newline-delimited JSON-RPC frames
// from the client half and read back raw JSON lines — avoiding a dependency on
// rmcp's `client` feature which is not enabled in this workspace.
//
// Frame format: one JSON object per line (b'\n' terminated), matching the
// `JsonRpcMessageCodec` in rmcp/src/transport/async_rw.rs.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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

/// Perform the MCP initialize handshake and return the `initialize` response.
///
/// Sends the `initialize` request (id 1), reads the response, then sends the
/// `notifications/initialized` notification (no response expected).
async fn do_initialize(
    writer: &mut (impl AsyncWriteExt + Unpin),
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
) -> serde_json::Value {
    send_frame(
        writer,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke-test","version":"0.0.0"}}}"#,
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

/// Verify the server reports `name = "cairn"` and includes a tools capability
/// in the `initialize` response (brief §8.0.a).
#[tokio::test]
async fn wire_initialize_server_info_name() {
    use rmcp::ServiceExt as _;

    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        CairnMcpHandler::new()
            .serve(server_half)
            .await
            .expect("server init")
            .waiting()
            .await
            .ok();
    });

    let (client_read, mut client_write) = tokio::io::split(client_half);
    let mut client_reader = BufReader::new(client_read);

    let init_resp = do_initialize(&mut client_write, &mut client_reader).await;

    let name = init_resp
        .pointer("/result/serverInfo/name")
        .and_then(|v| v.as_str())
        .expect("result.serverInfo.name must be present");
    assert_eq!(name, "cairn", "server must identify itself as 'cairn'");
}

/// Verify the `initialize` response advertises a `tools` capability.
#[tokio::test]
async fn wire_initialize_advertises_tools_capability() {
    use rmcp::ServiceExt as _;

    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        CairnMcpHandler::new()
            .serve(server_half)
            .await
            .expect("server init")
            .waiting()
            .await
            .ok();
    });

    let (client_read, mut client_write) = tokio::io::split(client_half);
    let mut client_reader = BufReader::new(client_read);

    let init_resp = do_initialize(&mut client_write, &mut client_reader).await;

    // The tools capability object must be present (may be an empty object or
    // contain `listChanged`; what matters is the key exists).
    let tools_cap = init_resp.pointer("/result/capabilities/tools");
    assert!(
        tools_cap.is_some(),
        "initialize response must advertise tools capability; got: {init_resp}"
    );
}

/// After `initialize` + `notifications/initialized`, send `tools/list` and
/// verify the response contains exactly 8 verb tools (brief §8).
#[tokio::test]
async fn wire_tools_list_returns_eight_verbs() {
    use rmcp::ServiceExt as _;

    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        CairnMcpHandler::new()
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

    assert_eq!(
        tools.len(),
        8,
        "tools/list must return exactly 8 verbs; got: {tools_resp}"
    );

    // Verify all canonical verb names are present.
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();
    for verb in &[
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
    ] {
        assert!(
            names.contains(verb),
            "tools/list must include verb '{verb}'; got names: {names:?}"
        );
    }
}

/// Calling an unknown verb via `tools/call` must return a successful JSON-RPC
/// response with `is_error = true` in the tool result (MCP error-in-result
/// convention), not a JSON-RPC error frame.
#[tokio::test]
async fn wire_call_tool_unknown_verb_returns_mcp_error() {
    use rmcp::ServiceExt as _;

    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        CairnMcpHandler::new()
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
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nonexistent_verb","arguments":{}}}"#,
    )
    .await;

    let call_resp = recv_frame(&mut client_reader).await;

    // Must be a JSON-RPC *response* (has "result"), not a JSON-RPC error.
    assert!(
        call_resp.get("result").is_some(),
        "unknown verb call must yield a result frame, not a JSON-RPC error; got: {call_resp}"
    );

    // Within the result, `isError` must be true (MCP error-in-result).
    let is_error = call_resp
        .pointer("/result/isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(
        is_error,
        "unknown verb call result must have isError=true; got: {call_resp}"
    );
}
