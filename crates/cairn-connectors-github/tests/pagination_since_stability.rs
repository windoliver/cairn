//! Fix 2: verify that `since` is kept stable while paginating a full window.
//!
//! When the poll returns a full page it must NOT advance `since` — it must
//! only advance the page counter.  Only when the page is partial (window
//! exhausted) should `since` be updated.  Advancing `since` mid-window shifts
//! the result set and causes page N of the new-since window to omit items that
//! were on page N-1 of the original window.

use cairn_connectors_core::CredentialHandle;
use cairn_connectors_github::testkit::{self, ResourceCursor};
use chrono::DateTime;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build the auth handle used in all tests.
fn pat_handle() -> CredentialHandle {
    let env = serde_json::json!({"kind": "pat", "token": "t"});
    CredentialHandle::from_bytes(env.to_string().into_bytes())
}

/// Build a JSON array of `n` issue DTOs all with `updated_at = ts`.
fn issue_page(start_id: u64, n: usize, updated_at: &str) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| {
            let id = start_id + i as u64;
            serde_json::json!({
                "id": id,
                "number": id,
                "title": format!("issue {id}"),
                "body": null,
                "state": "open",
                "user": {"login": "alice"},
                "created_at": updated_at,
                "updated_at": updated_at,
                "html_url": format!("https://github.com/o/r/issues/{id}"),
                "labels": []
            })
        })
        .collect()
}

/// Build a JSON array of `n` PR DTOs all with `updated_at = ts`.
fn pr_page(start_id: u64, n: usize, updated_at: &str) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| {
            let id = start_id + i as u64;
            serde_json::json!({
                "id": id,
                "number": id,
                "title": format!("PR {id}"),
                "body": null,
                "state": "open",
                "user": {"login": "alice"},
                "created_at": updated_at,
                "updated_at": updated_at,
                "html_url": format!("https://github.com/o/r/pull/{id}"),
                "head": {"sha": "abc", "ref": "feat"},
                "base": {"sha": "def", "ref": "main"},
                "merged": false,
                "draft": false
            })
        })
        .collect()
}

/// Issues: when page 1 is full the cursor must keep `since` stable and advance
/// only `page`.  The core assertion is on the cursor returned by the Rust code,
/// not on the URL parameters sent to the server.
///
/// The wiremock mocks are keyed only on `page=1` / `page=2` (not on `since`)
/// because chrono's `to_rfc3339()` produces `+00:00` not `Z` and URL-encoding
/// differences would make exact-match assertions fragile.
#[tokio::test]
async fn issues_since_stable_across_full_pages() {
    let server = MockServer::start().await;

    let full_page = issue_page(1, 50, "2026-05-10T00:00:00Z");
    let partial_page = issue_page(51, 3, "2026-05-15T00:00:00Z");

    // Page 1: full (50 items).
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(full_page))
        .mount(&server)
        .await;

    // Page 2: partial (3 items < 50).
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(partial_page))
        .mount(&server)
        .await;

    let handle = pat_handle();
    let base = Url::parse(&server.uri()).unwrap();

    let since_ts = DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let initial_cursor = ResourceCursor {
        since: Some(since_ts),
        page: Some(1),
        ..Default::default()
    };

    // Poll page 1: full page → since stays, page advances to 2.
    let (events_p1, cursor_p1) =
        testkit::run_issues_poll_with_cursor(&handle, &base, "o", "r", initial_cursor.clone(), 50)
            .await
            .expect("page 1");
    assert_eq!(events_p1.len(), 50);
    assert_eq!(
        cursor_p1.page,
        Some(2),
        "full page: page counter must advance"
    );
    assert_eq!(
        cursor_p1.since, initial_cursor.since,
        "full page: since must NOT change while paginating"
    );

    // Poll page 2: partial page → since advances to max seen, page resets to 1.
    let (events_p2, cursor_p2) =
        testkit::run_issues_poll_with_cursor(&handle, &base, "o", "r", cursor_p1, 50)
            .await
            .expect("page 2");
    assert_eq!(events_p2.len(), 3);
    assert_eq!(
        cursor_p2.page,
        Some(1),
        "partial page: page must reset to 1"
    );
    assert!(
        cursor_p2.since.is_some(),
        "partial page: since must be set after window exhausted"
    );
    // since must have advanced past the original value (max of page-2 items).
    assert!(
        cursor_p2.since > initial_cursor.since,
        "partial page: since must advance beyond original value"
    );
}

/// PRs: same invariant — `since` must not shift while paginating a full window.
#[tokio::test]
async fn prs_since_stable_across_full_pages() {
    let server = MockServer::start().await;

    // PRs use client-side since filtering (no `since` query param), so we
    // only need to assert the `page` parameter is correct across calls.
    // The `since` stability is verified by checking `cursor_p1.since == initial_cursor.since`.
    let full_page = pr_page(1, 50, "2026-05-10T00:00:00Z");
    let partial_page = pr_page(51, 2, "2026-05-15T00:00:00Z");

    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(full_page))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(partial_page))
        .mount(&server)
        .await;

    let handle = pat_handle();
    let base = Url::parse(&server.uri()).unwrap();

    let since_ts = DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let initial_cursor = ResourceCursor {
        since: Some(since_ts),
        page: Some(1),
        ..Default::default()
    };

    // Poll page 1: full → since stable, page advances.
    let (events_p1, cursor_p1) =
        testkit::run_prs_poll_with_cursor(&handle, &base, "o", "r", initial_cursor.clone(), 50)
            .await
            .expect("page 1");
    // All 50 items have updated_at > since_ts so none are filtered out.
    assert_eq!(events_p1.len(), 50);
    assert_eq!(cursor_p1.page, Some(2), "full page: page must advance");
    assert_eq!(
        cursor_p1.since, initial_cursor.since,
        "full page: since must NOT change while paginating"
    );

    // Poll page 2: partial → since advances, page resets.
    let (events_p2, cursor_p2) =
        testkit::run_prs_poll_with_cursor(&handle, &base, "o", "r", cursor_p1, 50)
            .await
            .expect("page 2");
    // 2 items total, both after since_ts.
    assert_eq!(events_p2.len(), 2);
    assert_eq!(cursor_p2.page, Some(1), "partial page: page resets to 1");
    assert!(
        cursor_p2.since > initial_cursor.since,
        "partial page: since must advance"
    );
}
