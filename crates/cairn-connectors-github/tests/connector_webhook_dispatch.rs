//! Drives `GitHubConnector::ingest_webhook` end-to-end for issues / PR / push.

use std::sync::Arc;

use cairn_connectors_core::{Connector, CredentialHandle, WebhookContext, WebhookRequest};
use cairn_connectors_github::GitHubConnector;

fn pat_handle() -> Arc<CredentialHandle> {
    let env = serde_json::json!({"kind": "pat", "token": "t"});
    Arc::new(CredentialHandle::from_bytes(env.to_string().into_bytes()))
}

fn req(event: &str, delivery: &str, body: &[u8]) -> WebhookRequest {
    WebhookRequest {
        connector: "github".into(),
        body: body.to_vec(),
        headers: vec![
            ("X-GitHub-Event".into(), event.into()),
            ("X-GitHub-Delivery".into(), delivery.into()),
            (
                "X-Hub-Signature-256".into(),
                "sha256=abc123deadbeef".into(),
            ),
        ],
    }
}

#[tokio::test]
async fn webhook_issues_opened_dispatches_to_issues_resource() {
    let connector = GitHubConnector::new("o", "r").unwrap();
    let body = include_bytes!("fixtures/webhook_issues_opened.json");
    let events = connector
        .ingest_webhook(
            &req("issues", "deliver-1", body),
            &WebhookContext::new(pat_handle(), 1000),
        )
        .await
        .expect("issues dispatch");
    assert_eq!(events.len(), 1);
    assert!(events[0].labels.contains("kind:issue"));
}

#[tokio::test]
async fn webhook_pull_request_dispatches_to_prs_resource() {
    let connector = GitHubConnector::new("o", "r").unwrap();
    let body = include_bytes!("fixtures/webhook_pull_request_opened.json");
    let events = connector
        .ingest_webhook(
            &req("pull_request", "deliver-2", body),
            &WebhookContext::new(pat_handle(), 1000),
        )
        .await
        .expect("pr dispatch");
    assert_eq!(events.len(), 1);
    assert!(events[0].labels.contains("kind:pr"));
}

#[tokio::test]
async fn webhook_push_dispatches_to_commits_resource() {
    let connector = GitHubConnector::new("o", "r").unwrap();
    let body = include_bytes!("fixtures/webhook_push.json");
    let events = connector
        .ingest_webhook(
            &req("push", "deliver-3", body),
            &WebhookContext::new(pat_handle(), 1000),
        )
        .await
        .expect("push dispatch");
    assert_eq!(events.len(), 1);
    assert!(events[0].labels.contains("kind:commit"));
}

#[tokio::test]
async fn webhook_ping_returns_empty_no_error() {
    let connector = GitHubConnector::new("o", "r").unwrap();
    let events = connector
        .ingest_webhook(
            &req("ping", "deliver-4", b"{\"zen\":\"keep it simple\"}"),
            &WebhookContext::new(pat_handle(), 1000),
        )
        .await
        .expect("ping must not error");
    assert!(events.is_empty());
}
