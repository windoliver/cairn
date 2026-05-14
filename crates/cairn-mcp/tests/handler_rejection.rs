//! MCP capability-rejection envelope (issue #53).
//!
//! Drives `handle_search` (via the full wire protocol) to a
//! `CapabilityUnavailable` rejection and asserts that the `CallToolResult`
//! text is a well-formed §8.0.b structured envelope carrying the capability
//! string, `error.code == "CapabilityUnavailable"`, and the remediation hint.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, Edge, EdgeDir, EdgeKey, KeywordSearchArgs, KeywordSearchPage, ListArgs,
    ListPage, MemoryStore, MemoryStoreCapabilities, RecordVersion, StoreError, TombstoneReason,
    UpsertOutcome,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::record::MemoryRecord;
use cairn_core::domain::{RecordId, TargetId};
use cairn_mcp::CairnMcpHandler;
use rmcp::ServiceExt as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// ── Minimal stub store: fts=true, vector=false ───────────────────────────────

/// Minimal `MemoryStore` stub for rejection tests. Reports `fts=true,
/// vector=false` so that `semantic` mode hits the `CapabilityUnavailable`
/// path. Methods beyond `name`/`capabilities`/`supported_contract_versions`
/// and `search_keyword` are unreachable from the status/dispatch path exercised
/// here.
struct RejectionStubStore;

#[async_trait::async_trait]
impl MemoryStore for RejectionStubStore {
    fn name(&self) -> &'static str {
        "rejection-stub"
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: true,
            vector: false,
            graph_edges: false,
            transactions: false,
            per_record_consent_model: true,
            graph_search: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(
            CONTRACT_VERSION,
            ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
        )
    }

    async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        panic!("unreachable in rejection test: upsert")
    }

    async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
        panic!("unreachable in rejection test: get")
    }

    async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
        panic!("unreachable in rejection test: list")
    }

    async fn tombstone(&self, _id: &RecordId, _reason: TombstoneReason) -> Result<(), StoreError> {
        panic!("unreachable in rejection test: tombstone")
    }

    async fn versions(&self, _target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
        panic!("unreachable in rejection test: versions")
    }

    async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
        panic!("unreachable in rejection test: put_edge")
    }

    async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
        panic!("unreachable in rejection test: remove_edge")
    }

    async fn neighbours(&self, _id: &RecordId, _dir: EdgeDir) -> Result<Vec<Edge>, StoreError> {
        panic!("unreachable in rejection test: neighbours")
    }

    async fn search_keyword(
        &self,
        _args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError> {
        panic!("unreachable in rejection test: search_keyword")
    }
}

// ── Wire-protocol helpers ─────────────────────────────────────────────────────

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
) {
    send_frame(
        writer,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"rejection-test","version":"0.0.0"}}}"#,
    )
    .await;
    let _resp = recv_frame(reader).await;
    send_frame(
        writer,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await;
}

// ── Test ──────────────────────────────────────────────────────────────────────

/// When the store advertises `fts=true, vector=false`, calling `search` with
/// `mode=semantic` must return a `CallToolResult` with `isError=true` whose
/// text content is a structured §8.0.b `CapabilityUnavailable` JSON envelope
/// carrying:
/// - `error.code == "CapabilityUnavailable"`,
/// - `error.data.capability == "cairn.mcp.v1.search.semantic"`, and
/// - `error.data.remediation` containing the keyword `local_embeddings`.
///
/// This pins the `CapabilityUnavailable` arm of `handle_search` and catches
/// two classes of bug:
/// 1. MCP derivation uses the wrong config field (e.g., `config.search.local_embeddings`
///    instead of `store.vector`) when deciding whether to grant semantic capability.
/// 2. The handler degrades the structured JSON envelope back to a plain-text message.
#[tokio::test]
async fn mcp_search_semantic_rejection_carries_remediation() {
    let (server_half, client_half) = tokio::io::duplex(65_536);

    let _server_task = tokio::spawn(async move {
        CairnMcpHandler::with_store(Arc::new(RejectionStubStore), CairnConfig::default())
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

    // Request semantic search — this must be rejected because vector=false.
    send_frame(
        &mut client_write,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"query":"anything","mode":"semantic"}}}"#,
    )
    .await;

    let call_resp = recv_frame(&mut client_reader).await;

    // Must be a result frame (not a JSON-RPC error).
    assert!(
        call_resp.get("result").is_some(),
        "semantic rejection must yield a result frame (not JSON-RPC error); got: {call_resp}"
    );

    // `isError` must be true.
    let is_error = call_resp
        .pointer("/result/isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(
        is_error,
        "semantic search with vector=false must return isError=true; got: {call_resp}"
    );

    // Extract the text content and parse it as a structured §8.0.b envelope.
    let content = call_resp
        .pointer("/result/content")
        .and_then(|v| v.as_array())
        .expect("result.content must be an array");
    assert!(!content.is_empty(), "content must not be empty");

    let text = content[0]
        .get("text")
        .and_then(serde_json::Value::as_str)
        .expect("first content item must have a text field");

    // The text must be a valid JSON object (structured envelope), not plain text.
    let envelope: serde_json::Value =
        serde_json::from_str(text).expect("rejection text must be a valid JSON envelope");

    // §8.0.b: contract, verb, status, operation_id must be present.
    assert_eq!(
        envelope["contract"], "cairn.mcp.v1",
        "envelope.contract must be 'cairn.mcp.v1'; got: {envelope}"
    );
    assert_eq!(
        envelope["verb"], "search",
        "envelope.verb must be 'search'; got: {envelope}"
    );
    assert_eq!(
        envelope["status"], "rejected",
        "envelope.status must be 'rejected'; got: {envelope}"
    );
    assert!(
        envelope
            .get("operation_id")
            .and_then(|v| v.as_str())
            .is_some(),
        "envelope.operation_id must be present; got: {envelope}"
    );

    // error.code must be CapabilityUnavailable.
    assert_eq!(
        envelope["error"]["code"], "CapabilityUnavailable",
        "error.code must be 'CapabilityUnavailable'; got: {envelope}"
    );

    // error.data.capability must be the exact wire id.
    assert_eq!(
        envelope["error"]["data"]["capability"], "cairn.mcp.v1.search.semantic",
        "error.data.capability must be 'cairn.mcp.v1.search.semantic'; got: {envelope}"
    );

    // error.data.remediation must carry the local_embeddings hint.
    let remediation = envelope["error"]["data"]["remediation"]
        .as_str()
        .expect("error.data.remediation must be a string");
    assert!(
        remediation.contains("local_embeddings"),
        "error.data.remediation must contain 'local_embeddings'; got: {remediation:?}"
    );
}
