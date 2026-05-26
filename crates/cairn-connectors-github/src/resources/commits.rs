//! Commits / push resource.

use std::collections::BTreeSet;

use async_trait::async_trait;
use cairn_connectors_core::{
    ConnectorEvent, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;

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
        // Discover the default branch from the repository metadata instead of
        // hard-coding "main". Only fetched when `branch` is not yet in cursor.
        let branch = match sub_cursor.branch.clone() {
            Some(b) => b,
            None => fetch_default_branch(client, repo).await?,
        };

        let last_seen = sub_cursor.last_sha.as_deref();
        let path = format!("/repos/{}/{}/commits", repo.owner, repo.name);
        let mut events: Vec<ConnectorEvent> = Vec::new();
        let mut new_head: Option<String> = None;
        let mut closed_gap = false;
        let mut budget_remaining = budget;
        let mut page = 1_u32;

        loop {
            if budget_remaining == 0 {
                // Out of budget mid-walk: do NOT advance last_sha so the next poll
                // continues from the same point (no commits get skipped).
                break;
            }
            let pp = per_page.min(budget_remaining);
            let query: Vec<(&str, String)> = vec![
                ("sha", branch.clone()),
                ("per_page", pp.to_string()),
                ("page", page.to_string()),
            ];
            let page_commits: Vec<CommitDto> = client.get_json(&path, &query).await?;

            if page_commits.is_empty() {
                // No more history — full backfill from epoch closes here.
                closed_gap = true;
                break;
            }

            if new_head.is_none() {
                new_head = Some(page_commits[0].sha.clone());
            }

            let mut stop = false;
            for dto in &page_commits {
                if Some(dto.sha.as_str()) == last_seen {
                    closed_gap = true;
                    stop = true;
                    break;
                }
                events.push(commit_to_event(dto, repo));
                budget_remaining = budget_remaining.saturating_sub(1);
                if budget_remaining == 0 {
                    break;
                }
            }
            if stop {
                break;
            }
            if u32::try_from(page_commits.len()).unwrap_or(u32::MAX) < pp {
                // Server-side end of history; treat as gap closed.
                closed_gap = true;
                break;
            }
            page = page.saturating_add(1);
        }

        // Only advance last_sha when the gap is closed; otherwise keep the prior
        // sha so the next poll resumes correctly (prevents permanent history skip
        // when > per_page commits accumulate between polls).
        let next_last_sha = if closed_gap {
            new_head.or_else(|| sub_cursor.last_sha.clone())
        } else {
            sub_cursor.last_sha.clone()
        };
        let next_cursor = ResourceCursor {
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

    let source_ref = SourceRef::new(
        "commit",
        format!("gh:{}/{}@{}", repo.owner, repo.name, dto.sha),
        None,
    );
    // Deterministic event ID: SHA is the revision marker for commits (immutable).
    let event_id = crate::event_id::deterministic("commit", &source_ref.system_id, &dto.sha);

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
        event_id,
        "github",
        source_ref,
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

    let source_ref = SourceRef::new(
        "commit",
        format!("gh:{}/{}@{}", repo.owner, repo.name, dto.id),
        None,
    );
    // Deterministic event ID: SHA (`dto.id`) is the revision marker for push commits.
    let event_id = crate::event_id::deterministic("commit", &source_ref.system_id, &dto.id);

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
        event_id,
        "github",
        source_ref,
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

    /// Fix 1: when `last_sha` is on page 2, paginate until the gap is closed
    /// and emit all commits from page 1 up to (but not including) `last_sha`.
    #[tokio::test]
    async fn commits_poll_walks_multiple_pages_until_last_sha() {
        let server = MockServer::start().await;

        // Page 1: two new commits (newer than last_sha).
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "sha": "new001",
                    "commit": {
                        "author": {"name": "Alice", "email": "a@x.com", "date": "2026-05-25T10:00:00Z"},
                        "committer": {"name": "Alice", "email": "a@x.com", "date": "2026-05-25T10:00:00Z"},
                        "message": "new commit 1"
                    },
                    "author": {"login": "alice"},
                    "html_url": "https://github.com/o/r/commit/new001"
                },
                {
                    "sha": "new002",
                    "commit": {
                        "author": {"name": "Bob", "email": "b@x.com", "date": "2026-05-25T09:00:00Z"},
                        "committer": {"name": "Bob", "email": "b@x.com", "date": "2026-05-25T09:00:00Z"},
                        "message": "new commit 2"
                    },
                    "author": {"login": "bob"},
                    "html_url": "https://github.com/o/r/commit/new002"
                }
            ])))
            .mount(&server)
            .await;

        // Page 2: contains the last_sha we already observed.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "sha": "old_cursor",
                    "commit": {
                        "author": {"name": "Alice", "email": "a@x.com", "date": "2026-05-24T08:00:00Z"},
                        "committer": {"name": "Alice", "email": "a@x.com", "date": "2026-05-24T08:00:00Z"},
                        "message": "previously seen"
                    },
                    "author": {"login": "alice"},
                    "html_url": "https://github.com/o/r/commit/old_cursor"
                }
            ])))
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
        let sub = ResourceCursor {
            last_sha: Some("old_cursor".into()),
            branch: Some("main".into()),
            ..ResourceCursor::default()
        };

        let result = CommitsResource
            .poll(&client, &repo, &sub, 100)
            .await
            .expect("poll succeeds");

        // Both new commits from page 1 must be emitted.
        assert_eq!(result.events.len(), 2, "both page-1 commits emitted");
        let shas: Vec<&str> = result
            .events
            .iter()
            .filter_map(|e| {
                if let cairn_connectors_core::ConnectorPayload::Json { body, .. } = &e.payload {
                    body.get("sha").and_then(|v| v.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(shas.contains(&"new001"), "new001 must be in events");
        assert!(shas.contains(&"new002"), "new002 must be in events");

        // Gap was closed: last_sha advances to new HEAD (new001).
        assert_eq!(
            result.next_cursor.last_sha.as_deref(),
            Some("new001"),
            "last_sha advances to new HEAD after closing gap"
        );
    }

    /// Fix 1: when budget is exhausted before the gap is closed, `last_sha`
    /// must NOT be advanced (so the next poll can resume from the same point).
    #[tokio::test]
    async fn commits_poll_budget_exhausted_keeps_prior_sha() {
        let server = MockServer::start().await;

        // Five commits on page 1; none match last_sha (which is on some future page).
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"sha": "c1", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T10:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T10:00:00Z"}, "message": "m1"}, "author": {"login": "a"}, "html_url": "u1"},
                {"sha": "c2", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T09:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T09:00:00Z"}, "message": "m2"}, "author": {"login": "a"}, "html_url": "u2"},
                {"sha": "c3", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T08:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T08:00:00Z"}, "message": "m3"}, "author": {"login": "a"}, "html_url": "u3"},
                {"sha": "c4", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T07:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T07:00:00Z"}, "message": "m4"}, "author": {"login": "a"}, "html_url": "u4"},
                {"sha": "c5", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T06:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T06:00:00Z"}, "message": "m5"}, "author": {"login": "a"}, "html_url": "u5"}
            ])))
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
        let prior_sha = "prior_sha_not_in_page";
        let sub = ResourceCursor {
            last_sha: Some(prior_sha.into()),
            branch: Some("main".into()),
            ..ResourceCursor::default()
        };

        // Budget of 2: only 2 events emitted, gap not closed.
        let result = CommitsResource
            .poll(&client, &repo, &sub, 2)
            .await
            .expect("poll succeeds");

        assert_eq!(result.events.len(), 2, "only 2 events with budget=2");
        // last_sha must remain unchanged: gap was not closed.
        assert_eq!(
            result.next_cursor.last_sha.as_deref(),
            Some(prior_sha),
            "last_sha must not advance when budget exhausted mid-walk"
        );
    }
}
