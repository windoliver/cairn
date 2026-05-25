//! `GhClient` — thin `reqwest` wrapper.
//!
//! All REST calls flow through `get_json`; rate-limit headers are captured
//! into `RateState` on every response (success or failure).

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use url::Url;

use crate::auth::{GitHubAuth, user_agent};
use crate::error::GhError;

/// Latest rate-limit signals captured from response headers.
#[derive(Debug, Clone, Default)]
pub(crate) struct RateState {
    pub remaining: Option<u32>,
    pub reset_at: Option<DateTime<Utc>>,
}

impl RateState {
    /// Hint duration, computed from `reset_at - now` if both fields are set
    /// and `remaining < threshold`.
    pub fn hint_if_low(&self, threshold: u32) -> Option<Duration> {
        let (remaining, reset_at) = (self.remaining?, self.reset_at?);
        if remaining >= threshold {
            return None;
        }
        let now = Utc::now();
        let diff = (reset_at - now).num_seconds();
        #[allow(clippy::cast_sign_loss)]
        Some(Duration::from_secs(diff.max(1) as u64))
    }
}

/// HTTP wrapper around `reqwest::Client` with adapter-side rate-state tracking.
pub(crate) struct GhClient {
    http: reqwest::Client,
    base_url: Url,
    auth: Arc<GitHubAuth>,
    rate_state: ArcSwap<RateState>,
}

impl GhClient {
    pub(crate) fn new(auth: Arc<GitHubAuth>, base_url: Url) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            auth,
            rate_state: ArcSwap::from_pointee(RateState::default()),
        }
    }

    pub(crate) fn rate_state(&self) -> Arc<RateState> {
        self.rate_state.load_full()
    }

    /// GET `path` with `query` and deserialize into `T`. Records rate-state
    /// from response headers; maps non-2xx into `GhError`.
    pub(crate) async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, GhError> {
        let bearer = self.auth.bearer(&self.http, &self.base_url).await?;
        let url = self
            .base_url
            .join(path)
            .map_err(|e| GhError::Malformed(format!("base url join: {e}")))?;

        let resp = self
            .http
            .get(url)
            .bearer_auth(&bearer)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", user_agent())
            .header("X-GitHub-Api-Version", "2022-11-28")
            .query(query)
            .send()
            .await?;

        self.record_rate(&resp);
        self.check_status(&resp)?;
        Ok(resp.json::<T>().await?)
    }

    fn record_rate(&self, resp: &reqwest::Response) {
        let remaining = resp
            .headers()
            .get("X-RateLimit-Remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u32>().ok());
        let reset_at = resp
            .headers()
            .get("X-RateLimit-Reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|t| DateTime::<Utc>::from_timestamp(t, 0));
        self.rate_state
            .store(Arc::new(RateState { remaining, reset_at }));
    }

    fn check_status(&self, resp: &reqwest::Response) -> Result<(), GhError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let code = status.as_u16();
        match code {
            401 | 403 => Err(GhError::Auth { status: code }),
            429 => {
                let retry_after = resp
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map_or_else(
                        || {
                            self.rate_state
                                .load()
                                .hint_if_low(u32::MAX)
                                .unwrap_or(Duration::from_mins(1))
                        },
                        Duration::from_secs,
                    );
                Err(GhError::RateLimited { retry_after })
            }
            s if s >= 500 => Err(GhError::Transient(format!("github 5xx: {s}"))),
            s => Err(GhError::BadRequest {
                status: s,
                message: String::new(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_connectors_core::CredentialHandle;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn pat_auth(token: &str) -> Arc<GitHubAuth> {
        let env = serde_json::json!({"kind": "pat", "token": token});
        let handle = CredentialHandle::from_bytes(env.to_string().into_bytes());
        Arc::new(GitHubAuth::from_handle(&handle).unwrap())
    }

    #[tokio::test]
    async fn get_json_carries_bearer_and_user_agent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues"))
            .and(header("Authorization", "Bearer test-pat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client = GhClient::new(
            pat_auth("test-pat"),
            Url::parse(&server.uri()).unwrap(),
        );
        let _: serde_json::Value = client.get_json("/repos/o/r/issues", &[]).await.unwrap();
    }

    #[tokio::test]
    async fn rate_state_captured_from_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("X-RateLimit-Remaining", "12")
                    .insert_header("X-RateLimit-Reset", "4102444800")
                    .set_body_json(serde_json::json!({})),
            )
            .mount(&server)
            .await;

        let client = GhClient::new(pat_auth("p"), Url::parse(&server.uri()).unwrap());
        let _: serde_json::Value = client.get_json("/x", &[]).await.unwrap();

        let state = client.rate_state();
        assert_eq!(state.remaining, Some(12));
        assert!(state.reset_at.is_some());
    }

    #[tokio::test]
    async fn status_429_maps_to_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "45")
                    .set_body_json(serde_json::json!({})),
            )
            .mount(&server)
            .await;

        let client = GhClient::new(pat_auth("p"), Url::parse(&server.uri()).unwrap());
        let err = client
            .get_json::<serde_json::Value>("/x", &[])
            .await
            .expect_err("429 must error");
        match err {
            GhError::RateLimited { retry_after } => {
                assert_eq!(retry_after, Duration::from_secs(45));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_401_maps_to_auth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let client = GhClient::new(pat_auth("p"), Url::parse(&server.uri()).unwrap());
        let err = client
            .get_json::<serde_json::Value>("/x", &[])
            .await
            .expect_err("401 must error");
        assert!(matches!(err, GhError::Auth { status: 401 }));
    }
}
