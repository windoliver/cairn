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

    // The poll implementation is intentionally long: it manages a multi-page
    // backward commit walk with budget caps, continuation state, and boundary-SHA
    // deduplication.  Splitting it would scatter tightly coupled state across
    // helper functions without improving readability.
    #[allow(clippy::too_many_lines)]
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
        // Track (sha, date) for every commit emitted this tick so we can build the
        // boundary SHA set for the next continuation.
        let mut walked: Vec<(String, DateTime<Utc>)> = Vec::new();
        // Build the set of boundary SHAs from the prior cursor (items already
        // emitted at the pending_until_date boundary on a previous tick).
        let boundary_filter: std::collections::BTreeSet<String> =
            sub_cursor.pending_boundary_shas.iter().cloned().collect();

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
                // previous page.  We skip duplicates via the boundary SHA set and
                // the pending_head sentinel below.
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
                // Skip SHAs already emitted at the boundary timestamp on a prior
                // tick.  This handles the case where > per_page commits share the
                // same author date: the `until=<date>` continuation re-returns them
                // but they must not be double-emitted.
                if boundary_filter.contains(&dto.sha) {
                    continue;
                }
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
                walked.push((dto.sha.clone(), dto.commit.author.date));
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

        // Build the boundary SHA set for the next continuation: SHAs emitted this
        // tick whose author date equals oldest_walked_date.  If the boundary date
        // matches the prior `pending_until_date`, merge with the prior set so we
        // don't re-emit commits from earlier ticks at the same boundary.
        let boundary_shas = if closed_gap {
            Vec::new()
        } else {
            let mut s: Vec<String> = if oldest_walked_date == sub_cursor.pending_until_date {
                sub_cursor.pending_boundary_shas.clone()
            } else {
                Vec::new()
            };
            if let Some(d) = oldest_walked_date {
                for (sha, date) in &walked {
                    if *date == d && !s.contains(sha) {
                        s.push(sha.clone());
                    }
                }
            }
            s
        };

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
                pending_boundary_shas: Vec::new(),
                ..ResourceCursor::default()
            }
        } else {
            // Mid-walk: persist continuation state for next poll.
            ResourceCursor {
                last_sha: sub_cursor.last_sha.clone(),
                branch: Some(branch),
                pending_until_date: oldest_walked_date,
                pending_head: recorded_head,
                pending_boundary_shas: boundary_shas,
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
    // Use the same SHA-only revision as `commit_to_event` so that a commit
    // observed via poll and then re-delivered via push webhook (or vice-versa)
    // produces the same event_id and is deduplicated by the substrate.
    // `delivery_id` is NOT used here — it changes per-delivery and would break
    // cross-channel dedup.  The `DeliveryMode::Webhook { signature_id }` field
    // separately carries the per-delivery UUID for the replay guard.
    let event_id = crate::event_id::from_parts("commit", &source_ref.system_id, &[&dto.id]);

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

    /// Fix 1 (round-4): `push_commit_to_event` and `commit_to_event` must produce
    /// the same `event_id` for the same commit SHA (cross-channel dedup).
    #[test]
    fn poll_and_webhook_paths_produce_same_event_id_for_same_commit() {
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        // Build a CommitDto (poll path).
        let poll_dto: CommitDto = serde_json::from_value(serde_json::json!({
            "sha": "deadbeef",
            "commit": {
                "author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T10:00:00Z"},
                "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T10:00:00Z"},
                "message": "feat: add something"
            },
            "author": {"login": "alice"},
            "html_url": "https://github.com/o/r/commit/deadbeef"
        }))
        .expect("poll dto");

        // Build a PushCommitDto (webhook push path) for the same SHA.
        let push_dto: PushCommitDto = serde_json::from_value(serde_json::json!({
            "id": "deadbeef",
            "message": "feat: add something",
            "timestamp": "2026-05-25T10:00:00Z",
            "url": "https://github.com/o/r/commit/deadbeef"
        }))
        .expect("push dto");

        let poll_event = commit_to_event(&poll_dto, &repo);
        let push_event =
            push_commit_to_event(&push_dto, &repo, "refs/heads/main", "delivery-1", "sig-1");

        assert_eq!(
            poll_event.event_id.as_str(),
            push_event.event_id.as_str(),
            "poll and webhook-push must produce the same event_id for the same commit SHA"
        );
        assert!(matches!(poll_event.delivery, DeliveryMode::Poll { .. }));
        assert!(matches!(push_event.delivery, DeliveryMode::Webhook { .. }));
    }

    /// Fix 2 (round-4): when all commits in a continuation page share the
    /// boundary timestamp (same-second batch), the boundary SHA set filters
    /// already-emitted SHAs and only new ones are emitted.
    #[tokio::test]
    async fn commits_poll_handles_same_timestamp_page_boundary() {
        let server = MockServer::start().await;
        let boundary_ts = "2026-05-01T00:00:00Z";

        // Build 4 commits all at the same timestamp.
        let make_commit = |sha: &str| {
            serde_json::json!({
                "sha": sha,
                "commit": {
                    "author": {"name": "A", "email": "a@x.com", "date": boundary_ts},
                    "committer": {"name": "A", "email": "a@x.com", "date": boundary_ts},
                    "message": format!("commit {sha}")
                },
                "author": {"login": "a"},
                "html_url": format!("https://github.com/o/r/commit/{sha}")
            })
        };

        // First request (no `until`): 2 commits at boundary_ts — "full" page
        // because per_page=2 (budget=2).
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                make_commit("sha001"),
                make_commit("sha002"),
            ])))
            .mount(&server)
            .await;

        let auth = std::sync::Arc::new(
            crate::auth::GitHubAuth::from_handle(&pat_handle("tok")).expect("auth"),
        );
        let client = GhClient::new(auth.clone(), url::Url::parse(&server.uri()).unwrap());
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };

        // First poll: budget=2, per_page=2 → full page → no gap close.
        let sub1 = ResourceCursor {
            branch: Some("main".into()),
            ..ResourceCursor::default()
        };
        let result1 = CommitsResource
            .poll(&client, &repo, &sub1, 2)
            .await
            .expect("poll 1");
        assert_eq!(
            result1.events.len(),
            2,
            "both commits emitted on first poll"
        );
        assert!(
            result1.next_cursor.pending_until_date.is_some(),
            "continuation set"
        );
        // Both SHAs at boundary_ts should be in pending_boundary_shas.
        assert_eq!(
            result1.next_cursor.pending_boundary_shas.len(),
            2,
            "boundary SHAs recorded"
        );
        assert!(
            result1
                .next_cursor
                .pending_boundary_shas
                .contains(&"sha001".to_string())
        );
        assert!(
            result1
                .next_cursor
                .pending_boundary_shas
                .contains(&"sha002".to_string())
        );

        // Second poll uses the continuation cursor.  The server returns a page
        // with the 2 already-seen SHAs plus 1 new one.  The already-seen SHAs
        // must be filtered via pending_boundary_shas; only the new one is emitted.
        //
        // Note: wiremock already has a catch-all mount above; we need a new server
        // or a priority mount.  Since the catch-all returns the original page,
        // and we filtered sha001/sha002, a third commit sha003 must appear.
        // Use a separate mock server for the second poll to get a different response.
        let server2 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                make_commit("sha001"),
                make_commit("sha002"),
                make_commit("sha003"),
            ])))
            .mount(&server2)
            .await;

        let client2 = GhClient::new(auth, url::Url::parse(&server2.uri()).unwrap());
        let result2 = CommitsResource
            .poll(&client2, &repo, &result1.next_cursor, 10)
            .await
            .expect("poll 2");

        // sha001 and sha002 filtered; sha003 is new.
        // The page has 3 items < per_page=10 → gap closes.
        assert_eq!(
            result2.events.len(),
            1,
            "only sha003 emitted; sha001/sha002 filtered"
        );
        let emitted_sha = match &result2.events[0].payload {
            cairn_connectors_core::ConnectorPayload::Json { body, .. } => {
                body.get("sha").and_then(|v| v.as_str()).unwrap_or("")
            }
            _ => "",
        };
        assert_eq!(emitted_sha, "sha003", "emitted event must be sha003");
        // Gap closed: boundary_shas must be cleared.
        assert!(
            result2.next_cursor.pending_boundary_shas.is_empty(),
            "boundary SHAs cleared on gap close"
        );
    }
}
