//! Integration: `IssuesResource::poll` against wiremock fixture data.

use cairn_connectors_core::CredentialHandle;
use cairn_connectors_github::testkit;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn issues_poll_emits_two_events_and_advances_cursor() {
    let server = MockServer::start().await;
    let body = include_str!("fixtures/issues_page_1.json");

    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .and(query_param("state", "all"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let auth_env = serde_json::json!({"kind": "pat", "token": "t"});
    let handle = CredentialHandle::from_bytes(auth_env.to_string().into_bytes());

    let (events, next_cursor) = testkit::run_issues_poll(
        &handle,
        &Url::parse(&server.uri()).unwrap(),
        "o",
        "r",
        None,
        50,
    )
    .await
    .expect("poll succeeds");

    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.labels.contains("kind:issue")));
    assert!(next_cursor.since.is_some(), "cursor since advanced");
}
