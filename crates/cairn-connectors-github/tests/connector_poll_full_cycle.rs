//! Drives the full `GitHubConnector::poll` against wiremock, verifying:
//!   - All three resources are queried.
//!   - Events from all three carry the right `kind:*` labels.
//!   - The cursor encodes per-resource sub-cursors with `v=1`.

use std::sync::Arc;

use cairn_connectors_core::{Connector, ConnectorError, CredentialHandle, PollContext};
use cairn_connectors_github::GitHubConnector;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn full_poll_emits_events_from_all_resources() {
    let server = MockServer::start().await;

    let issues_body = include_str!("fixtures/issues_page_1.json");
    let prs_body = include_str!("fixtures/prs_page_1.json");
    let commits_body = include_str!("fixtures/commits_page_1.json");

    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_string(issues_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_string(prs_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_string(commits_body))
        .mount(&server)
        .await;

    let connector = GitHubConnector::with_base_url("o", "r", server.uri()).unwrap();

    let env = serde_json::json!({"kind": "pat", "token": "t"});
    let handle = Arc::new(CredentialHandle::from_bytes(env.to_string().into_bytes()));

    let outcome = connector
        .poll(&PollContext::new(
            handle.clone(),
            None,
            600,
            CancellationToken::new(),
        ))
        .await
        .expect("poll");

    // 2 issues + 1 PR + 2 commits.
    assert_eq!(outcome.events.len(), 5, "expected 5 events total");

    let kinds: std::collections::BTreeSet<String> = outcome
        .events
        .iter()
        .flat_map(|e| e.labels.iter().cloned())
        .collect();
    assert!(kinds.contains("kind:issue"), "missing kind:issue label");
    assert!(kinds.contains("kind:pr"), "missing kind:pr label");
    assert!(kinds.contains("kind:commit"), "missing kind:commit label");

    // Cursor is valid JSON with v=1.
    let next = outcome.next_cursor.expect("cursor must be present");
    let parsed: serde_json::Value = serde_json::from_str(&next).unwrap();
    assert_eq!(parsed["v"], 1, "cursor v field must be 1");
}

#[tokio::test]
async fn poll_returns_error_when_first_resource_429s() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "30"))
        .mount(&server)
        .await;

    let connector = GitHubConnector::with_base_url("o", "r", server.uri()).unwrap();

    let env = serde_json::json!({"kind": "pat", "token": "t"});
    let handle = Arc::new(CredentialHandle::from_bytes(env.to_string().into_bytes()));

    let err = connector
        .poll(&PollContext::new(
            handle,
            None,
            600,
            CancellationToken::new(),
        ))
        .await
        .expect_err("must error on 429");

    assert!(
        matches!(err, ConnectorError::RateLimited { .. }),
        "expected ConnectorError::RateLimited, got {err:?}"
    );
}
