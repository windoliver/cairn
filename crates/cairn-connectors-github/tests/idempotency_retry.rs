//! Idempotency test: the same upstream object must yield the same `event_id`
//! across multiple polls, so that the substrate's event-id dedup gate can
//! suppress duplicates when a poll is retried after a partial success.

use cairn_connectors_core::CredentialHandle;
use cairn_connectors_github::testkit;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn same_upstream_object_yields_stable_event_id_across_polls() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(include_str!("fixtures/issues_page_1.json")),
        )
        .mount(&server)
        .await;

    let env = serde_json::json!({"kind": "pat", "token": "t"});
    let handle = CredentialHandle::from_bytes(env.to_string().into_bytes());
    let base = Url::parse(&server.uri()).unwrap();

    let (events1, _) = testkit::run_issues_poll(&handle, &base, "o", "r", None, 50)
        .await
        .unwrap();
    let (events2, _) = testkit::run_issues_poll(&handle, &base, "o", "r", None, 50)
        .await
        .unwrap();

    assert_eq!(events1.len(), events2.len());
    for (a, b) in events1.iter().zip(events2.iter()) {
        assert_eq!(
            a.event_id.as_str(),
            b.event_id.as_str(),
            "same upstream object must produce the same event_id across polls"
        );
    }
}
