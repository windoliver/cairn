//! MCP search tool dispatch tests.
//!
//! Tests the `CairnMcpHandler::with_store` path via the full wire protocol
//! (`tokio::io::duplex` + raw JSON-RPC frames), mirroring the approach used in
//! `smoke.rs`. This validates that when a store is wired the `search` tool
//! returns a successful JSON envelope, and when no store is wired it falls
//! back to the stub error.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::CONTRACT_VERSION;
use cairn_core::contract::memory_store::{
    Edge, EdgeDir, EdgeKey, HybridSearchArgs, HybridSearchPage, KeywordSearchArgs,
    KeywordSearchPage, ListArgs, ListPage, MemoryStore, MemoryStoreCapabilities, RecordVersion,
    SemanticSearchArgs, SemanticSearchPage, StoreError, TombstoneReason, UpsertOutcome,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::record::MemoryRecord;
use cairn_core::domain::{RecordId, TargetId};
use cairn_core::generated::verbs::search::SearchData;
use cairn_mcp::handler::CairnMcpHandler;
use rmcp::ServiceExt as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Minimal stub store: returns empty pages for all three search legs.
struct EmptyStore;

#[async_trait::async_trait]
impl MemoryStore for EmptyStore {
    fn name(&self) -> &'static str {
        "empty"
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: true,
            vector: true,
            graph_edges: false,
            transactions: true,
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
        unimplemented!("EmptyStore: upsert not used in search tests")
    }

    async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
        Ok(None)
    }

    async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
        Ok(ListPage {
            records: vec![],
            next_cursor: None,
        })
    }

    async fn tombstone(&self, _id: &RecordId, _reason: TombstoneReason) -> Result<(), StoreError> {
        unimplemented!("EmptyStore: tombstone not used in search tests")
    }

    async fn versions(&self, _target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
        Ok(vec![])
    }

    async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
        unimplemented!("EmptyStore: put_edge not used in search tests")
    }

    async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
        Ok(false)
    }

    async fn neighbours(&self, _id: &RecordId, _dir: EdgeDir) -> Result<Vec<Edge>, StoreError> {
        Ok(vec![])
    }

    async fn search_keyword(
        &self,
        _args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError> {
        Ok(KeywordSearchPage {
            candidates: vec![],
            next_cursor: None,
            explain: None,
        })
    }

    async fn search_semantic(
        &self,
        _args: &SemanticSearchArgs<'_>,
    ) -> Result<SemanticSearchPage, StoreError> {
        Ok(SemanticSearchPage {
            candidates: vec![],
            explain: None,
        })
    }

    async fn search_hybrid(
        &self,
        _args: &HybridSearchArgs<'_>,
    ) -> Result<HybridSearchPage, StoreError> {
        Ok(HybridSearchPage {
            candidates: vec![],
            explain: None,
        })
    }
}

// ── Wire-protocol helpers (mirrored from smoke.rs) ──────────────────────────

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
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"search-test","version":"0.0.0"}}}"#,
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

// ── Tests ────────────────────────────────────────────────────────────────────

/// When a store is wired, the `search` tool with mode `keyword` returns a
/// successful result whose content is a valid JSON envelope with `hits`.
#[tokio::test]
async fn search_tool_with_store_returns_success_envelope() {
    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        CairnMcpHandler::with_store(Arc::new(EmptyStore), CairnConfig::default())
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
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello","mode":"keyword","limit":10}}}"#,
    )
    .await;

    let call_resp = recv_frame(&mut client_reader).await;

    // Must be a result frame (not a JSON-RPC error).
    assert!(
        call_resp.get("result").is_some(),
        "search call must yield a result frame; got: {call_resp}"
    );

    // `isError` must be absent or false.
    let is_error = call_resp
        .pointer("/result/isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(
        !is_error,
        "search with wired store must not be an error; got: {call_resp}"
    );

    // Extract the text content and parse as SearchData.
    let content = call_resp
        .pointer("/result/content")
        .and_then(|v| v.as_array())
        .expect("result.content must be an array");
    assert!(!content.is_empty(), "content must not be empty");

    let text = content[0]
        .get("text")
        .and_then(serde_json::Value::as_str)
        .expect("first content item must have text field");

    let data: SearchData = serde_json::from_str(text).expect("text must be valid SearchData JSON");

    assert!(
        data.hits.is_empty(),
        "empty store: no hits expected, got {:?}",
        data.hits
    );
    assert!(data.next_cursor.is_none());
    assert!(data.score_explain.is_none());
}

/// When no store is wired (using `CairnMcpHandler::new()`), the `search` tool
/// falls back to the stub and returns `isError = true`.
#[tokio::test]
async fn search_tool_without_store_returns_stub_error() {
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
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello","mode":"keyword","limit":10}}}"#,
    )
    .await;

    let call_resp = recv_frame(&mut client_reader).await;

    // Must be a result frame, not a JSON-RPC error.
    assert!(
        call_resp.get("result").is_some(),
        "search call must yield a result frame; got: {call_resp}"
    );

    // `isError` must be true (stub fallback).
    let is_error = call_resp
        .pointer("/result/isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(
        is_error,
        "search without store must return isError=true; got: {call_resp}"
    );
}

/// Calling `search` with an invalid mode returns `isError = true`.
#[tokio::test]
async fn search_tool_invalid_args_returns_error() {
    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        CairnMcpHandler::with_store(Arc::new(EmptyStore), CairnConfig::default())
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

    // Missing required `mode` field — serde_json deserialization will fail.
    send_frame(
        &mut client_write,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello"}}}"#,
    )
    .await;

    let call_resp = recv_frame(&mut client_reader).await;

    // Must be a result frame, not a JSON-RPC error.
    assert!(
        call_resp.get("result").is_some(),
        "bad-args search call must yield a result frame; got: {call_resp}"
    );

    // `isError` must be true.
    let is_error = call_resp
        .pointer("/result/isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(
        is_error,
        "search with invalid args must return isError=true; got: {call_resp}"
    );
}

/// `CairnMcpHandler::with_store` must be `Debug` and show `store_wired: true`.
#[test]
fn handler_with_store_debug_shows_wired() {
    let h = CairnMcpHandler::with_store(Arc::new(EmptyStore), CairnConfig::default());
    let dbg = format!("{h:?}");
    assert!(
        dbg.contains("store_wired: true"),
        "Debug must show store_wired: true; got: {dbg}"
    );
}

/// `CairnMcpHandler::new()` must be `Debug` and show `store_wired: false`.
#[test]
fn handler_new_debug_shows_not_wired() {
    let h = CairnMcpHandler::new();
    let dbg = format!("{h:?}");
    assert!(
        dbg.contains("store_wired: false"),
        "Debug must show store_wired: false; got: {dbg}"
    );
}
