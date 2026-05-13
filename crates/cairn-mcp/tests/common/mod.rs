//! Shared helpers for MCP integration tests — newline-delimited JSON-RPC
//! framing over a `tokio::io::duplex` transport.
//!
//! `smoke.rs`, `init_status_parity.rs`, and `handler_rejection.rs` each carry
//! their own copy of these helpers. Issue #67 added this module for the new
//! `mcp_conformance` test; the older copies remain as-is.
//!
//! Tests reach this module via `#[path = "common/mod.rs"]` from
//! `mcp_conformance.rs` — Cargo integration tests are separate binaries and
//! a module imported from a sibling test file is the simplest way to share
//! without spinning up another crate.
#![allow(dead_code, missing_docs)]

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Write one newline-terminated JSON-RPC frame and flush.
pub async fn send_frame(writer: &mut (impl AsyncWriteExt + Unpin), json: &str) {
    writer
        .write_all(json.as_bytes())
        .await
        .expect("write frame");
    writer.write_all(b"\n").await.expect("write newline");
    writer.flush().await.expect("flush");
}

/// Read one newline-terminated JSON-RPC frame and parse it.
pub async fn recv_frame(
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read frame line");
    serde_json::from_str(line.trim()).expect("parse frame as JSON")
}

/// Send `initialize` (id=1), read response, send `notifications/initialized`.
pub async fn do_initialize(
    writer: &mut (impl AsyncWriteExt + Unpin),
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
) -> serde_json::Value {
    send_frame(
        writer,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"conformance-test","version":"0.0.0"}}}"#,
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
