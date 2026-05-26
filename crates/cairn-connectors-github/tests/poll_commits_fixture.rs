//! Integration: `CommitsResource::poll` and `push` webhook against wiremock fixture data.

use cairn_connectors_core::CredentialHandle;
use cairn_connectors_github::testkit;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn commits_poll_emits_two_events() {
    let server = MockServer::start().await;
    let body = include_str!("fixtures/commits_page_1.json");

    Mock::given(method("GET"))
        .and(path("/repos/o/r/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let env = serde_json::json!({"kind": "pat", "token": "t"});
    let handle = CredentialHandle::from_bytes(env.to_string().into_bytes());

    let (events, cursor) = testkit::run_commits_poll(
        &handle,
        &Url::parse(&server.uri()).unwrap(),
        "o",
        "r",
        None,
        Some("main".into()),
        50,
    )
    .await
    .expect("poll succeeds");

    assert_eq!(events.len(), 2);
    assert_eq!(cursor.last_sha.as_deref(), Some("aaa111"));
}

#[tokio::test]
async fn commits_poll_stops_at_last_sha() {
    let server = MockServer::start().await;
    let body = include_str!("fixtures/commits_page_1.json");

    Mock::given(method("GET"))
        .and(path("/repos/o/r/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let env = serde_json::json!({"kind": "pat", "token": "t"});
    let handle = CredentialHandle::from_bytes(env.to_string().into_bytes());

    let (events, _cursor) = testkit::run_commits_poll(
        &handle,
        &Url::parse(&server.uri()).unwrap(),
        "o",
        "r",
        Some("aaa111".into()),
        Some("main".into()),
        50,
    )
    .await
    .expect("poll succeeds");

    assert!(
        events.is_empty(),
        "first commit equals last_sha so walk stops"
    );
}
