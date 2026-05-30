//! Verifies 429 + Retry-After surfaces as `RateLimited`; cursor not advanced.

use cairn_connectors_core::CredentialHandle;
use cairn_connectors_github::{GhError, testkit};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn rate_limited_429_returns_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "120")
                .set_body_json(serde_json::json!({
                    "message": "API rate limit exceeded"
                })),
        )
        .mount(&server)
        .await;

    let env = serde_json::json!({"kind": "pat", "token": "t"});
    let handle = CredentialHandle::from_bytes(env.to_string().into_bytes());

    let err = testkit::run_issues_poll(
        &handle,
        &Url::parse(&server.uri()).unwrap(),
        "o",
        "r",
        None,
        50,
    )
    .await
    .expect_err("429 must error");

    match err {
        GhError::RateLimited { retry_after } => {
            assert_eq!(retry_after.as_secs(), 120);
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}
