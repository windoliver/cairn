//! Test-only entrypoints that integration tests call directly.

use std::sync::Arc;

use cairn_connectors_core::{ConnectorEvent, CredentialHandle};
use url::Url;

use crate::auth::GitHubAuth;
use crate::client::GhClient;
use crate::error::GhError;
use crate::resources::{
    GhResource, Repo, commits::CommitsResource, issues::IssuesResource, prs::PrsResource,
};

// Re-export so integration tests can name the type without reaching into
// the private `cursor` module. The `pub use` also brings it into scope
// for use within this module.
pub use crate::cursor::ResourceCursor;

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

/// Run `PrsResource::poll` once and return the produced events + new cursor.
pub async fn run_prs_poll(
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
    let outcome = PrsResource.poll(&client, &repo, &sub, budget).await?;
    Ok((outcome.events, outcome.next_cursor))
}

/// Run `CommitsResource::poll` with a fully-specified `ResourceCursor`.
/// Used by tests that need explicit control over branch / since / `last_sha`.
pub async fn run_commits_poll_with_cursor(
    handle: &CredentialHandle,
    base_url: &Url,
    owner: &str,
    name: &str,
    cursor: ResourceCursor,
    budget: u32,
) -> Result<(Vec<ConnectorEvent>, ResourceCursor), GhError> {
    let auth = Arc::new(GitHubAuth::from_handle(handle)?);
    let client = GhClient::new(auth, base_url.clone());
    let repo = Repo {
        owner: owner.into(),
        name: name.into(),
    };
    let outcome = CommitsResource
        .poll(&client, &repo, &cursor, budget)
        .await?;
    Ok((outcome.events, outcome.next_cursor))
}

/// Run `CommitsResource::poll` once and return the produced events + new cursor.
pub async fn run_commits_poll(
    handle: &CredentialHandle,
    base_url: &Url,
    owner: &str,
    name: &str,
    last_sha: Option<String>,
    branch: Option<String>,
    budget: u32,
) -> Result<(Vec<ConnectorEvent>, ResourceCursor), GhError> {
    let auth = Arc::new(GitHubAuth::from_handle(handle)?);
    let client = GhClient::new(auth, base_url.clone());
    let repo = Repo {
        owner: owner.into(),
        name: name.into(),
    };
    let sub = ResourceCursor {
        last_sha,
        branch,
        ..Default::default()
    };
    let outcome = CommitsResource.poll(&client, &repo, &sub, budget).await?;
    Ok((outcome.events, outcome.next_cursor))
}

/// Run `PrsResource::poll` with a fully-specified `ResourceCursor`.
/// Used by pagination tests that need to thread `page` and `since` across calls.
pub async fn run_prs_poll_with_cursor(
    handle: &CredentialHandle,
    base_url: &Url,
    owner: &str,
    name: &str,
    cursor: ResourceCursor,
    budget: u32,
) -> Result<(Vec<ConnectorEvent>, ResourceCursor), GhError> {
    let auth = Arc::new(GitHubAuth::from_handle(handle)?);
    let client = GhClient::new(auth, base_url.clone());
    let repo = Repo {
        owner: owner.into(),
        name: name.into(),
    };
    let outcome = PrsResource.poll(&client, &repo, &cursor, budget).await?;
    Ok((outcome.events, outcome.next_cursor))
}

/// Run `IssuesResource::poll` with a fully-specified `ResourceCursor`.
/// Used by backfill tests that need to thread `page` across calls.
pub async fn run_issues_poll_with_cursor(
    handle: &CredentialHandle,
    base_url: &Url,
    owner: &str,
    name: &str,
    cursor: ResourceCursor,
    budget: u32,
) -> Result<(Vec<ConnectorEvent>, ResourceCursor), GhError> {
    let auth = Arc::new(GitHubAuth::from_handle(handle)?);
    let client = GhClient::new(auth, base_url.clone());
    let repo = Repo {
        owner: owner.into(),
        name: name.into(),
    };
    let outcome = IssuesResource.poll(&client, &repo, &cursor, budget).await?;
    Ok((outcome.events, outcome.next_cursor))
}
