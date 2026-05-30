//! Integration: `PrsResource::poll` against wiremock fixture data.

use cairn_connectors_core::CredentialHandle;
use cairn_connectors_github::testkit;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn prs_poll_emits_one_event() {
    let server = MockServer::start().await;
    let body = include_str!("fixtures/prs_page_1.json");

    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let env = serde_json::json!({"kind": "pat", "token": "t"});
    let handle = CredentialHandle::from_bytes(env.to_string().into_bytes());

    let (events, _cursor) = testkit::run_prs_poll(
        &handle,
        &Url::parse(&server.uri()).unwrap(),
        "o",
        "r",
        None,
        50,
    )
    .await
    .expect("poll succeeds");

    assert_eq!(events.len(), 1);
    assert!(events[0].labels.contains("kind:pr"));
}
