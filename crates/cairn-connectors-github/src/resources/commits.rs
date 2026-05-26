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
        let mut oldest_walked_date: Option<DateTime<Utc>> = None;
        // Track (sha, date) for every commit emitted this tick so we can build the
        // boundary SHA set for the next continuation.
        let mut walked: Vec<(String, DateTime<Utc>)> = Vec::new();
        // Build the set of boundary SHAs from the prior cursor (items already
        // emitted at the pending_until_date boundary on a previous tick).
        let boundary_filter: std::collections::BTreeSet<String> =
            sub_cursor.pending_boundary_shas.iter().cloned().collect();
        // Track SHAs emitted within this poll to dedupe same-timestamp commits
        // across pages within a single tick.
        let mut in_poll_seen: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();

        // Two-mode pagination: never mix `until=` with an unfiltered page counter.
        //
        // - Fresh walk  (pending_until_date == None): no `until=` param; walk
        //   page=1, 2, 3, … of the unfiltered result set within this poll.
        // - Continuation (pending_until_date == Some): always send `until=<date>`;
        //   walk page=<pending_until_page>, page+1, page+2, … within this poll.
        //   The `until=` value is FIXED for the entire continuation and never
        //   changes mid-poll.  Switching fresh→continuation only happens BETWEEN
        //   polls (at the page-cap boundary), not within a poll.
        let in_continuation = sub_cursor.pending_until_date.is_some();
        // The fixed `until=` date for the whole continuation (None on fresh walks).
        let cont_until: Option<DateTime<Utc>> = sub_cursor.pending_until_date;
        // Page number at which this poll starts.
        let start_page: u32 = if in_continuation {
            sub_cursor.pending_until_page.unwrap_or(1)
        } else {
            1
        };

        let mut pages_walked = 0_u32;
        loop {
            if pages_walked >= MAX_PAGES_PER_POLL || budget_remaining == 0 {
                break;
            }
            let pp = per_page.min(budget_remaining);
            let current_page = start_page.saturating_add(pages_walked);

            let mut query: Vec<(&str, String)> = vec![
                ("sha", branch.clone()),
                ("per_page", pp.to_string()),
                ("page", current_page.to_string()),
            ];
            // Fresh walks must NOT include `until=`; the page numbers correspond
            // to the unfiltered result set.  Continuation walks always include the
            // fixed `until=` date so the bounded result set stays stable.
            if in_continuation && let Some(u) = cont_until {
                query.push(("until", u.to_rfc3339()));
            }

            let page_commits: Vec<CommitDto> = client.get_json(&path, &query).await?;
            pages_walked = pages_walked.saturating_add(1);

            if page_commits.is_empty() {
                // No more history (or empty page under continuation) — gap closes.
                closed_gap = true;
                break;
            }

            // Only the first page of a fresh walk identifies the new HEAD.
            if recorded_head.is_none() && !in_continuation {
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
                // Skip SHAs already emitted within this tick's page sequence.
                if in_poll_seen.contains(&dto.sha) {
                    continue;
                }
                if Some(dto.sha.as_str()) == last_seen {
                    closed_gap = true;
                    stop = true;
                    break;
                }
                events.push(commit_to_event(dto, repo));
                in_poll_seen.insert(dto.sha.clone());
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
            // Server-side end of history under the current filter.
            if u32::try_from(page_commits.len()).unwrap_or(u32::MAX) < pp {
                closed_gap = true;
                break;
            }
        }

        // Build the boundary SHA set for the next continuation: SHAs emitted this
        // tick whose author date equals oldest_walked_date.  If continuing under
        // the same `until=` date, merge with the prior set so we don't re-emit
        // commits from earlier ticks at the same boundary.
        let boundary_shas: Vec<String> = if closed_gap {
            Vec::new()
        } else if in_continuation {
            // Continuing under the same `until=` date — merge with prior set.
            let mut s: Vec<String> = sub_cursor.pending_boundary_shas.clone();
            if let Some(d) = oldest_walked_date {
                for (sha, date) in &walked {
                    if *date == d && !s.contains(sha) {
                        s.push(sha.clone());
                    }
                }
            }
            s
        } else {
            // Fresh walk that hit the page cap — record SHAs at the oldest date.
            let mut s: Vec<String> = Vec::new();
            if let Some(d) = oldest_walked_date {
                for (sha, date) in &walked {
                    if *date == d {
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
                pending_until_page: None,
                pending_head: None,
                pending_boundary_shas: Vec::new(),
                ..ResourceCursor::default()
            }
        } else if in_continuation {
            // Continued same `until=` set; advance only the page counter.
            let next_page = start_page.saturating_add(pages_walked);
            ResourceCursor {
                last_sha: sub_cursor.last_sha.clone(),
                branch: Some(branch),
                pending_until_date: cont_until,
                pending_until_page: Some(next_page),
                pending_head: recorded_head.or_else(|| sub_cursor.pending_head.clone()),
                pending_boundary_shas: boundary_shas,
                ..ResourceCursor::default()
            }
        } else {
            // Fresh walk hit cap — record oldest_walked_date as the fixed `until=`
            // so the next poll switches to continuation mode at page 1.
            ResourceCursor {
                last_sha: sub_cursor.last_sha.clone(),
                branch: Some(branch),
                pending_until_date: oldest_walked_date,
                pending_until_page: Some(1),
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
    ///
    /// Uses page-specific mocks so each page returns unique commit SHAs.
    /// `in_poll_seen` deduplicates within a tick, so all 500 unique SHAs are emitted.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn commits_poll_caps_pages_per_call_and_persists_continuation() {
        let server = MockServer::start().await;

        // Mount MAX_PAGES_PER_POLL (10) page-specific mocks, each returning 50
        // unique commits.  The page number sent by the code on page N > 1 will be
        // N itself (because in_continuation=false, current_page = 1 + pages_walked).
        // Page 1 is requested without a `page=` param; pages 2-10 carry `page=N`.
        // All pages also carry `until=` after the first page sets oldest_walked_date.
        //
        // Because this is a fresh walk (no pending_until_date), page 1 is fetched
        // without `page=` or `until=`.  Subsequent pages include both.
        // Use a catch-all for all page-N requests (the exact page param varies per
        // iteration).  Each page must have unique SHAs to avoid in_poll_seen dedup.
        //
        // Strategy: mount 10 page-specific mocks where page N matches `page=N`
        // and returns commits sha(N*50)..(N*50+50).  Page 1 has no `page=` param.
        // The hour spread ensures oldest_walked_date advances so `until=` is stable.

        // Page 1: no `page=` param.
        let make_page = |page_idx: u32| -> Vec<serde_json::Value> {
            let base = page_idx * 50;
            (0_u32..50)
                .map(|i| {
                    let global_i = base + i;
                    // Spread dates: group every 50 by hour so each page has a
                    // distinct oldest_walked_date and `until=` advances each page.
                    let hour = 20_u32.saturating_sub(page_idx);
                    serde_json::json!({
                        "sha": format!("p{page_idx}sha{i:04}"),
                        "commit": {
                            "author": {
                                "name": "A", "email": "a@x.com",
                                "date": format!("2026-05-25T{hour:02}:{i:02}:00Z")
                            },
                            "committer": {
                                "name": "A", "email": "a@x.com",
                                "date": format!("2026-05-25T{hour:02}:{i:02}:00Z")
                            },
                            "message": format!("commit {global_i}")
                        },
                        "author": {"login": "a"},
                        "html_url": format!("https://github.com/o/r/commit/p{page_idx}sha{i:04}")
                    })
                })
                .collect()
        };

        // Page 1 catch-all (no page= param needed; this fires for the first request).
        // Lower priority (5 = default) so it loses to the page-specific mocks below.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(make_page(0)))
            .with_priority(5)
            .mount(&server)
            .await;
        // Pages 2-10: higher priority (1) so they win over the catch-all above.
        for pg in 2_u32..=10 {
            Mock::given(method("GET"))
                .and(path("/repos/o/r/commits"))
                .and(query_param("page", pg.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(make_page(pg - 1)))
                .with_priority(1)
                .mount(&server)
                .await;
        }

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

        // 10 pages × 50 unique commits = 500 events.
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

    /// Fix 1 (round-5): when ALL commits in every page share the same author
    /// date, the `until=<date>` continuation must also send `page=N` to walk
    /// through the bounded result set without getting stuck re-returning the same
    /// boundary items.
    ///
    /// Setup: three pages, all commits at `2026-05-01T00:00:00Z`.
    /// - Poll 1 (no continuation): page 1 — [sha001, sha002] emitted.
    /// - Poll 2 (continuation, page=2): [sha003, sha004] emitted.
    /// - Poll 3 (continuation, page=3): [] → gap closed.
    ///
    /// All 4 SHAs must be emitted, none duplicated, and gap closes at the empty page.
    #[tokio::test]
    // The test body exhaustively sets up three sequential mock servers to validate
    // the full page-increment continuation across same-timestamp boundaries.
    #[allow(clippy::too_many_lines)]
    async fn commits_poll_walks_past_same_timestamp_via_page_increment() {
        use wiremock::matchers::query_param;

        let boundary_ts = "2026-05-01T00:00:00Z";
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

        // Server 1: first poll — page 1, no `until=` yet.
        let server1 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                make_commit("sha001"),
                make_commit("sha002"),
            ])))
            .mount(&server1)
            .await;

        let auth = std::sync::Arc::new(
            crate::auth::GitHubAuth::from_handle(&pat_handle("tok")).expect("auth"),
        );
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };

        // Poll 1: fresh walk, budget=2 so per_page=2; full page → no gap close.
        let sub1 = ResourceCursor {
            branch: Some("main".into()),
            ..ResourceCursor::default()
        };
        let client1 = GhClient::new(auth.clone(), url::Url::parse(&server1.uri()).unwrap());
        let result1 = CommitsResource
            .poll(&client1, &repo, &sub1, 2)
            .await
            .expect("poll 1");

        assert_eq!(result1.events.len(), 2, "sha001 + sha002 on first poll");
        assert!(
            result1.next_cursor.pending_until_date.is_some(),
            "continuation date set after poll 1"
        );
        // pending_until_page should be Some(1) — next poll will request page=2.
        assert_eq!(
            result1.next_cursor.pending_until_page,
            Some(1),
            "pending_until_page starts at 1 after first page cap"
        );

        let emitted_poll1: Vec<String> = result1
            .events
            .iter()
            .filter_map(|e| {
                if let cairn_connectors_core::ConnectorPayload::Json { body, .. } = &e.payload {
                    body.get("sha").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect();
        assert!(emitted_poll1.contains(&"sha001".to_string()));
        assert!(emitted_poll1.contains(&"sha002".to_string()));

        // Server 2: second poll — continuation under `until=`.
        //
        // With pending_until_page=Some(1), the poll sends:
        //   iteration 0: until=boundary_ts&page=1 → sha001+sha002 (filtered by boundary_filter)
        //   iteration 1: until=boundary_ts&page=2 → sha003+sha004 (budget=2 exhausted)
        //
        // The page=1 response returns the same boundary commits; pending_boundary_shas
        // filters them so they are not re-emitted.  The page=2 response has new commits.
        let server2 = MockServer::start().await;
        let until_str = "2026-05-01T00:00:00+00:00";

        // page=1: re-serves sha001+sha002 (will be filtered by boundary_filter).
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("until", until_str))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                make_commit("sha001"),
                make_commit("sha002"),
            ])))
            .mount(&server2)
            .await;

        // page=2: new commits sha003+sha004.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("until", until_str))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                make_commit("sha003"),
                make_commit("sha004"),
            ])))
            .mount(&server2)
            .await;

        let client2 = GhClient::new(auth.clone(), url::Url::parse(&server2.uri()).unwrap());
        // Budget=2 so per_page=2 exactly.  Page=1 returns 2 items (all filtered by
        // boundary_filter) which equals per_page, so the code sees a full page and
        // continues to page=2 rather than closing the gap on a partial page.
        // Page=2 then emits sha003+sha004 and exhausts the budget.
        let result2 = CommitsResource
            .poll(&client2, &repo, &result1.next_cursor, 2)
            .await
            .expect("poll 2");

        assert_eq!(result2.events.len(), 2, "sha003 + sha004 on second poll");
        assert!(
            result2.next_cursor.pending_until_date.is_some(),
            "continuation still open after poll 2"
        );
        // After poll 2: 2 pages walked (page=1 filtered, page=2 emitted, budget=0).
        // next_page = pending_until_page(1) + pages_walked(2) = 3.
        assert_eq!(
            result2.next_cursor.pending_until_page,
            Some(3),
            "pending_until_page advances to 3 after second poll walks pages 1+2"
        );

        let emitted_poll2: Vec<String> = result2
            .events
            .iter()
            .filter_map(|e| {
                if let cairn_connectors_core::ConnectorPayload::Json { body, .. } = &e.payload {
                    body.get("sha").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect();
        assert!(emitted_poll2.contains(&"sha003".to_string()));
        assert!(emitted_poll2.contains(&"sha004".to_string()));
        // sha001/sha002 must NOT be re-emitted.
        assert!(
            !emitted_poll2.contains(&"sha001".to_string()),
            "sha001 must not be re-emitted"
        );
        assert!(
            !emitted_poll2.contains(&"sha002".to_string()),
            "sha002 must not be re-emitted"
        );

        // Server 3: third poll — page=3, empty → gap closes.
        // pending_until_page=Some(3): poll sends until=boundary_ts&page=3.
        let server3 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("until", until_str))
            .and(query_param("page", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server3)
            .await;

        let client3 = GhClient::new(auth.clone(), url::Url::parse(&server3.uri()).unwrap());
        let result3 = CommitsResource
            .poll(&client3, &repo, &result2.next_cursor, 10)
            .await
            .expect("poll 3");

        assert_eq!(result3.events.len(), 0, "no new commits on empty page");
        assert!(
            result3.next_cursor.pending_until_date.is_none(),
            "continuation cleared on empty page"
        );
        assert!(
            result3.next_cursor.pending_until_page.is_none(),
            "pending_until_page cleared on gap close"
        );
    }

    /// Fix 1 (round-6): a fresh walk must NOT include `until=` in the request.
    /// The page numbers of an unfiltered result set must not be mixed with a
    /// `until=`-filtered result set — doing so skips real items.
    #[tokio::test]
    async fn commits_poll_fresh_walk_uses_no_until_param() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Mount a mock that only matches when `until` is NOT in the query.
        // wiremock does not have a "query param absent" matcher, so we mount
        // the expected response at the unfiltered path and a 500 at the
        // `until`-bearing path.  If the code sends `until=`, the 500 fires and
        // the test fails.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("until", ".*")) // regex-style — matches any until value
            .respond_with(ResponseTemplate::new(500)) // must not be reached
            .with_priority(1) // wins over the catch-all below
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"sha": "sha_a", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T10:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T10:00:00Z"}, "message": "a"}, "author": {"login": "a"}, "html_url": "u"},
                {"sha": "sha_b", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T09:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T09:00:00Z"}, "message": "b"}, "author": {"login": "a"}, "html_url": "u2"}
            ])))
            .with_priority(5)
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
        // No pending_until_date → fresh walk.
        let sub = ResourceCursor {
            branch: Some("main".into()),
            ..ResourceCursor::default()
        };
        let result = CommitsResource
            .poll(&client, &repo, &sub, 50)
            .await
            .expect("fresh walk must not send until= and must succeed");

        // Two items returned as a partial page → gap closes; both emitted.
        assert_eq!(result.events.len(), 2, "both commits emitted on fresh walk");
        assert!(
            result.next_cursor.pending_until_date.is_none(),
            "gap closed — no continuation needed"
        );
    }

    /// Fix 1 (round-6): a continuation poll must send the fixed `until=` value
    /// and the correct `page=` from the cursor, never mixing unfiltered page
    /// numbers with a `until=`-filtered result set.
    #[tokio::test]
    async fn commits_poll_continuation_uses_pinned_until() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let until_date = "2026-05-24T12:00:00+00:00";

        // The mock only responds successfully when BOTH `until=<date>` AND
        // `page=3` are present — any other combination returns 500.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("until", until_date))
            .and(query_param("page", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"sha": "cont_sha", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-24T10:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-24T10:00:00Z"}, "message": "resumed"}, "author": {"login": "a"}, "html_url": "u"}
            ])))
            .with_priority(1)
            .mount(&server)
            .await;

        // Catch-all 500: any request that doesn't match the above fails the test.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .respond_with(ResponseTemplate::new(500))
            .with_priority(5)
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
        let pending_date = chrono::DateTime::parse_from_rfc3339(until_date)
            .unwrap()
            .with_timezone(&Utc);
        // Continuation cursor: pending_until_date set, pending_until_page = Some(3).
        let sub = ResourceCursor {
            branch: Some("main".into()),
            pending_until_date: Some(pending_date),
            pending_until_page: Some(3),
            pending_head: Some("old_head".into()),
            ..ResourceCursor::default()
        };
        let result = CommitsResource
            .poll(&client, &repo, &sub, 50)
            .await
            .expect("continuation poll must use fixed until= and page=3");

        // Single item < per_page → gap closes.
        assert_eq!(result.events.len(), 1, "resumed commit emitted");
        assert!(
            result.next_cursor.pending_until_date.is_none(),
            "continuation cleared after gap closes"
        );
    }

    /// Fix 1 (round-6): a fresh walk must fetch page=2 of the unfiltered result
    /// set WITHOUT including `until=` in the request.  Old buggy code sent
    /// `until=<date_of_last_item_on_page_1>&page=2` which is "page 2 of the
    /// filtered set" — a different set of rows that skips real items 51-99.
    ///
    /// Setup: page=1 contains the previously-seen commit as its LAST item
    /// (sentinel `last_sha`).  The code must stop when it finds `last_sha` and
    /// report a closed gap without ever requesting page=2.  This test verifies
    /// that the page=1 request does NOT carry `until=`.  The complementary
    /// guarantee — that a second page is fetched without `until=` — is covered
    /// structurally by `commits_poll_caps_pages_per_call_and_persists_continuation`
    /// (which mounts page-specific mocks for pages 2-10 without `until=`).
    #[tokio::test]
    async fn commits_poll_fresh_walk_does_not_skip_real_page_2() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Mount a 500 for any request that carries `until=` — this fires if the
        // code incorrectly mixes `until=` into a fresh walk.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("until", ".*"))
            .respond_with(ResponseTemplate::new(500))
            .with_priority(1)
            .mount(&server)
            .await;

        // Page 1 (no `until=`): 3 new commits, then `last_sha` as the sentinel.
        // The sentinel closes the gap so no page=2 request is made.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"sha": "new1", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T20:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T20:00:00Z"}, "message": "new1"}, "author": {"login": "a"}, "html_url": "u1"},
                {"sha": "new2", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T19:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T19:00:00Z"}, "message": "new2"}, "author": {"login": "a"}, "html_url": "u2"},
                {"sha": "new3", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T18:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T18:00:00Z"}, "message": "new3"}, "author": {"login": "a"}, "html_url": "u3"},
                {"sha": "sentinel", "commit": {"author": {"name": "A", "email": "a@x.com", "date": "2026-05-25T17:00:00Z"}, "committer": {"name": "A", "email": "a@x.com", "date": "2026-05-25T17:00:00Z"}, "message": "sentinel"}, "author": {"login": "a"}, "html_url": "u4"},
            ])))
            .with_priority(2)
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
        // `last_sha = "sentinel"` simulates a previously-seen commit on the page.
        let sub = ResourceCursor {
            branch: Some("main".into()),
            last_sha: Some("sentinel".into()),
            ..ResourceCursor::default()
        };
        let result = CommitsResource
            .poll(&client, &repo, &sub, 100)
            .await
            .expect("fresh walk must not send until= (would 500) and must succeed");

        // 3 new commits emitted before hitting the sentinel; gap closed.
        assert_eq!(
            result.events.len(),
            3,
            "new1, new2, new3 emitted before sentinel closes the gap"
        );
        // Gap closed: last_sha advances to the new HEAD (new1).
        assert_eq!(
            result.next_cursor.last_sha.as_deref(),
            Some("new1"),
            "last_sha must advance to new HEAD"
        );
        assert!(
            result.next_cursor.pending_until_date.is_none(),
            "no continuation — gap closed on page 1"
        );
    }
}
