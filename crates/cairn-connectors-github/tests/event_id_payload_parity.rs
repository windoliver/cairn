//! Verify cross-channel commit dedup: poll and push-webhook paths produce
//! byte-identical payloads (and thus the same `payload_hash`) for the same SHA,
//! so the substrate's event-id index deduplicates correctly without treating
//! the cross-channel observation as fatal reuse (Fix 1, round-8).

use std::sync::Arc;

use cairn_connectors_core::{Connector, CredentialHandle, WebhookRequest};
use cairn_connectors_github::GitHubConnector;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn pat_handle() -> CredentialHandle {
    let env = serde_json::json!({"kind": "pat", "token": "t"});
    CredentialHandle::from_bytes(env.to_string().into_bytes())
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn same_commit_via_poll_and_webhook_yields_identical_payload() {
    // --- Poll side ---

    let server = MockServer::start().await;

    // Stub the default-branch lookup.
    Mock::given(method("GET"))
        .and(path("/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "default_branch": "main"
        })))
        .mount(&server)
        .await;

    // Stub the commits list — returns the same SHA we'll deliver via webhook.
    let commits_body = serde_json::json!([{
        "sha": "abc123",
        "commit": {
            "author": {"name": "Carol", "email": "carol@example.com", "date": "2026-05-25T16:00:00Z"},
            "committer": {"name": "Carol", "email": "carol@example.com", "date": "2026-05-25T16:00:00Z"},
            "message": "feat: push event"
        },
        "author": {"login": "carol"},
        "html_url": "https://github.com/o/r/commit/abc123"
    }]);
    Mock::given(method("GET"))
        .and(path("/repos/o/r/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&commits_body))
        .mount(&server)
        .await;

    // Stub issues and PRs so the full-connector poll doesn't 404.
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let handle = Arc::new(pat_handle());

    let poll_connector =
        GitHubConnector::with_base_url("o", "r", server.uri()).expect("construct");
    let poll_outcome = poll_connector
        .poll(&cairn_connectors_core::PollContext::new(
            handle.clone(),
            None,
            300,
            tokio_util::sync::CancellationToken::new(),
        ))
        .await
        .expect("poll");

    let poll_commit = poll_outcome
        .events
        .iter()
        .find(|e| e.source_ref.kind == "commit")
        .expect("commit event must be emitted by poll");

    // --- Webhook side ---

    let webhook_body = serde_json::json!({
        "ref": "refs/heads/main",
        "before": "old",
        "after": "abc123",
        "repository": {"full_name": "o/r"},
        "commits": [{
            "id": "abc123",
            "message": "feat: push event",
            "author": {"name": "Carol", "email": "carol@example.com", "username": "carol"},
            "timestamp": "2026-05-25T16:00:00Z",
            "url": "https://github.com/o/r/commit/abc123"
        }]
    });

    let webhook_connector = GitHubConnector::new("o", "r").expect("construct");
    let webhook_events = webhook_connector
        .ingest_webhook(
            &WebhookRequest {
                connector: "github".into(),
                body: webhook_body.to_string().into_bytes(),
                headers: vec![
                    ("X-GitHub-Event".into(), "push".into()),
                    ("X-GitHub-Delivery".into(), "deliv-1".into()),
                    ("X-Hub-Signature-256".into(), "sha256=abc".into()),
                ],
            },
            &cairn_connectors_core::WebhookContext::new(handle.clone(), 10),
        )
        .await
        .expect("webhook ingest");

    let webhook_commit = webhook_events
        .iter()
        .find(|e| e.source_ref.kind == "commit")
        .expect("commit event must be emitted by webhook");

    // The point of this test: same SHA → same event_id AND same payload bytes.
    assert_eq!(
        poll_commit.event_id.as_str(),
        webhook_commit.event_id.as_str(),
        "event_id must match across poll and webhook for the same SHA (Fix 1, round-4)"
    );

    match (&poll_commit.payload, &webhook_commit.payload) {
        (
            cairn_connectors_core::ConnectorPayload::Json { body: p, .. },
            cairn_connectors_core::ConnectorPayload::Json { body: w, .. },
        ) => {
            assert_eq!(
                serde_json::to_string(p).unwrap(),
                serde_json::to_string(w).unwrap(),
                "payload JSON must be byte-identical for cross-channel dedup (Fix 1, round-8)"
            );
        }
        _ => panic!("expected JSON payloads on both sides"),
    }
}
