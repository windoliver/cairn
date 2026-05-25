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

use std::time::Duration;

use arc_swap::ArcSwap;
use cairn_connectors_core::CredentialHandle;
use chrono::{DateTime, Utc};
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
    // Body filled in Task 5/6; no `.await` yet — allow until then.
    #[allow(clippy::unused_async)]
    async fn app_installation_token(
        &self,
        _http: &reqwest::Client,
        _base_url: &url::Url,
    ) -> Result<String, GhError> {
        // Body filled in Task 5/6.
        Err(GhError::Transient(
            "app_installation_token not yet implemented".into(),
        ))
    }

    /// Pre-expiry refresh threshold: refresh if token expires in less than this.
    pub(crate) const REFRESH_LEAD: Duration = Duration::from_secs(90);
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
        assert!(matches!(GitHubAuth::from_handle(&handle), Err(GhError::Malformed(_))));
    }
}
