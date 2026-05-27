//! MCP wire test for the wired `assemble_hot` tool.

#![allow(missing_docs)]

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::record::MemoryRecord;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::domain::{RecordId, Rfc3339Timestamp, ScopeTuple, TargetId};
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

fn scoped_playbook_record(
    id: &str,
    body: &str,
    updated_at: &str,
    skill_id: &str,
    lane: &str,
    requires: &[&str],
    provides: &[&str],
) -> MemoryRecord {
    let mut record = sample_record();
    record.id = RecordId::parse(id).expect("record id");
    record.target_id = TargetId::parse(id).expect("target id");
    record.kind = MemoryKind::Playbook;
    record.visibility = MemoryVisibility::Project;
    record.scope = ScopeTuple {
        tenant: Some("acme".to_owned()),
        ..ScopeTuple::default()
    };
    record.body = body.to_owned();
    record.updated_at = Rfc3339Timestamp::parse(updated_at).expect("updated_at");
    record
        .extra_frontmatter
        .insert("skill_id".to_owned(), serde_json::json!(skill_id));
    record
        .extra_frontmatter
        .insert("lane".to_owned(), serde_json::json!(lane));
    record
        .extra_frontmatter
        .insert("requires".to_owned(), serde_json::json!(requires));
    record
        .extra_frontmatter
        .insert("provides".to_owned(), serde_json::json!(provides));
    record
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

    let metrics =
        std::fs::read_to_string(vault.path().join(".cairn/metrics.jsonl")).expect("metrics file");
    assert!(
        metrics.lines().any(|line| {
            let event: serde_json::Value = serde_json::from_str(line).expect("metric JSON");
            event["event"] == "verb_invocation"
                && event["surface"] == "mcp"
                && event["verb"] == "assemble_hot"
                && event["status"] == "committed"
        }),
        "assemble_hot MCP call should emit a committed verb metric: {metrics}"
    );
    assert!(
        !metrics.contains("mcp purpose text"),
        "metric output must not include source body text: {metrics}"
    );
}

#[tokio::test]
async fn wired_assemble_hot_returns_active_playbook_prerequisites() {
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
    let prereq = scoped_playbook_record(
        "01HQZX9F5N0000000000000001",
        "mcp run-tests prerequisite playbook",
        "2026-04-22T14:03:00Z",
        "run-tests",
        "test.run",
        &[],
        &["cap.test"],
    );
    let active = scoped_playbook_record(
        "01HQZX9F5N0000000000000002",
        "mcp ship-pr active playbook",
        "2026-04-22T14:05:00Z",
        "ship-pr",
        "ship.pr",
        &["cap.test"],
        &["cap.ship"],
    );
    sqlite_store.upsert(&prereq).await.expect("upsert prereq");
    sqlite_store.upsert(&active).await.expect("upsert active");

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
            "arguments": { "budget": 4096 }
        }
    });
    send_frame(&mut client_write, &frame.to_string()).await;
    let resp = recv_frame(&mut client_reader).await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("text result");
    let envelope: serde_json::Value = serde_json::from_str(text).expect("envelope JSON");
    let prefix = envelope["data"]["prefix"].as_str().expect("prefix");

    let prereq_idx = prefix
        .find("mcp run-tests prerequisite playbook")
        .expect("prerequisite playbook in prefix");
    let active_idx = prefix
        .find("mcp ship-pr active playbook")
        .expect("active playbook in prefix");
    assert!(
        prereq_idx < active_idx,
        "prerequisite should precede active playbook: {prefix}"
    );
}
