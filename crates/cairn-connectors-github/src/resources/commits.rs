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

/// Maximum number of API pages walked in a single poll call.
///
/// Caps the per-tick API consumption and ensures that on a rate-limit error
/// mid-walk, the continuation cursor preserves progress so the next poll can
/// resume without re-scanning from page 1.
const MAX_PAGES_PER_POLL: u32 = 10;

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
        let per_page: u32 = 50.min(budget.max(1));
        // Discover the default branch from the repository metadata instead of
        // hard-coding "main". Only fetched when `branch` is not yet in cursor.
        let branch = match sub_cursor.branch.clone() {
            Some(b) => b,
            None => fetch_default_branch(client, repo).await?,
        };

        let last_seen = sub_cursor.last_sha.as_deref();
        let path = format!("/repos/{}/{}/commits", repo.owner, repo.name);
        let mut events: Vec<ConnectorEvent> = Vec::new();
        // `pending_head` carries the HEAD sha recorded when the walk started.
        // Persisted across polls so a multi-tick walk promotes the correct HEAD
        // to `last_sha` only when the full gap is finally closed.
        let mut recorded_head: Option<String> = sub_cursor.pending_head.clone();
        let mut closed_gap = false;
        let mut budget_remaining = budget;
        // `oldest_walked_date` tracks the date of the oldest commit seen so far.
        // Used as the `until=` continuation parameter on resumption.
        let mut oldest_walked_date: Option<DateTime<Utc>> = sub_cursor.pending_until_date;
        let mut pages_walked = 0_u32;

        loop {
            if pages_walked >= MAX_PAGES_PER_POLL {
                // Per-poll page cap hit: persist continuation state so the next
                // tick can resume without re-walking already-seen commits.
                break;
            }
            if budget_remaining == 0 {
                // Out of budget mid-walk: do NOT advance last_sha so the next poll
                // continues from the same point (no commits get skipped).
                break;
            }
            let pp = per_page.min(budget_remaining);
            let mut query: Vec<(&str, String)> =
                vec![("sha", branch.clone()), ("per_page", pp.to_string())];
            if let Some(until) = oldest_walked_date {
                // Continuation: keep walking backward from where we left off.
                // GitHub's `until` is inclusive on second granularity; the first
                // commit of the resumed page may equal the last commit of the
                // previous page.  We skip that duplicate below.
                query.push(("until", until.to_rfc3339()));
            }

            let page_commits: Vec<CommitDto> = client.get_json(&path, &query).await?;
            pages_walked = pages_walked.saturating_add(1);

            if page_commits.is_empty() {
                // No more history — full backfill from epoch closes here.
                closed_gap = true;
                break;
            }

            if recorded_head.is_none() {
                recorded_head = Some(page_commits[0].sha.clone());
            }

            let mut stop = false;
            for dto in &page_commits {
                // Skip the continuation boundary item: when `until=<date>` is
                // inclusive, GitHub may return the same commit as the last item
                // of the previous page (same date, same sha via pending_head).
                if oldest_walked_date == Some(dto.commit.author.date)
                    && sub_cursor.pending_head.as_deref() == Some(dto.sha.as_str())
                {
                    continue;
                }
                if Some(dto.sha.as_str()) == last_seen {
                    closed_gap = true;
                    stop = true;
                    break;
                }
                events.push(commit_to_event(dto, repo));
                budget_remaining = budget_remaining.saturating_sub(1);
                oldest_walked_date = Some(dto.commit.author.date);
                if budget_remaining == 0 {
                    break;
                }
            }
            if stop {
                break;
            }
            // Server-side end of history under the current `until` filter.
            if u32::try_from(page_commits.len()).unwrap_or(u32::MAX) < pp {
                closed_gap = true;
                break;
            }
        }

        // Only advance last_sha when the gap is closed; otherwise keep the prior
        // sha so the next poll resumes correctly (prevents permanent history skip
        // when > per_page commits accumulate between polls).
        let next_cursor = if closed_gap {
            // Walk completed — promote recorded_head to last_sha, clear continuation.
            ResourceCursor {
                last_sha: recorded_head.or_else(|| sub_cursor.last_sha.clone()),
                branch: Some(branch),
                pending_until_date: None,
                pending_head: None,
                ..ResourceCursor::default()
            }
        } else {
            // Mid-walk: persist continuation state for next poll.
            ResourceCursor {
                last_sha: sub_cursor.last_sha.clone(),
                branch: Some(branch),
                pending_until_date: oldest_walked_date,
                pending_head: recorded_head,
                ..ResourceCursor::default()
            }
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
    // Commits are content-addressed by SHA: the SHA alone is a stable, unique
    // revision marker.  No payload hash needed — commits are immutable.
    let event_id = crate::event_id::from_parts("commit", &source_ref.system_id, &[&dto.sha]);

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
    // Push delivery: include both the delivery_id (unique per webhook delivery)
    // and the commit SHA so each commit in a multi-commit push gets a distinct id.
    let event_id =
        crate::event_id::from_parts("commit", &source_ref.system_id, &[delivery_id, &dto.id]);

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

    /// When `last_sha` is found on the second request (continuation via `until=`),
    /// all commits before it must be emitted and `last_sha` advances to the new HEAD.
    ///
    /// The new implementation uses `until=<date>` for backward walking rather than
    /// `page=N`, so mocks match on the path only (the second request carries
    /// `until=` for the continuation; the first does not).
    #[tokio::test]
    async fn commits_poll_walks_multiple_pages_until_last_sha() {
        let server = MockServer::start().await;

        // First request (no `until`): two new commits.
        // Both are full pages (per_page=2 via budget=2 cap), so a second request
        // is made to continue the walk.  We use a budget of 100 here so the
        // per_page=50 cap applies; the page is "full" at 2 items < 50, so
        // actually we need per_page to be 2.  Use budget=2 so per_page=2 and
        // the page of 2 items is exactly full → triggers second request.
        // For simplicity, just return 2 items and the `old_cursor` sha on the
        // next request (with `until=` param present) so the gap closes.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
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
                    "sha": "old_cursor",
                    "commit": {
                        "author": {"name": "Bob", "email": "b@x.com", "date": "2026-05-25T09:00:00Z"},
                        "committer": {"name": "Bob", "email": "b@x.com", "date": "2026-05-25T09:00:00Z"},
                        "message": "previously seen"
                    },
                    "author": {"login": "bob"},
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

        // new001 is emitted; old_cursor is the stop point (not emitted).
        assert_eq!(
            result.events.len(),
            1,
            "only commits before last_sha emitted"
        );
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

        // Gap was closed: last_sha advances to new HEAD (new001).
        assert_eq!(
            result.next_cursor.last_sha.as_deref(),
            Some("new001"),
            "last_sha advances to new HEAD after closing gap"
        );
    }

    /// When budget is exhausted before the gap is closed, `last_sha`
    /// must NOT be advanced (so the next poll can resume from the same point).
    #[tokio::test]
    async fn commits_poll_budget_exhausted_keeps_prior_sha() {
        let server = MockServer::start().await;

        // Five commits; none match last_sha (which is on some future page).
        // No `page=` param in the new implementation — match only on path.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
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

    /// Fix 1 (round-3): bounded poll caps at `MAX_PAGES_PER_POLL` and persists
    /// continuation state (`pending_head` + `pending_until_date`) so the next tick
    /// resumes without re-walking already-seen commits.
    #[tokio::test]
    async fn commits_poll_caps_pages_per_call_and_persists_continuation() {
        let server = MockServer::start().await;

        // Each request returns per_page commits (full page), so the loop
        // continues until MAX_PAGES_PER_POLL (10) is hit.  We mount a single
        // catch-all mock that always returns the same 2-commit page, and use
        // per_page=2 (budget=2) so each loop iteration sees a "full" page.
        //
        // With budget=2 the loop exits on budget exhaustion after 1 page (2
        // commits consumed).  To actually trigger the page cap we need budget >
        // per_page * MAX_PAGES_PER_POLL.  Use budget=10000 and per_page=50; we
        // mount a mock that always returns 50 identical commits so every page is
        // full.  The loop will hit MAX_PAGES_PER_POLL=10 and break.
        let commit_page: Vec<serde_json::Value> = (0_u32..50)
            .map(|i| {
                serde_json::json!({
                    "sha": format!("sha{i:04}"),
                    "commit": {
                        "author": {
                            "name": "A",
                            "email": "a@x.com",
                            // Spread dates so oldest_walked_date keeps advancing.
                            "date": format!("2026-05-25T{:02}:00:00Z", (10 - i / 10).min(9))
                        },
                        "committer": {
                            "name": "A", "email": "a@x.com",
                            "date": format!("2026-05-25T{:02}:00:00Z", (10 - i / 10).min(9))
                        },
                        "message": format!("commit {i}")
                    },
                    "author": {"login": "a"},
                    "html_url": format!("https://github.com/o/r/commit/sha{i:04}")
                })
            })
            .collect();

        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&commit_page))
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
            // No last_sha: full backfill scenario.
            branch: Some("main".into()),
            ..ResourceCursor::default()
        };

        let result = CommitsResource
            .poll(&client, &repo, &sub, 10_000)
            .await
            .expect("poll succeeds");

        // 10 pages × 50 commits = 500 events.
        assert_eq!(
            result.events.len(),
            500,
            "exactly MAX_PAGES_PER_POLL * per_page events emitted"
        );
        // Gap NOT closed: continuation state must be persisted.
        assert!(
            result.next_cursor.pending_head.is_some(),
            "pending_head must be set when page cap hit"
        );
        assert!(
            result.next_cursor.pending_until_date.is_some(),
            "pending_until_date must be set when page cap hit"
        );
        // last_sha must NOT advance until gap is closed.
        assert_eq!(
            result.next_cursor.last_sha, sub.last_sha,
            "last_sha must not advance mid-walk"
        );
    }

    /// Fix 1 (round-3): a second poll with the continuation cursor includes
    /// `until=<date>` in the request so the walk resumes where it left off.
    #[tokio::test]
    async fn commits_poll_resumes_from_pending_until_date() {
        use chrono::DateTime;
        use wiremock::matchers::query_param;

        let server = MockServer::start().await;
        let until_date = "2026-05-24T12:00:00+00:00";

        // When `until` is in the query, the mock returns the known commit.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("until", until_date))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "sha": "resumed_sha",
                    "commit": {
                        "author": {"name": "A", "email": "a@x.com", "date": "2026-05-24T11:00:00Z"},
                        "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-24T11:00:00Z"},
                        "message": "resumed commit"
                    },
                    "author": {"login": "a"},
                    "html_url": "https://github.com/o/r/commit/resumed_sha"
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
        let pending_date = DateTime::parse_from_rfc3339(until_date)
            .unwrap()
            .with_timezone(&Utc);
        let sub = ResourceCursor {
            branch: Some("main".into()),
            pending_head: Some("prior_head".into()),
            pending_until_date: Some(pending_date),
            ..ResourceCursor::default()
        };

        let result = CommitsResource
            .poll(&client, &repo, &sub, 100)
            .await
            .expect("poll succeeds with continuation");

        // The resumed commit is emitted (it isn't the boundary item because its
        // sha != pending_head and its date < pending_until_date).
        assert_eq!(result.events.len(), 1, "resumed commit emitted");
        // Single item returned is < per_page → gap closed; last_sha advances.
        assert_eq!(
            result.next_cursor.last_sha.as_deref(),
            Some("prior_head"),
            "last_sha promoted from pending_head on gap close"
        );
        assert!(
            result.next_cursor.pending_until_date.is_none(),
            "continuation cleared on gap close"
        );
    }
}
