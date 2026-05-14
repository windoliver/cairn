//! MCP wire test for the wired `assemble_hot` tool.

#![allow(missing_docs)]

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::domain::ScopeTuple;
use cairn_core::mcp_auth::{McpAuthContext, McpSessionScope, ScopeResolutionError};
use cairn_mcp::CairnMcpHandler;
use rmcp::ServiceExt as _;
use tokio::io::BufReader;

use common::{do_initialize, recv_frame, send_frame};

struct StaticScope(Vec<ScopeTuple>);

impl McpSessionScope for StaticScope {
    fn allowed_scopes(
        &self,
        _ctx: &McpAuthContext<'_>,
    ) -> Result<Vec<ScopeTuple>, ScopeResolutionError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn wired_assemble_hot_returns_committed_prefix() {
    let vault = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(vault.path().join(".cairn")).expect("create .cairn");
    std::fs::write(vault.path().join(".cairn/vault.id"), "vault-test\n").expect("vault id");
    std::fs::write(vault.path().join("purpose.md"), "mcp purpose text").expect("purpose");
    std::fs::write(vault.path().join("index.md"), "mcp index text").expect("index");

    let sqlite_store = Arc::new(
        cairn_store_sqlite::open(&vault.path().join(".cairn/cairn.db"))
            .await
            .expect("open sqlite store"),
    );
    let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = sqlite_store.clone();
    let principal = ScopeTuple {
        tenant: Some("acme".to_owned()),
        ..ScopeTuple::default()
    };
    let scope: Arc<dyn McpSessionScope> = Arc::new(StaticScope(vec![principal.clone()]));
    let handler = CairnMcpHandler::with_store_scope_sqlite_and_vault(
        store,
        sqlite_store,
        scope,
        CairnConfig::default(),
        principal,
        vault.path().to_path_buf(),
    );

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
    let _init = do_initialize(&mut client_write, &mut client_reader).await;

    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "assemble_hot",
            "arguments": { "budget": 64 }
        }
    });
    send_frame(&mut client_write, &frame.to_string()).await;
    let resp = recv_frame(&mut client_reader).await;

    assert_ne!(
        resp.pointer("/result/isError"),
        Some(&serde_json::Value::Bool(true)),
        "assemble_hot should not return an MCP error result: {resp}"
    );
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("text result");
    let envelope: serde_json::Value = serde_json::from_str(text).expect("envelope JSON");

    assert_eq!(envelope["verb"], "assemble_hot");
    assert_eq!(envelope["status"], "committed");
    assert!(
        envelope["data"]["prefix"]
            .as_str()
            .expect("prefix")
            .contains("mcp purpose text"),
        "envelope={envelope}"
    );
    assert!(envelope["data"]["bytes"].as_u64().expect("bytes") <= 64);
    assert!(envelope["data"]["segments"].is_array());
}
