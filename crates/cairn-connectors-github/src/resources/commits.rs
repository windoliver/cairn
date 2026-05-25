//! Commits / push resource.

use std::collections::BTreeSet;

use async_trait::async_trait;
use cairn_connectors_core::{
    ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use ulid::Ulid;

use crate::client::GhClient;
use crate::cursor::ResourceCursor;
use crate::error::GhError;
use crate::resources::{GhResource, Repo, ResourcePoll};

pub(crate) struct CommitsResource;

/// Minimal response shape for `GET /repos/{owner}/{name}`.
///
/// Only `default_branch` is needed; other fields are silently ignored by serde
/// because this struct does not use `#[serde(deny_unknown_fields)]`.
#[derive(Debug, Deserialize)]
struct RepoMetaDto {
    default_branch: String,
}

/// Fetch the default branch name from the GitHub repository metadata.
///
/// Called once per poll cycle when `sub_cursor.branch` is `None` (i.e. on
/// the very first poll for a repository, or when the cursor has been reset).
/// The result is stored in the returned cursor so subsequent polls re-use it
/// without making an extra API call.
async fn fetch_default_branch(client: &GhClient, repo: &Repo) -> Result<String, GhError> {
    let path = format!("/repos/{}/{}", repo.owner, repo.name);
    let meta: RepoMetaDto = client.get_json(&path, &[]).await?;
    Ok(meta.default_branch)
}

#[derive(Debug, Deserialize)]
struct CommitDto {
    sha: String,
    commit: CommitInnerDto,
    #[serde(default)]
    author: Option<ActorDto>,
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct CommitInnerDto {
    author: GitActorDto,
    message: String,
}

#[derive(Debug, Deserialize)]
struct ActorDto {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GitActorDto {
    name: String,
    email: String,
    date: DateTime<Utc>,
}

/// Envelope for `push` webhook events.
///
/// The on-wire payload also contains `before`, `after`, and `pusher` fields
/// which are not needed by this adapter — serde silently ignores them because
/// this struct does not use `#[serde(deny_unknown_fields)]`.
#[derive(Debug, Deserialize)]
struct WebhookPushEnvelope {
    #[serde(rename = "ref")]
    ref_name: String,
    repository: RepositoryDto,
    commits: Vec<PushCommitDto>,
}

#[derive(Debug, Deserialize)]
struct RepositoryDto {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct PushCommitDto {
    id: String,
    message: String,
    timestamp: DateTime<Utc>,
    url: String,
}

#[async_trait]
impl GhResource for CommitsResource {
    fn kind(&self) -> &'static str {
        "commit"
    }

    async fn poll(
        &self,
        client: &GhClient,
        repo: &Repo,
        sub_cursor: &ResourceCursor,
        budget: u32,
    ) -> Result<ResourcePoll, GhError> {
        let per_page: u32 = 50.min(budget);
        // Fix 4: discover the default branch from the repository metadata
        // instead of hard-coding "main".  For repositories using "master" or
        // any other default, hard-coding "main" would cause every commit poll
        // to fail — and because `GitHubConnector::poll` returns `Err`
        // immediately on any resource error, that would discard events already
        // fetched for issues and PRs in the same tick.
        //
        // We only call the `/repos/{o}/{r}` endpoint when `branch` is not yet
        // recorded in the cursor (i.e. on the first poll or after a cursor
        // reset).  Subsequent ticks re-use the cached value from the cursor.
        let branch = match sub_cursor.branch.clone() {
            Some(b) => b,
            None => fetch_default_branch(client, repo).await?,
        };
        let mut query: Vec<(&str, String)> =
            vec![("sha", branch.clone()), ("per_page", per_page.to_string())];
        if let Some(since) = sub_cursor.since {
            query.push(("since", since.to_rfc3339()));
        }

        let path = format!("/repos/{}/{}/commits", repo.owner, repo.name);
        let commits: Vec<CommitDto> = client.get_json(&path, &query).await?;

        let mut events: Vec<ConnectorEvent> = Vec::with_capacity(commits.len());
        let last_seen = sub_cursor.last_sha.as_deref();
        for dto in &commits {
            if Some(dto.sha.as_str()) == last_seen {
                break;
            }
            events.push(commit_to_event(dto, repo));
        }

        let next_last_sha = commits
            .first()
            .map(|c| c.sha.clone())
            .or_else(|| sub_cursor.last_sha.clone());
        let max_date = commits
            .iter()
            .map(|c| c.commit.author.date)
            .max()
            .or(sub_cursor.since);
        let next_cursor = ResourceCursor {
            since: max_date,
            last_sha: next_last_sha,
            branch: Some(branch),
            ..ResourceCursor::default()
        };

        let rate_limit_hint = client.rate_state().hint_if_low(50);
        Ok(ResourcePoll {
            events,
            next_cursor,
            rate_limit_hint,
        })
    }

    fn parse_webhook(
        &self,
        event_type: &str,
        delivery_id: &str,
        signature_id: &str,
        body: &[u8],
        repo: &Repo,
    ) -> Result<Vec<ConnectorEvent>, GhError> {
        if event_type != "push" {
            return Ok(vec![]);
        }
        let env: WebhookPushEnvelope = serde_json::from_slice(body)?;
        let expected = repo.scope_value();
        if env.repository.full_name != expected {
            return Err(GhError::Malformed(format!(
                "webhook repo {} != configured {expected}",
                env.repository.full_name
            )));
        }
        let mut out: Vec<ConnectorEvent> = Vec::with_capacity(env.commits.len());
        for c in &env.commits {
            out.push(push_commit_to_event(
                c,
                repo,
                &env.ref_name,
                delivery_id,
                signature_id,
            ));
        }
        Ok(out)
    }
}

fn commit_to_event(dto: &CommitDto, repo: &Repo) -> ConnectorEvent {
    let mut labels = BTreeSet::new();
    labels.insert("source:github".into());
    labels.insert("kind:commit".into());

    let payload = ConnectorPayload::Json {
        mime: "application/json".into(),
        body: serde_json::json!({
            "sha": dto.sha,
            "author_name": dto.commit.author.name,
            "author_email": dto.commit.author.email,
            "author_login": dto.author.as_ref().map(|a| a.login.clone()),
            "message": dto.commit.message,
            "html_url": dto.html_url,
        }),
    };

    ConnectorEvent::new(
        ConnectorEventId::new(Ulid::new().to_string()),
        "github",
        SourceRef::new(
            "commit",
            format!("gh:{}/{}@{}", repo.owner, repo.name, dto.sha),
            None,
        ),
        dto.commit.author.date.timestamp(),
        labels,
        ConnectorScope::project(repo.scope_value()),
        payload,
        DeliveryMode::Poll { cursor: None },
    )
}

fn push_commit_to_event(
    dto: &PushCommitDto,
    repo: &Repo,
    ref_name: &str,
    delivery_id: &str,
    signature_id: &str,
) -> ConnectorEvent {
    let mut labels = BTreeSet::new();
    labels.insert("source:github".into());
    labels.insert("kind:commit".into());

    let payload = ConnectorPayload::Json {
        mime: "application/json".into(),
        body: serde_json::json!({
            "sha": dto.id,
            "message": dto.message,
            "url": dto.url,
            "ref": ref_name,
        }),
    };

    ConnectorEvent::new(
        ConnectorEventId::new(Ulid::new().to_string()),
        "github",
        SourceRef::new(
            "commit",
            format!("gh:{}/{}@{}", repo.owner, repo.name, dto.id),
            None,
        ),
        dto.timestamp.timestamp(),
        labels,
        ConnectorScope::project(repo.scope_value()),
        payload,
        DeliveryMode::Webhook {
            signature_id: format!("{signature_id}:{delivery_id}"),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_connectors_core::CredentialHandle;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn pat_handle(token: &str) -> CredentialHandle {
        let env = serde_json::json!({"kind": "pat", "token": token});
        CredentialHandle::from_bytes(env.to_string().into_bytes())
    }

    #[test]
    fn webhook_push_emits_one_event_per_commit() {
        let body = include_bytes!("../../tests/fixtures/webhook_push.json");
        let r = CommitsResource;
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let events = r
            .parse_webhook("push", "d", "s", body, &repo)
            .expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source_ref.kind, "commit");
        assert!(events[0].labels.contains("kind:commit"));
    }

    #[test]
    fn non_push_event_returns_empty() {
        let r = CommitsResource;
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        assert!(
            r.parse_webhook("issues", "d", "s", b"{}", &repo)
                .unwrap()
                .is_empty()
        );
    }

    /// Fix 4: when `cursor.branch` is `None`, the resource must call
    /// `GET /repos/{o}/{r}` to discover the default branch and use it for the
    /// commit query, rather than falling back to the hard-coded `"main"`.
    #[tokio::test]
    async fn discovers_default_branch_from_repo_meta() {
        let server = MockServer::start().await;

        // Mount: GET /repos/o/r → {"default_branch": "master", ...}
        Mock::given(method("GET"))
            .and(path("/repos/o/r"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1,
                "default_branch": "master",
                "full_name": "o/r"
            })))
            .expect(1)
            .mount(&server)
            .await;

        // Mount: GET /repos/o/r/commits?sha=master → fixture commits.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("sha", "master"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "sha": "deadbeef",
                    "commit": {
                        "author": {
                            "name": "Alice",
                            "email": "a@x.com",
                            "date": "2026-05-24T08:00:00Z"
                        },
                        "committer": {
                            "name": "Alice",
                            "email": "a@x.com",
                            "date": "2026-05-24T08:00:00Z"
                        },
                        "message": "init"
                    },
                    "author": {"login": "alice"},
                    "html_url": "https://github.com/o/r/commit/deadbeef"
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let auth = std::sync::Arc::new(
            crate::auth::GitHubAuth::from_handle(&pat_handle("tok")).expect("auth"),
        );
        let client = GhClient::new(auth, url::Url::parse(&server.uri()).unwrap());
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        // cursor.branch = None → should trigger repo-meta fetch.
        let sub = ResourceCursor::default();
        let result = CommitsResource
            .poll(&client, &repo, &sub, 50)
            .await
            .expect("poll succeeds");

        // The returned cursor must record the discovered branch.
        assert_eq!(
            result.next_cursor.branch.as_deref(),
            Some("master"),
            "cursor must cache the discovered default branch"
        );
        assert_eq!(result.events.len(), 1, "one commit event emitted");
    }
}
