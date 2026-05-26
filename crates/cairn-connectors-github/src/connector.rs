//! `GitHubConnector` — `Connector` + `ConnectorPlugin` impl that orchestrates
//! the three internal `GhResource` implementations.

use std::sync::Arc;

use async_trait::async_trait;
use cairn_connectors_core::{
    CONTRACT_VERSION, Connector, ConnectorCapabilities, ConnectorError, ConnectorEvent,
    ConnectorManifest, ConnectorPlugin, ContractVersion, CredentialHandle, Identity, PollContext,
    PollOutcome, VersionRange, WebhookContext, WebhookRequest,
};
use sha2::{Digest as _, Sha256};
use url::Url;

use crate::MANIFEST_TOML;
use crate::auth::GitHubAuth;
use crate::client::GhClient;
use crate::cursor::CursorState;
use crate::resources::{
    GhResource, Repo, commits::CommitsResource, issues::IssuesResource, prs::PrsResource,
};
use crate::webhook::dispatch;

/// Cached `GitHubAuth` with a fingerprint of the credential bytes that produced it.
struct CachedAuth {
    fingerprint: [u8; 32],
    auth: Arc<GitHubAuth>,
}

/// Public top-level connector. Created once per `(repo, credentials)` pair.
pub struct GitHubConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
    repo: Repo,
    base_url: Url,
    /// Cached `GitHubAuth` keyed by a fingerprint of the credential bytes.
    /// `None` until the first poll constructs it. On poll, if the fingerprint
    /// matches we reuse the cached auth (preserving App's installation-token
    /// cache across polls); otherwise we rebuild when credentials rotate.
    cached_auth: std::sync::Mutex<Option<CachedAuth>>,
}

impl GitHubConnector {
    /// Construct a new GitHub connector for `owner/name` against the default
    /// `https://api.github.com` base URL.
    pub fn new(owner: impl Into<String>, name: impl Into<String>) -> Result<Self, ConnectorError> {
        Self::with_base_url(owner, name, "https://api.github.com")
    }

    /// Construct with a caller-supplied base URL. Used by integration tests
    /// to redirect to `wiremock`.
    pub fn with_base_url(
        owner: impl Into<String>,
        name: impl Into<String>,
        base: impl AsRef<str>,
    ) -> Result<Self, ConnectorError> {
        let manifest = ConnectorManifest::parse_toml(MANIFEST_TOML)
            .map_err(|e| ConnectorError::fatal_msg(format!("github manifest: {e}")))?;
        let sensor = Identity::parse("snr:local:connector:github:v1")
            .map_err(|e| ConnectorError::fatal_msg(format!("github sensor identity: {e:?}")))?;
        let base_url = Url::parse(base.as_ref())
            .map_err(|e| ConnectorError::fatal_msg(format!("github base url: {e}")))?;
        Ok(Self {
            manifest,
            sensor,
            repo: Repo {
                owner: owner.into(),
                name: name.into(),
            },
            base_url,
            cached_auth: std::sync::Mutex::new(None),
        })
    }

    // `self` is intentionally kept for forward compatibility — resource sets may
    // become instance-configurable when per-repo feature flags land (Task 18+).
    #[allow(clippy::unused_self)]
    fn resources(&self) -> [&dyn GhResource; 3] {
        [&IssuesResource, &PrsResource, &CommitsResource]
    }

    /// Return a cached `Arc<GitHubAuth>`, rebuilding only when the credential
    /// bytes have changed (detected via SHA-256 fingerprint). This preserves
    /// the App variant's `ArcSwap<Option<InstallationToken>>` cache across
    /// polls, avoiding a fresh JWT + installation-token round-trip every tick.
    ///
    /// Uses `std::sync::Mutex` — the lock is held only during a non-async
    /// fingerprint comparison and optional `Arc` clone; we never await while
    /// holding it.
    fn resolve_auth(&self, handle: &CredentialHandle) -> Result<Arc<GitHubAuth>, ConnectorError> {
        let fp = fingerprint(handle);
        let mut guard = self
            .cached_auth
            .lock()
            .expect("cached_auth mutex: not poisoned");
        if let Some(c) = guard.as_ref()
            && c.fingerprint == fp
        {
            return Ok(c.auth.clone());
        }
        // Credentials rotated or first call — rebuild.
        let fresh = Arc::new(GitHubAuth::from_handle(handle)?);
        *guard = Some(CachedAuth {
            fingerprint: fp,
            auth: fresh.clone(),
        });
        Ok(fresh)
    }
}

/// SHA-256 fingerprint of the raw credential bytes. Used as a cache key so we
/// detect credential rotation without storing the plaintext bytes.
fn fingerprint(handle: &CredentialHandle) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(handle.bytes());
    let out = hasher.finalize();
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&out);
    fp
}

#[async_trait]
impl Connector for GitHubConnector {
    fn name(&self) -> &str {
        self.manifest.name()
    }

    fn manifest(&self) -> &ConnectorManifest {
        &self.manifest
    }

    fn capabilities(&self) -> &ConnectorCapabilities {
        static C: ConnectorCapabilities = ConnectorCapabilities {
            poll: true,
            webhook: true,
            backfill: true,
        };
        &C
    }

    fn sensor_identity(&self) -> &Identity {
        &self.sensor
    }

    fn supported_contract_versions(&self) -> VersionRange {
        <Self as ConnectorPlugin>::SUPPORTED_VERSIONS
    }

    async fn poll(&self, cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
        let auth = self.resolve_auth(&cx.credentials)?;
        let client = GhClient::new(auth, self.base_url.clone());

        let mut state = CursorState::decode(cx.last_cursor.as_deref())?;
        let resources = self.resources();
        let n_resources = u32::try_from(resources.len()).unwrap_or(3);
        let per_resource_budget = cx
            .budget_remaining_items
            .checked_div(n_resources)
            .unwrap_or(0)
            .max(1);

        let mut all_events: Vec<ConnectorEvent> = Vec::new();
        let mut max_hint: Option<std::time::Duration> = None;

        for (idx, r) in resources.iter().enumerate() {
            // Bail on cancel without losing what we already gathered.
            if cx.cancel.is_cancelled() {
                break;
            }
            let sub = match idx {
                0 => &state.issues,
                1 => &state.prs,
                _ => &state.commits,
            };
            match r.poll(&client, &self.repo, sub, per_resource_budget).await {
                Ok(outcome) => {
                    all_events.extend(outcome.events);
                    match idx {
                        0 => state.issues = outcome.next_cursor,
                        1 => state.prs = outcome.next_cursor,
                        _ => state.commits = outcome.next_cursor,
                    }
                    if let Some(h) = outcome.rate_limit_hint {
                        max_hint = Some(max_hint.map_or(h, |m| m.max(h)));
                    }
                }
                // Events from completed resources are intentionally discarded; the
                // substrate retries from the prior cursor on any Err return.
                Err(e) => return Err(e.into()),
            }
        }

        state.v = 1;
        let next_cursor = state
            .encode()
            .map_err(|e| ConnectorError::transient_msg(format!("cursor encode: {e}")))?;

        Ok(PollOutcome {
            events: all_events,
            next_cursor: Some(next_cursor),
            rate_limit_hint: max_hint,
        })
    }

    async fn ingest_webhook(
        &self,
        req: &WebhookRequest,
        _cx: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        // The substrate has already verified the HMAC-SHA256 signature and
        // stripped the "sha256=" prefix (declared in connector.toml
        // `"signature.prefix" = "sha256="`).  Use the raw header value as a
        // signature_id surrogate; the substrate dedups on the canonical sig_id
        // internally so no manual prefix stripping is needed here.
        let signature_id = req
            .header("X-Hub-Signature-256")
            .unwrap_or("unverified")
            .to_owned();
        Ok(dispatch(req, &signature_id, &self.repo)?)
    }
}

impl ConnectorPlugin for GitHubConnector {
    const NAME: &'static str = "github";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(CONTRACT_VERSION, ContractVersion::new(0, 2, 0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parses_and_name_matches() {
        let c = GitHubConnector::new("o", "r").expect("constructs");
        assert_eq!(c.name(), "github");
        assert_eq!(
            c.sensor_identity().as_str(),
            "snr:local:connector:github:v1"
        );
    }

    #[test]
    fn is_arc_dyn_connector() {
        let c: Arc<dyn Connector> = Arc::new(GitHubConnector::new("o", "r").expect("constructs"));
        assert_eq!(c.name(), "github");
    }

    #[test]
    fn capabilities_advertise_all_three() {
        let c = GitHubConnector::new("o", "r").unwrap();
        let caps = c.capabilities();
        assert!(caps.poll && caps.webhook && caps.backfill);
    }
}
