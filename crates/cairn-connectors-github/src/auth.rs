//! GitHub auth (PAT + GitHub App).
//!
//! Credential resolution from `CredentialHandle`: the handle bytes carry a
//! JSON envelope describing the auth shape:
//!
//! ```json
//! { "kind": "pat", "token": "ghp_..." }
//! { "kind": "app",
//!   "app_id": 12345,
//!   "installation_id": 67890,
//!   "private_key_pem": "-----BEGIN RSA PRIVATE KEY-----\n..." }
//! ```

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use cairn_connectors_core::CredentialHandle;
use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};

use crate::error::GhError;

/// Cached installation token returned by the App auth path.
#[derive(Clone)]
pub(crate) struct InstallationToken {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// Adapter-internal auth surface. Constructed from a `CredentialHandle`.
pub(crate) enum GitHubAuth {
    Pat {
        token: String,
    },
    App {
        app_id: u64,
        installation_id: u64,
        private_key_pem: String,
        cached: ArcSwap<Option<InstallationToken>>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CredentialEnvelope {
    Pat {
        token: String,
    },
    App {
        app_id: u64,
        installation_id: u64,
        private_key_pem: String,
    },
}

#[derive(Serialize)]
struct AppJwtClaims {
    iss: String,
    iat: i64,
    exp: i64,
}

#[derive(Deserialize)]
struct InstallationTokenResp {
    token: String,
    expires_at: DateTime<Utc>,
}

impl GitHubAuth {
    /// Build a `GitHubAuth` from a substrate-provided credential handle.
    pub(crate) fn from_handle(handle: &CredentialHandle) -> Result<Self, GhError> {
        let env: CredentialEnvelope = serde_json::from_slice(handle.bytes())
            .map_err(|e| GhError::Malformed(format!("credential envelope: {e}")))?;
        Ok(match env {
            CredentialEnvelope::Pat { token } => Self::Pat { token },
            CredentialEnvelope::App {
                app_id,
                installation_id,
                private_key_pem,
            } => Self::App {
                app_id,
                installation_id,
                private_key_pem,
                cached: ArcSwap::from_pointee(None),
            },
        })
    }

    /// Returns a token usable directly in `Authorization: Bearer <token>`.
    pub(crate) async fn bearer(
        &self,
        http: &reqwest::Client,
        base_url: &url::Url,
    ) -> Result<String, GhError> {
        match self {
            Self::Pat { token } => Ok(token.clone()),
            Self::App { .. } => self.app_installation_token(http, base_url).await,
        }
    }

    /// App-only: produce a current installation token, minting or refreshing as needed.
    async fn app_installation_token(
        &self,
        http: &reqwest::Client,
        base_url: &url::Url,
    ) -> Result<String, GhError> {
        let (app_id, installation_id, private_key_pem, cached) = match self {
            Self::App {
                app_id,
                installation_id,
                private_key_pem,
                cached,
            } => (*app_id, *installation_id, private_key_pem, cached),
            Self::Pat { .. } => unreachable!("only called from App branch"),
        };

        if let Some(tok) = cached.load().as_ref().as_ref() {
            let now = Utc::now();
            if tok.expires_at - now
                > chrono::Duration::from_std(Self::REFRESH_LEAD)
                    .expect("REFRESH_LEAD fits in chrono::Duration")
            {
                return Ok(tok.token.clone());
            }
        }

        let jwt = mint_jwt(app_id, private_key_pem)?;
        let path = format!("/app/installations/{installation_id}/access_tokens");
        let url = base_url
            .join(&path)
            .map_err(|e| GhError::Malformed(format!("base url: {e}")))?;

        let resp = http
            .post(url)
            .bearer_auth(jwt)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", user_agent())
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(GhError::Auth {
                    status: status.as_u16(),
                });
            }
            return Err(GhError::Transient(format!(
                "installation token fetch returned {status}"
            )));
        }

        let body: InstallationTokenResp = resp.json().await?;
        let fresh = InstallationToken {
            token: body.token.clone(),
            expires_at: body.expires_at,
        };
        cached.store(Arc::new(Some(fresh)));
        Ok(body.token)
    }

    /// Pre-expiry refresh threshold: refresh if token expires in less than this.
    pub(crate) const REFRESH_LEAD: Duration = Duration::from_secs(90);
}

fn mint_jwt(app_id: u64, private_key_pem: &str) -> Result<String, GhError> {
    let now = Utc::now().timestamp();
    let claims = AppJwtClaims {
        iss: app_id.to_string(),
        iat: now - 60,
        exp: now + 540,
    };
    let header = Header::new(Algorithm::RS256);
    let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())?;
    Ok(encode(&header, &claims, &key)?)
}

pub(crate) fn user_agent() -> String {
    format!("cairn-connectors-github/{}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pat_envelope_round_trips() {
        let json = serde_json::json!({"kind": "pat", "token": "ghp_test"});
        let handle = CredentialHandle::from_bytes(json.to_string().into_bytes());
        let auth = GitHubAuth::from_handle(&handle).expect("envelope parses");
        let client = reqwest::Client::new();
        let base = url::Url::parse("https://api.github.com").unwrap();
        let bearer = auth.bearer(&client, &base).await.expect("pat bearer");
        assert_eq!(bearer, "ghp_test");
    }

    #[test]
    fn app_envelope_round_trips() {
        let json = serde_json::json!({
            "kind": "app",
            "app_id": 12345_u64,
            "installation_id": 67890_u64,
            "private_key_pem": "-----BEGIN RSA PRIVATE KEY-----\nABC\n-----END RSA PRIVATE KEY-----",
        });
        let handle = CredentialHandle::from_bytes(json.to_string().into_bytes());
        let auth = GitHubAuth::from_handle(&handle).expect("envelope parses");
        match auth {
            GitHubAuth::App {
                app_id,
                installation_id,
                ..
            } => {
                assert_eq!(app_id, 12345);
                assert_eq!(installation_id, 67890);
            }
            GitHubAuth::Pat { .. } => panic!("expected App variant"),
        }
    }

    #[test]
    fn malformed_envelope_rejected() {
        let handle = CredentialHandle::from_bytes(b"{\"kind\":\"oops\"}".to_vec());
        assert!(matches!(
            GitHubAuth::from_handle(&handle),
            Err(GhError::Malformed(_))
        ));
    }
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use base64::engine::Engine as _;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TEST_PEM_RAW: &str = include_str!("../tests/fixtures/test_rsa_2048.pem");

    fn test_pem() -> &'static str {
        // Strip leading comment lines so `from_rsa_pem` finds the BEGIN marker.
        TEST_PEM_RAW
            .find("-----BEGIN")
            .map_or(TEST_PEM_RAW, |i| &TEST_PEM_RAW[i..])
    }

    #[test]
    fn jwt_claims_have_iss_and_exp_window() {
        let jwt = mint_jwt(42, test_pem()).expect("mint succeeds");
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT has three segments");

        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .expect("base64 decode");
        let json: serde_json::Value = serde_json::from_slice(&payload).expect("json");
        assert_eq!(json["iss"], "42");
        let iat = json["iat"].as_i64().unwrap();
        let exp = json["exp"].as_i64().unwrap();
        assert_eq!(
            exp - iat,
            600,
            "iat..exp window is 600 seconds (9 + 1 min slack)"
        );
    }

    #[tokio::test]
    async fn app_fetches_installation_token_then_caches() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/app/installations/67890/access_tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "token": "ghs_installtoken",
                "expires_at": "2099-01-01T00:00:00Z",
            })))
            .expect(1) // must be called exactly once across both bearer() calls
            .mount(&server)
            .await;

        let envelope = serde_json::json!({
            "kind": "app",
            "app_id": 42_u64,
            "installation_id": 67890_u64,
            "private_key_pem": test_pem(),
        });
        let handle = CredentialHandle::from_bytes(envelope.to_string().into_bytes());
        let auth = GitHubAuth::from_handle(&handle).unwrap();

        let http = reqwest::Client::new();
        let base = url::Url::parse(&server.uri()).unwrap();

        let tok1 = auth.bearer(&http, &base).await.expect("first bearer");
        let tok2 = auth
            .bearer(&http, &base)
            .await
            .expect("second bearer cached");
        assert_eq!(tok1, "ghs_installtoken");
        assert_eq!(tok2, "ghs_installtoken");
        // Mock's .expect(1) verifies caching at drop time.
    }
}
