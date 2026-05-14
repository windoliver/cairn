//! Initialize ↔ status parity + extension-namespace gating (issue #65,
//! brief §8.0.a).
//!
//! These tests cover the issue's three acceptance criteria:
//!  - (a) Initialize output and status agree on protocol version and
//!    capability set.
//!  - (b) covered in `handshake_tool.rs`.
//!  - (c) Unsupported extension tools are absent or explicitly unavailable.
//!
//! Integration-test files are not public API; doc comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::domain::ScopeTuple;
use cairn_core::mcp_auth::{McpAuthContext, McpSessionScope, ScopeResolutionError};
use cairn_core::status::default_sensor_capabilities;
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

async fn send_frame(writer: &mut (impl AsyncWriteExt + Unpin), json: &str) {
    writer
        .write_all(json.as_bytes())
        .await
        .expect("write frame");
    writer.write_all(b"\n").await.expect("write newline");
    writer.flush().await.expect("flush");
}

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
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"parity-test","version":"0.0.0"}}}"#,
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

async fn build_handler_wired() -> CairnMcpHandler {
    let f = tiny_graph_async().await;
    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(f.scope_a.clone());

    let scope: Arc<dyn McpSessionScope> = Arc::new(StaticScope(vec![f.scope_a.clone()]));
    let store_dyn: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = f.store.clone();
    CairnMcpHandler::with_store_scope_and_sqlite(store_dyn, f.store, scope, cfg, f.scope_a)
}

// ── AC (a): initialize == status agreement ──────────────────────────────────

/// `initialize.experimental["cairn.status"].contract` equals the
/// canonical `cairn.mcp.v1` contract id.
#[tokio::test]
async fn initialize_advertises_canonical_contract_id() {
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
    let resp = do_initialize(&mut client_write, &mut client_reader).await;

    let contract = resp
        .pointer("/result/capabilities/experimental/cairn.status/contract")
        .and_then(serde_json::Value::as_str);
    assert_eq!(contract, Some("cairn.mcp.v1"));
}

/// The capability set advertised in `initialize` must equal what the
/// handler's `status_response()` reports — same handler instance, same
/// capabilities. Volatile fields (`incarnation`, `started_at`) differ
/// per call so we compare only `capabilities` and `extensions`.
#[tokio::test]
async fn initialize_capabilities_match_status_response() {
    let handler = build_handler_wired().await;
    // Snapshot the in-process status response BEFORE moving the handler
    // onto the rmcp transport — same instance, same advertise() output.
    let local_status = handler.status_response();

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
    let resp = do_initialize(&mut client_write, &mut client_reader).await;

    let wire_caps = resp
        .pointer("/result/capabilities/experimental/cairn.status/capabilities")
        .and_then(serde_json::Value::as_array)
        .expect("wire capabilities array")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    let local_caps = local_status
        .capabilities
        .iter()
        .map(|c| {
            serde_json::to_value(c)
                .expect("serialize capability")
                .as_str()
                .map(ToOwned::to_owned)
                .expect("capability serializes to string")
        })
        .collect::<Vec<_>>();

    assert_eq!(
        wire_caps, local_caps,
        "initialize.experimental.cairn.status.capabilities must equal handler.status_response().capabilities"
    );

    // Extensions agreement.
    let wire_exts = resp
        .pointer("/result/capabilities/experimental/cairn.status/extensions")
        .and_then(serde_json::Value::as_array)
        .expect("wire extensions array");
    assert_eq!(
        wire_exts.len(),
        local_status.extensions.len(),
        "initialize.extensions length must match status_response().extensions"
    );
}

// ── AC (c): extension namespaces — capability-gated ─────────────────────────

const EXTENSION_NAMESPACE_PREFIXES: &[&str] = &[
    "cairn.aggregate.",
    "cairn.admin.",
    "cairn.federation.",
    "cairn.sessiontree.",
];

/// Default P0 config must NOT advertise any extension verbs in
/// `tools/list`. Brief §8.0.a: extensions are opt-in via
/// `.cairn/config.yaml` and gated by capability negotiation.
#[tokio::test]
async fn tools_list_omits_extension_namespaces_under_default_config() {
    let handler = build_handler_wired().await;
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

    for prefix in EXTENSION_NAMESPACE_PREFIXES {
        for name in &names {
            assert!(
                !name.starts_with(prefix),
                "extension verb '{name}' must not appear in tools/list under default \
                 config (matched prefix '{prefix}'); full list: {names:?}"
            );
        }
    }
}

/// Calling an extension verb directly must fail with an MCP
/// error-in-result, not silently succeed. Belt-and-suspenders — even if
/// a malicious or buggy client ignores `tools/list` and dispatches by
/// name, the server fails closed.
#[tokio::test]
async fn calling_unadvertised_extension_verb_returns_mcp_error() {
    let handler = build_handler_wired().await;
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

    // Call a representative extension verb from each namespace.
    for verb in &[
        "cairn.aggregate.agent_summary",
        "cairn.admin.snapshot",
        "cairn.federation.propose_share",
        "cairn.sessiontree.fork_session",
    ] {
        send_frame(
            &mut client_write,
            &format!(
                r#"{{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{{"name":"{verb}","arguments":{{}}}}}}"#
            ),
        )
        .await;
        let resp = recv_frame(&mut client_reader).await;
        let is_error = resp
            .pointer("/result/isError")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        assert!(
            is_error,
            "extension verb {verb} must return isError=true; got: {resp}"
        );
    }
}

/// `initialize`'s `extensions` array must be empty under default config —
/// brief §8.0.a wire shape `"extensions": []`.
#[tokio::test]
async fn initialize_extensions_array_empty_under_default_config() {
    let handler = build_handler_wired().await;
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
    let resp = do_initialize(&mut client_write, &mut client_reader).await;

    let exts = resp
        .pointer("/result/capabilities/experimental/cairn.status/extensions")
        .and_then(serde_json::Value::as_array)
        .expect("extensions array");
    assert!(
        exts.is_empty(),
        "initialize.extensions must be empty under default config; got: {exts:?}"
    );
}

// ── Sanity: unwired handler still advertises a status block ────────────────

/// Even an unwired handler emits the cairn.status block in initialize so
/// negotiating clients can see contract version + compiled local sensor
/// capabilities that do not require a store binding.
#[test]
fn unwired_status_response_is_well_formed() {
    let handler = CairnMcpHandler::new();
    let status = handler.status_response();
    assert_eq!(status.contract, "cairn.mcp.v1");
    assert_eq!(status.capabilities, default_sensor_capabilities());
    assert!(
        status.extensions.is_empty(),
        "unwired handler must advertise zero extensions"
    );
}
