//! Test-only entrypoints that integration tests call directly.

use std::sync::Arc;

use cairn_connectors_core::{ConnectorEvent, CredentialHandle};
use url::Url;

use crate::auth::GitHubAuth;
use crate::client::GhClient;
use crate::cursor::ResourceCursor;
use crate::error::GhError;
use crate::resources::{GhResource, Repo, issues::IssuesResource};

/// Run `IssuesResource::poll` once and return the produced events + new cursor.
pub async fn run_issues_poll(
    handle: &CredentialHandle,
    base_url: &Url,
    owner: &str,
    name: &str,
    since: Option<chrono::DateTime<chrono::Utc>>,
    budget: u32,
) -> Result<(Vec<ConnectorEvent>, ResourceCursor), GhError> {
    let auth = Arc::new(GitHubAuth::from_handle(handle)?);
    let client = GhClient::new(auth, base_url.clone());
    let repo = Repo {
        owner: owner.into(),
        name: name.into(),
    };
    let sub = ResourceCursor {
        since,
        ..Default::default()
    };
    let outcome = IssuesResource.poll(&client, &repo, &sub, budget).await?;
    Ok((outcome.events, outcome.next_cursor))
}
