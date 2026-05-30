//! Verify the GitHub App installation token cache survives across `poll` calls.
//!
//! The `GitHubConnector` caches `Arc<GitHubAuth>` keyed by a SHA-256 fingerprint
//! of the credential bytes.  Reusing the same `Arc` preserves the App variant's
//! `ArcSwap<Option<InstallationToken>>`, so the installation token is minted only
//! once across two polls with identical credentials.

use std::sync::Arc;

use cairn_connectors_core::{Connector, CredentialHandle, PollContext};
use cairn_connectors_github::GitHubConnector;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_PEM: &str = include_str!("fixtures/test_rsa_2048.pem");

/// Strip the leading comment lines that our fixture prepends so that
/// `jsonwebtoken` can find the `-----BEGIN RSA PRIVATE KEY-----` marker.
fn pem() -> &'static str {
    TEST_PEM
        .find("-----BEGIN")
        .map_or(TEST_PEM, |i| &TEST_PEM[i..])
}

#[tokio::test]
async fn app_installation_token_minted_once_across_two_polls() {
    let server = MockServer::start().await;

    // Stub the installation-token endpoint.  `.expect(1)` asserts at drop time
    // that it was called exactly once — proving the cache survived poll #2.
    Mock::given(method("POST"))
        .and(path("/app/installations/67890/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "token": "ghs_installtoken",
            "expires_at": "2099-01-01T00:00:00Z",
        })))
        .expect(1)
        .mount(&server)
        .await;

    // Stub the default-branch lookup (called once per poll for commits resource).
    Mock::given(method("GET"))
        .and(path("/repos/o/r"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"default_branch": "main"})),
        )
        .mount(&server)
        .await;

    // Stub the three resource endpoints with empty results.
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
    Mock::given(method("GET"))
        .and(path("/repos/o/r/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let env = serde_json::json!({
        "kind": "app",
        "app_id": 42_u64,
        "installation_id": 67890_u64,
        "private_key_pem": pem(),
    });
    let handle = Arc::new(CredentialHandle::from_bytes(env.to_string().into_bytes()));

    let connector = GitHubConnector::with_base_url("o", "r", server.uri()).unwrap();

    // Poll twice with the same credentials.
    for _ in 0..2 {
        connector
            .poll(&PollContext::new(
                handle.clone(),
                None,
                10,
                CancellationToken::new(),
            ))
            .await
            .expect("poll should succeed");
    }

    // Mock's `.expect(1)` on drop asserts the installation-token endpoint was
    // hit exactly once — proving the Arc<GitHubAuth> (and its token cache) was
    // reused across both polls.
}
