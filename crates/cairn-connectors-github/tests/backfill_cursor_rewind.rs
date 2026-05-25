//! Verifies `last_cursor = None` triggers a full backfill traversal across pages.

use cairn_connectors_core::CredentialHandle;
use cairn_connectors_github::testkit;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn backfill_walks_two_pages_then_terminates() {
    let server = MockServer::start().await;

    // Page 1 — full page of 50 items (synthetic). per_page=50, so a full page
    // forces a second poll.
    let page1: Vec<serde_json::Value> = (0..50)
        .map(|n| {
            serde_json::json!({
                "id": n,
                "number": n,
                "title": format!("issue {n}"),
                "body": null,
                "state": "open",
                "user": {"login": "alice"},
                "created_at": "2026-05-01T00:00:00Z",
                "updated_at": "2026-05-01T00:00:00Z",
                "html_url": format!("https://github.com/o/r/issues/{n}"),
                "labels": []
            })
        })
        .collect();
    // Page 2 — partial page (3 items) signals end-of-stream.
    let page2: Vec<serde_json::Value> = (50..53)
        .map(|n| {
            serde_json::json!({
                "id": n,
                "number": n,
                "title": format!("issue {n}"),
                "body": null,
                "state": "open",
                "user": {"login": "alice"},
                "created_at": "2026-05-02T00:00:00Z",
                "updated_at": "2026-05-02T00:00:00Z",
                "html_url": format!("https://github.com/o/r/issues/{n}"),
                "labels": []
            })
        })
        .collect();

    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page1))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page2))
        .mount(&server)
        .await;

    let env = serde_json::json!({"kind": "pat", "token": "t"});
    let handle = CredentialHandle::from_bytes(env.to_string().into_bytes());
    let base = Url::parse(&server.uri()).unwrap();

    // First poll: cursor=default → fetches page 1 (50 items), advances page to 2.
    let (events_p1, cursor_after_p1) = testkit::run_issues_poll_with_cursor(
        &handle,
        &base,
        "o",
        "r",
        testkit::ResourceCursor::default(),
        50,
    )
    .await
    .expect("page 1 poll");
    assert_eq!(events_p1.len(), 50);
    assert_eq!(cursor_after_p1.page, Some(2));

    // Second poll: thread `cursor_after_p1` (with page=2) → fetches page 2
    // (3 items < per_page=50 means end-of-stream; page resets to 1).
    let (events_p2, cursor_after_p2) = testkit::run_issues_poll_with_cursor(
        &handle,
        &base,
        "o",
        "r",
        cursor_after_p1,
        50,
    )
    .await
    .expect("page 2 poll");
    assert_eq!(events_p2.len(), 3);
    assert_eq!(
        cursor_after_p2.page,
        Some(1),
        "partial page resets cursor.page to 1 (next cycle starts a fresh window)"
    );
}
