//! Internal `GhResource` trait + `Repo` + `ResourcePoll`.
//!
//! Per-resource adapters (issues, prs, commits) implement [`GhResource`]; the
//! top-level [`crate::GitHubConnector`] walks them in `Connector::poll` and
//! dispatches `X-GitHub-Event` to them in `ingest_webhook`.

use std::time::Duration;

use async_trait::async_trait;
use cairn_connectors_core::ConnectorEvent;
use serde::{Deserialize, Serialize};

use crate::client::GhClient;
use crate::cursor::ResourceCursor;
use crate::error::GhError;

pub(crate) mod commits;
pub(crate) mod issues;
pub(crate) mod prs;

/// Repository the connector is configured against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    /// Repository owner (user or organisation).
    pub owner: String,
    /// Repository name.
    pub name: String,
}

impl Repo {
    /// Returns `"owner/name"` — used as the `scope` value in [`ConnectorEvent`].
    pub fn scope_value(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Outcome of one `GhResource::poll` call.
#[derive(Debug, Default)]
pub(crate) struct ResourcePoll {
    /// Events produced by this poll.
    pub events: Vec<ConnectorEvent>,
    /// Cursor to pass on the next poll call.
    pub next_cursor: ResourceCursor,
    /// Optional back-off hint derived from rate-limit headers.
    pub rate_limit_hint: Option<Duration>,
}

/// Internal adapter trait — one impl per GitHub resource.
#[async_trait]
pub(crate) trait GhResource: Send + Sync {
    /// Short identifier for this resource (e.g. `"issues"`, `"prs"`, `"commits"`).
    // Called by the orchestrator for tracing/debugging; not yet wired to a
    // callsite in P0, so suppress the false-positive dead-code lint here.
    #[allow(dead_code)]
    fn kind(&self) -> &'static str;

    /// Poll for new events since the cursor. `budget` is the maximum number of
    /// API pages to consume in a single call.
    async fn poll(
        &self,
        client: &GhClient,
        repo: &Repo,
        sub_cursor: &ResourceCursor,
        budget: u32,
    ) -> Result<ResourcePoll, GhError>;

    /// Parse a webhook delivery into zero or more [`ConnectorEvent`]s.
    fn parse_webhook(
        &self,
        event_type: &str,
        delivery_id: &str,
        signature_id: &str,
        body: &[u8],
        repo: &Repo,
    ) -> Result<Vec<ConnectorEvent>, GhError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_scope_value() {
        let r = Repo {
            owner: "windoliver".into(),
            name: "cairn".into(),
        };
        assert_eq!(r.scope_value(), "windoliver/cairn");
    }
}
