//! Wire-protocol tests for the `handshake` MCP prelude tool (issue #65,
//! brief §8.0.a).
//!
//! Coverage matrix:
//!  - **Wire-level** (`tokio::io::duplex` driving a real `CairnMcpHandler`)
//!    tests assert the production posture against the actual
//!    [`cairn_core::status::wiring::REPLAY_CHALLENGE_WIRED`] constant.
//!    While that flag is `false`, `handshake` MUST be absent from
//!    `tools/list` and any direct call MUST return a
//!    `CapabilityUnavailable` error (round-1 review #1, brief §15
//!    fail-closed).
//!  - **Direct-dispatch** tests bypass the production gate by calling
//!    [`cairn_mcp::prelude_tools::dispatch`] with
//!    `replay_challenge_wired = true`, exercising the mint /
//!    persistence path so the implementation stays covered while the
//!    wire-level posture stays gated. When a follow-up flips
//!    `REPLAY_CHALLENGE_WIRED` to `true`, the wire-level tests
//!    automatically start asserting the wired behavior.
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

// ── Wire-level (production-posture) tests ──────────────────────────────────
//
// These read the live `REPLAY_CHALLENGE_WIRED` flag and branch on it. While
// the flag is `false` (current state) they pin the gated posture; when a
// follow-up flips the flag they automatically start asserting the wired
// posture. No test is left dormant — failure to keep both branches
// passing means a regression.

/// `tools/list` must reflect the live wiring flag. While
/// `REPLAY_CHALLENGE_WIRED` is `false`, `handshake` is absent even when a
/// sqlite store is wired (round-1 review #1).
#[tokio::test]
async fn handshake_listing_follows_replay_challenge_wiring() {
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
    if cairn_core::status::wiring::REPLAY_CHALLENGE_WIRED {
        assert!(
            names.contains(&"handshake"),
            "REPLAY_CHALLENGE_WIRED=true must list handshake; got: {names:?}"
        );
    } else {
        assert!(
            !names.contains(&"handshake"),
            "REPLAY_CHALLENGE_WIRED=false must hide handshake; got: {names:?}"
        );
    }
}

/// `tools/list` omits `handshake` when no sqlite store is wired,
/// regardless of the wiring flag — both gates are required.
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

/// A direct `tools/call name=handshake` over the wire path must return an
/// MCP error-in-result that names the held-back capability when
/// `REPLAY_CHALLENGE_WIRED` is `false`. Belt-and-suspenders against
/// clients that ignore the `tools/list` gate and dispatch by name.
#[tokio::test]
async fn handshake_call_returns_capability_unavailable_when_replay_unwired() {
    if cairn_core::status::wiring::REPLAY_CHALLENGE_WIRED {
        // The flag has flipped; this test no longer applies. Sibling
        // direct-dispatch tests cover the wired path.
        return;
    }
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
        "gated handshake call must return isError=true; got: {resp}"
    );
    assert!(
        text.contains("cairn.mcp.v1.replay.challenge"),
        "error text must name the held-back capability; got: {text}"
    );
}

// ── Direct-dispatch (implementation-coverage) tests ────────────────────────
//
// These call `prelude_tools::dispatch` directly with
// `replay_challenge_wired = true` so the mint / persistence / argument
// validation paths stay covered while the wire-level gate is closed. They
// do not use rmcp transports — argument validation, store interaction,
// and result shape are pure functions of the dispatch helper.

/// Two back-to-back `dispatch` invocations must return distinct nonces
/// (brief §8.0.a invariant d).
#[tokio::test]
async fn dispatch_returns_distinct_nonces_per_call() {
    let (_handler, store) = build_handler_wired().await;
    let mut args = serde_json::Map::new();
    args.insert(
        "issuer".into(),
        serde_json::Value::String("hmn:tester".into()),
    );

    let mut nonces: Vec<String> = Vec::with_capacity(2);
    for _ in 0..2 {
        let result = cairn_mcp::prelude_tools::dispatch(
            "handshake",
            Some(args.clone()),
            Some(store.clone()),
            true,
        )
        .await;
        assert!(
            !result.is_error.unwrap_or(false),
            "dispatch with replay_challenge_wired=true must succeed; got: {result:?}"
        );
        let text = result
            .content
            .first()
            .and_then(|c| match &**c {
                rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .expect("text content");
        let payload: serde_json::Value =
            serde_json::from_str(&text).expect("handshake content must be valid JSON");
        assert_eq!(
            payload
                .pointer("/contract")
                .and_then(serde_json::Value::as_str),
            Some("cairn.mcp.v1")
        );
        let nonce = payload
            .pointer("/challenge/nonce")
            .and_then(serde_json::Value::as_str)
            .expect("response must carry challenge.nonce")
            .to_owned();
        nonces.push(nonce);
    }
    assert_ne!(
        nonces[0], nonces[1],
        "two back-to-back handshake calls must return distinct nonces"
    );
}

/// `dispatch` persists exactly one row to `outstanding_challenges` per
/// successful call (brief §4.2 — nonce is redeemable by the next signed
/// envelope from the same issuer).
#[tokio::test]
async fn dispatch_persists_nonce_to_outstanding_challenges() {
    let (_handler, store) = build_handler_wired().await;
    let mut args = serde_json::Map::new();
    args.insert(
        "issuer".into(),
        serde_json::Value::String("hmn:tafeng".into()),
    );

    let result =
        cairn_mcp::prelude_tools::dispatch("handshake", Some(args), Some(store.clone()), true)
            .await;
    assert!(
        !result.is_error.unwrap_or(false),
        "dispatch with replay_challenge_wired=true must succeed; got: {result:?}"
    );

    // Purge with `now_ms = i64::MAX` deletes every row regardless of TTL —
    // the count of rows deleted is the count minted by the dispatch above.
    let purged = store
        .with_tx(move |tx| tx.purge_expired_challenges(i64::MAX))
        .await
        .expect("purge outstanding challenges");

    assert_eq!(
        purged, 1,
        "dispatch must persist exactly one outstanding challenge for the issuer"
    );
}

/// `dispatch` with no `issuer` argument fails closed with a structured
/// invalid-args error.
#[tokio::test]
async fn dispatch_without_issuer_returns_invalid_args() {
    let (_handler, store) = build_handler_wired().await;
    let result = cairn_mcp::prelude_tools::dispatch(
        "handshake",
        Some(serde_json::Map::new()),
        Some(store.clone()),
        true,
    )
    .await;
    assert_eq!(
        result.is_error,
        Some(true),
        "missing issuer must return isError=true"
    );
}

/// `dispatch` without a wired sqlite store fails closed even when
/// `replay_challenge_wired = true` — the persistence step requires a
/// store handle.
#[tokio::test]
async fn dispatch_without_sqlite_store_returns_capability_unavailable() {
    let mut args = serde_json::Map::new();
    args.insert("issuer".into(), serde_json::Value::String("hmn:x".into()));
    let result = cairn_mcp::prelude_tools::dispatch("handshake", Some(args), None, true).await;
    assert_eq!(
        result.is_error,
        Some(true),
        "no sqlite store must return isError=true"
    );
}

/// `dispatch` with `replay_challenge_wired = false` returns
/// capability-unavailable for the held-back replay-challenge capability.
/// Pins the central gate without depending on the `REPLAY_CHALLENGE_WIRED`
/// const's current value.
#[tokio::test]
async fn dispatch_returns_capability_unavailable_when_flag_false() {
    let (_handler, store) = build_handler_wired().await;
    let mut args = serde_json::Map::new();
    args.insert("issuer".into(), serde_json::Value::String("hmn:x".into()));
    let result =
        cairn_mcp::prelude_tools::dispatch("handshake", Some(args), Some(store.clone()), false)
            .await;
    assert_eq!(result.is_error, Some(true));
    let text = result
        .content
        .first()
        .and_then(|c| match &**c {
            rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(
        text.contains("cairn.mcp.v1.replay.challenge"),
        "error text must name the held-back capability; got: {text}"
    );
}
