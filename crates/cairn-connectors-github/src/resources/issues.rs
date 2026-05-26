//! Issues resource (`issue_comment` ingestion out of scope for v0.3 slice 1; see spec §1.2).

use std::collections::BTreeSet;

use async_trait::async_trait;
use cairn_connectors_core::{
    ConnectorEvent, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use crate::client::GhClient;
use crate::cursor::ResourceCursor;
use crate::error::GhError;
use crate::resources::{GhResource, Repo, ResourcePoll};

pub(crate) struct IssuesResource;

#[derive(Debug, Deserialize)]
struct IssueDto {
    id: u64,
    number: u64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    state: String,
    user: UserDto,
    // Captured from the API response for completeness; the event timestamp
    // uses `updated_at`, so `created_at` is consumed by serde but not read
    // by Rust code in this slice.
    #[allow(dead_code)]
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    html_url: String,
    /// GitHub's `/repos/{o}/{r}/issues` endpoint returns both issues AND pull
    /// requests.  PRs carry a `pull_request` marker object; real issues do not.
    /// When this field is `Some`, the DTO represents a PR that is also owned
    /// by `PrsResource` — skip it here to avoid duplicate, misclassified records.
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct UserDto {
    login: String,
}

#[derive(Debug, Deserialize)]
struct WebhookIssueEnvelope {
    action: String,
    issue: IssueDto,
    repository: RepositoryDto,
}

#[derive(Debug, Deserialize)]
struct RepositoryDto {
    full_name: String,
}

#[async_trait]
impl GhResource for IssuesResource {
    fn kind(&self) -> &'static str {
        "issue"
    }

    async fn poll(
        &self,
        client: &GhClient,
        repo: &Repo,
        sub_cursor: &ResourceCursor,
        budget: u32,
    ) -> Result<ResourcePoll, GhError> {
        let per_page: u32 = 50.min(budget);
        let page = sub_cursor.page.unwrap_or(1);
        let mut query: Vec<(&str, String)> = vec![
            ("state", "all".into()),
            ("sort", "updated".into()),
            ("direction", "asc".into()),
            ("per_page", per_page.to_string()),
            ("page", page.to_string()),
        ];
        if let Some(since) = sub_cursor.since {
            query.push(("since", since.to_rfc3339()));
        }

        let path = format!("/repos/{}/{}/issues", repo.owner, repo.name);
        let issues: Vec<IssueDto> = client.get_json(&path, &query).await?;

        // Build the set of event IDs already emitted at the boundary timestamp.
        // These are skipped to avoid re-emitting the same rows in steady state
        // (when the cursor advances to `max_updated - 1s` for overlap safety, the
        // boundary row is re-served every poll until a newer item appears).
        let boundary_skip: std::collections::BTreeSet<String> = sub_cursor
            .pending_boundary_event_ids
            .iter()
            .cloned()
            .collect();

        // Track (event, updated_at) pairs so we can record the boundary IDs later.
        let mut events_with_meta: Vec<(ConnectorEvent, DateTime<Utc>)> =
            Vec::with_capacity(issues.len());
        // Carry forward any in-progress max seen from prior pages; initialize
        // from `pending_since` so exact-full-page windows accumulate correctly
        // across multiple poll calls before exhaustion.
        let mut max_updated = sub_cursor.pending_since.or(sub_cursor.since);
        for dto in &issues {
            // Update the high-water mark for EVERY returned row, including PRs,
            // so the cursor advances even on PR-only pages.  Failing to do this
            // causes the cursor to stall: the next poll replays the same window
            // because `max_updated` never moved past the PR rows.
            if max_updated.is_none_or(|t| dto.updated_at > t) {
                max_updated = Some(dto.updated_at);
            }
            // Skip pull requests returned by /issues.
            // GitHub's /repos/{o}/{r}/issues endpoint returns both issues and
            // PRs.  PRs carry a non-null `pull_request` marker field.
            // `PrsResource` already owns PRs via /repos/{o}/{r}/pulls — emitting
            // them here too would produce duplicate, misclassified records.
            // The timestamp bump above already advanced the cursor past them.
            if dto.pull_request.is_some() {
                continue;
            }
            let event = issue_to_event(dto, repo, None);
            // Skip events already emitted at the boundary timestamp so that the
            // deliberate 1-second cursor overlap doesn't re-spool the same rows
            // in a no-update steady state.
            if boundary_skip.contains(event.event_id.as_str()) {
                continue;
            }
            events_with_meta.push((event, dto.updated_at));
        }

        let events: Vec<ConnectorEvent> = events_with_meta.iter().map(|(e, _)| e.clone()).collect();

        let exhausted = u32::try_from(issues.len()).unwrap_or(u32::MAX) < per_page;
        // Only advance `since` when the current `since`-window is exhausted
        // (partial page).  While paginating through a full window keep `since`
        // stable and accumulate `pending_since` so exact-full-page counts
        // (50, 100, …) do not replay the same window on the next poll.
        // Advancing `since` mid-window would shift the result set and cause
        // page N of the new window to skip items that were on page N-1 of the
        // old window.
        let next_cursor = if exhausted {
            // Window done: commit pending_since → since, reset page + pending.
            // Advance `since` to `max_updated - 1s` (overlap by 1 second) so that
            // items updated at the exact same second as `max_updated` are re-served
            // on the next poll.  Deterministic event IDs dedupe any re-served items
            // the substrate has already ingested, so the overlap is harmless.
            //
            // Record the event IDs emitted at `max_updated` so the next poll can
            // skip re-emitting them (the 1-second overlap re-serves them, but in a
            // no-update steady state that causes unnecessary churn).
            let new_boundary_ids: Vec<String> = match max_updated {
                Some(m) => {
                    let ids_at_max: Vec<String> = events_with_meta
                        .iter()
                        .filter(|(_, d)| *d == m)
                        .map(|(e, _)| e.event_id.as_str().to_owned())
                        .collect();
                    if ids_at_max.is_empty() {
                        // No NEW events were emitted at the high-water timestamp.
                        // This happens in the all-skipped steady-state: the overlap
                        // rewind re-served only items in the boundary skip set, so
                        // `events_with_meta` is empty.  Carry the prior IDs forward
                        // to keep suppressing them on the next poll.
                        // It is also safe for the "boundary advanced" case: prior IDs
                        // refer to a timestamp ≤ the new `since`, so they won't appear
                        // in future polls and carrying them forward is harmless.
                        sub_cursor.pending_boundary_event_ids.clone()
                    } else {
                        ids_at_max
                    }
                }
                None => {
                    // No rows at all this tick — carry forward so the skip stays active.
                    sub_cursor.pending_boundary_event_ids.clone()
                }
            };
            ResourceCursor {
                since: max_updated
                    .map(|t| t - Duration::seconds(1))
                    .or(sub_cursor.since),
                page: Some(1),
                pending_since: None,
                pending_boundary_event_ids: new_boundary_ids,
                ..ResourceCursor::default()
            }
        } else {
            // Still paginating same since window — keep since stable, advance
            // pending_since so it survives to the exhaustion page. Carry forward
            // boundary IDs from prior state unchanged.
            ResourceCursor {
                since: sub_cursor.since,
                page: Some(page + 1),
                pending_since: max_updated,
                pending_boundary_event_ids: sub_cursor.pending_boundary_event_ids.clone(),
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
        match event_type {
            "issues" => {
                let env: WebhookIssueEnvelope = serde_json::from_slice(body)?;
                let expected = repo.scope_value();
                if env.repository.full_name != expected {
                    return Err(GhError::Malformed(format!(
                        "webhook for {} does not match configured repo {expected}",
                        env.repository.full_name
                    )));
                }
                Ok(vec![issue_to_event(
                    &env.issue,
                    repo,
                    Some((delivery_id, signature_id, &env.action)),
                )])
            }
            _ => Ok(vec![]),
        }
    }
}

fn issue_to_event(
    dto: &IssueDto,
    repo: &Repo,
    webhook_meta: Option<(&str, &str, &str)>,
) -> ConnectorEvent {
    let source_ref = SourceRef::new(
        "issue",
        format!("gh:{}/{}#{}", repo.owner, repo.name, dto.number),
        None,
    );
    let mut labels: BTreeSet<String> = BTreeSet::new();
    labels.insert("source:github".into());
    labels.insert("kind:issue".into());

    let payload_value = serde_json::json!({
        "id": dto.id,
        "number": dto.number,
        "title": dto.title,
        "body": dto.body,
        "state": dto.state,
        "user": dto.user.login,
        "html_url": dto.html_url,
    });
    // Deterministic event ID: always keyed on upstream-object identity so that
    // the same issue update is deduplicated regardless of whether it arrives via
    // poll or webhook.  `delivery_id` is NOT used here — it changes per-delivery
    // and would break cross-channel dedup.  Instead, timestamp + payload content
    // hash captures both "which version" and "which edit within 1s" invariants.
    // The `DeliveryMode::Webhook { signature_id }` field separately encodes the
    // per-delivery UUID for the substrate's webhook replay guard.
    let ts = dto.updated_at.timestamp().to_string();
    let payload_rev = crate::event_id::payload_revision(&payload_value);
    let event_id =
        crate::event_id::from_parts("issue", &source_ref.system_id, &[&ts, &payload_rev]);

    let payload = ConnectorPayload::Json {
        mime: "application/json".into(),
        body: payload_value,
    };

    let delivery = match webhook_meta {
        None => DeliveryMode::Poll { cursor: None },
        Some((delivery_id, signature_id, _action)) => DeliveryMode::Webhook {
            signature_id: format!("{signature_id}:{delivery_id}"),
        },
    };

    ConnectorEvent::new(
        event_id,
        "github",
        source_ref,
        dto.updated_at.timestamp(),
        labels,
        ConnectorScope::project(repo.scope_value()),
        payload,
        delivery,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_parse_extracts_issue_event() {
        let body = include_bytes!("../../tests/fixtures/webhook_issues_opened.json");
        let r = IssuesResource;
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let events = r
            .parse_webhook("issues", "deliver-abc", "sigid-1", body, &repo)
            .expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source_ref.kind, "issue");
        assert!(events[0].labels.contains("kind:issue"));
        assert!(matches!(events[0].delivery, DeliveryMode::Webhook { .. }));
    }

    #[test]
    fn webhook_parse_rejects_mismatched_repo() {
        let body = include_bytes!("../../tests/fixtures/webhook_issues_opened.json");
        let r = IssuesResource;
        let repo = Repo {
            owner: "x".into(),
            name: "y".into(),
        };
        let err = r
            .parse_webhook("issues", "d", "s", body, &repo)
            .expect_err("must reject");
        assert!(matches!(err, GhError::Malformed(_)));
    }

    #[test]
    fn webhook_unknown_event_returns_empty() {
        let r = IssuesResource;
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let events = r
            .parse_webhook("ping", "d", "s", b"{}", &repo)
            .expect("ping returns empty");
        assert!(events.is_empty());
    }

    /// Fix 3 (round-3): two issue payloads with the same `updated_at` but
    /// different `body` must produce different event IDs (payload hash breaks the tie).
    #[test]
    fn issues_event_id_differs_on_same_second_payload_change() {
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let make_dto = |body_text: &str| -> IssueDto {
            serde_json::from_value(serde_json::json!({
                "id": 42,
                "number": 42,
                "title": "Same timestamp, different body",
                "body": body_text,
                "state": "open",
                "user": {"login": "alice"},
                "created_at": "2026-05-25T10:00:00Z",
                // Same second — triggers the collision without Fix 3.
                "updated_at": "2026-05-25T10:00:00Z",
                "html_url": "https://github.com/o/r/issues/42"
            }))
            .expect("deserializes")
        };
        let dto_a = make_dto("First edit within the same second");
        let dto_b = make_dto("Second edit within the same second");

        let ev_a = issue_to_event(&dto_a, &repo, None);
        let ev_b = issue_to_event(&dto_b, &repo, None);

        assert_ne!(
            ev_a.event_id.as_str(),
            ev_b.event_id.as_str(),
            "different payload content must produce different event IDs even at the same timestamp"
        );
    }

    /// Fix 2 (round-3): when exactly `N*per_page` issues exist, the last page is
    /// empty.  The preceding full pages must accumulate `pending_since` so that
    /// when the empty page (exhaustion) arrives, `since` is correctly advanced.
    ///
    /// Two sequential polls: first returns 50 items (full, page advances),
    /// second returns 0 (empty, exhaustion).  After the second poll `since`
    /// must equal the max `updated_at` seen across both pages.
    #[tokio::test]
    async fn issues_poll_advances_since_after_exact_full_page_then_empty() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let ts = "2026-05-25T10:00:00Z";
        let full_page: Vec<serde_json::Value> = (0_u64..50)
            .map(|i| {
                serde_json::json!({
                    "id": i, "number": i,
                    "title": format!("issue {i}"),
                    "body": null, "state": "open",
                    "user": {"login": "alice"},
                    "created_at": ts,
                    "updated_at": ts,
                    "html_url": format!("https://github.com/o/r/issues/{i}"),
                    "labels": []
                })
            })
            .collect();

        // Page 1: full (50 items).
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&full_page))
            .mount(&server)
            .await;

        // Page 2: empty.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let handle = CredentialHandle::from_bytes(
            serde_json::json!({"kind": "pat", "token": "t"})
                .to_string()
                .into_bytes(),
        );
        let base = url::Url::parse(&server.uri()).unwrap();

        let initial = ResourceCursor {
            page: Some(1),
            ..Default::default()
        };

        // First poll: full page → pending_since set, page advances to 2.
        let outcome1 = IssuesResource
            .poll(
                &{
                    let auth = std::sync::Arc::new(
                        crate::auth::GitHubAuth::from_handle(&handle).expect("auth"),
                    );
                    GhClient::new(auth, base.clone())
                },
                &Repo {
                    owner: "o".into(),
                    name: "r".into(),
                },
                &initial,
                50,
            )
            .await
            .expect("poll 1");
        assert_eq!(outcome1.events.len(), 50);
        assert_eq!(outcome1.next_cursor.page, Some(2), "page advances");
        assert!(
            outcome1.next_cursor.pending_since.is_some(),
            "pending_since must be set after full page"
        );
        assert_eq!(
            outcome1.next_cursor.since, initial.since,
            "since must not advance on full page"
        );

        // Second poll (empty page → exhaustion): since must advance.
        let outcome2 = IssuesResource
            .poll(
                &{
                    let auth = std::sync::Arc::new(
                        crate::auth::GitHubAuth::from_handle(&handle).expect("auth"),
                    );
                    GhClient::new(auth, base.clone())
                },
                &Repo {
                    owner: "o".into(),
                    name: "r".into(),
                },
                &outcome1.next_cursor,
                50,
            )
            .await
            .expect("poll 2");
        assert_eq!(outcome2.events.len(), 0);
        assert!(
            outcome2.next_cursor.since.is_some(),
            "since must be set after exhaustion"
        );
        assert!(
            outcome2.next_cursor.pending_since.is_none(),
            "pending_since must be cleared after exhaustion"
        );
        assert_eq!(outcome2.next_cursor.page, Some(1), "page resets to 1");
    }

    /// Fix 3 (round-2, confirmed): `IssueDto` entries carrying a `pull_request`
    /// marker field must be skipped.
    #[test]
    fn pull_request_field_triggers_skip() {
        // Minimal IssueDto with pull_request set — should be skipped.
        let pr_dto: IssueDto = serde_json::from_value(serde_json::json!({
            "id": 99,
            "number": 99,
            "title": "PR disguised as issue",
            "body": null,
            "state": "open",
            "user": {"login": "alice"},
            "created_at": "2026-05-20T10:00:00Z",
            "updated_at": "2026-05-20T10:00:00Z",
            "html_url": "https://github.com/o/r/pull/99",
            "pull_request": {"url": "https://api.github.com/repos/o/r/pulls/99"}
        }))
        .expect("deserializes");
        assert!(
            pr_dto.pull_request.is_some(),
            "pull_request field must be Some"
        );

        // Minimal IssueDto without pull_request — should be emitted.
        let issue_dto: IssueDto = serde_json::from_value(serde_json::json!({
            "id": 10,
            "number": 10,
            "title": "Real issue",
            "body": null,
            "state": "open",
            "user": {"login": "bob"},
            "created_at": "2026-05-21T10:00:00Z",
            "updated_at": "2026-05-21T10:00:00Z",
            "html_url": "https://github.com/o/r/issues/10"
        }))
        .expect("deserializes");
        assert!(
            issue_dto.pull_request.is_none(),
            "real issue must have no pull_request"
        );

        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        // The PR DTO must be skipped (no event produced).
        // The issue DTO must yield an event with kind:issue.
        let issue_event = issue_to_event(&issue_dto, &repo, None);
        assert!(issue_event.labels.contains("kind:issue"));
    }

    /// Fix 1 (round-4): poll and webhook paths must produce the same `event_id`
    /// for the same upstream issue state (identical payload + timestamp).
    #[test]
    fn poll_and_webhook_paths_produce_same_event_id_for_same_issue() {
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let dto: IssueDto = serde_json::from_value(serde_json::json!({
            "id": 42,
            "number": 42,
            "title": "Cross-channel dedup",
            "body": "same content",
            "state": "open",
            "user": {"login": "alice"},
            "created_at": "2026-05-25T10:00:00Z",
            "updated_at": "2026-05-25T10:00:00Z",
            "html_url": "https://github.com/o/r/issues/42"
        }))
        .expect("deserializes");

        // Poll path: no webhook_meta.
        let poll_event = issue_to_event(&dto, &repo, None);
        // Webhook path: webhook_meta present (delivery_id differs).
        let webhook_event =
            issue_to_event(&dto, &repo, Some(("delivery-uuid-xyz", "sig-1", "edited")));

        assert_eq!(
            poll_event.event_id.as_str(),
            webhook_event.event_id.as_str(),
            "poll and webhook must produce the same event_id for identical issue content"
        );
        // DeliveryMode must differ (poll vs webhook).
        assert!(matches!(poll_event.delivery, DeliveryMode::Poll { .. }));
        assert!(matches!(
            webhook_event.delivery,
            DeliveryMode::Webhook { .. }
        ));
    }

    /// Fix 3 (round-4): `since` advances to `max_updated - 1s` on exhaustion.
    #[tokio::test]
    async fn issues_advance_since_overlaps_by_one_second() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let ts = "2026-05-25T12:00:00Z";

        // One item, partial page → exhaustion on first poll.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 1, "number": 1,
                    "title": "issue",
                    "body": null, "state": "open",
                    "user": {"login": "alice"},
                    "created_at": ts,
                    "updated_at": ts,
                    "html_url": "https://github.com/o/r/issues/1"
                }
            ])))
            .mount(&server)
            .await;

        let handle = CredentialHandle::from_bytes(
            serde_json::json!({"kind": "pat", "token": "t"})
                .to_string()
                .into_bytes(),
        );
        let auth =
            std::sync::Arc::new(crate::auth::GitHubAuth::from_handle(&handle).expect("auth"));
        let client = GhClient::new(auth, url::Url::parse(&server.uri()).unwrap());
        let outcome = IssuesResource
            .poll(
                &client,
                &Repo {
                    owner: "o".into(),
                    name: "r".into(),
                },
                &ResourceCursor {
                    page: Some(1),
                    ..Default::default()
                },
                50,
            )
            .await
            .expect("poll");

        let expected_max = DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc);
        let expected_since = expected_max - Duration::seconds(1);

        assert_eq!(
            outcome.next_cursor.since,
            Some(expected_since),
            "since must be max_updated - 1s"
        );
    }

    /// Fix 2 (round-5): in steady state (no new issues), the boundary issue must
    /// NOT be re-emitted on the second poll.  The 1-second cursor overlap re-serves
    /// it, but `pending_boundary_event_ids` causes it to be skipped.
    #[tokio::test]
    async fn issues_steady_state_no_new_updates_emits_nothing() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let ts = "2026-05-25T12:00:00Z";

        let issue_payload = serde_json::json!([{
            "id": 1, "number": 1,
            "title": "Steady-state issue",
            "body": null, "state": "open",
            "user": {"login": "alice"},
            "created_at": ts,
            "updated_at": ts,
            "html_url": "https://github.com/o/r/issues/1"
        }]);

        // Both polls return the identical issue.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&issue_payload))
            .mount(&server)
            .await;

        let handle = CredentialHandle::from_bytes(
            serde_json::json!({"kind": "pat", "token": "t"})
                .to_string()
                .into_bytes(),
        );
        let auth =
            std::sync::Arc::new(crate::auth::GitHubAuth::from_handle(&handle).expect("auth"));
        let base = url::Url::parse(&server.uri()).unwrap();
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };

        // Poll 1: emits 1 event (the issue), sets boundary_event_ids.
        let result1 = IssuesResource
            .poll(
                &GhClient::new(auth.clone(), base.clone()),
                &repo,
                &ResourceCursor {
                    page: Some(1),
                    ..Default::default()
                },
                50,
            )
            .await
            .expect("poll 1");
        assert_eq!(result1.events.len(), 1, "first poll emits 1 event");
        assert!(
            !result1.next_cursor.pending_boundary_event_ids.is_empty(),
            "boundary event IDs must be recorded after first poll"
        );

        // Poll 2: same server, same response — boundary skip must suppress re-emit.
        let result2 = IssuesResource
            .poll(
                &GhClient::new(auth.clone(), base.clone()),
                &repo,
                &result1.next_cursor,
                50,
            )
            .await
            .expect("poll 2");
        assert_eq!(
            result2.events.len(),
            0,
            "second poll in steady state must emit nothing"
        );
    }

    /// Fix 3 (round-6): when a page contains only PR rows (`pull_request: {...}`),
    /// no events are emitted but the cursor must still advance past those rows so
    /// the next poll does not replay the same window.
    #[tokio::test]
    async fn issues_cursor_advances_through_pr_only_page() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let ts = "2026-05-25T10:00:00Z";

        // All three items have `pull_request` markers — none should be emitted.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 11, "number": 11,
                    "title": "PR disguised as issue 1",
                    "body": null, "state": "open",
                    "user": {"login": "alice"},
                    "created_at": ts,
                    "updated_at": ts,
                    "html_url": "https://github.com/o/r/pull/11",
                    "pull_request": {"url": "https://api.github.com/repos/o/r/pulls/11"}
                },
                {
                    "id": 12, "number": 12,
                    "title": "PR disguised as issue 2",
                    "body": null, "state": "open",
                    "user": {"login": "bob"},
                    "created_at": ts,
                    "updated_at": ts,
                    "html_url": "https://github.com/o/r/pull/12",
                    "pull_request": {"url": "https://api.github.com/repos/o/r/pulls/12"}
                },
                {
                    "id": 13, "number": 13,
                    "title": "PR disguised as issue 3",
                    "body": null, "state": "open",
                    "user": {"login": "carol"},
                    "created_at": ts,
                    "updated_at": ts,
                    "html_url": "https://github.com/o/r/pull/13",
                    "pull_request": {"url": "https://api.github.com/repos/o/r/pulls/13"}
                }
            ])))
            .mount(&server)
            .await;

        let handle = CredentialHandle::from_bytes(
            serde_json::json!({"kind": "pat", "token": "t"})
                .to_string()
                .into_bytes(),
        );
        let auth =
            std::sync::Arc::new(crate::auth::GitHubAuth::from_handle(&handle).expect("auth"));
        let client = GhClient::new(auth, url::Url::parse(&server.uri()).unwrap());

        let result = IssuesResource
            .poll(
                &client,
                &Repo {
                    owner: "o".into(),
                    name: "r".into(),
                },
                &ResourceCursor {
                    page: Some(1),
                    ..Default::default()
                },
                50,
            )
            .await
            .expect("poll");

        // No events emitted (all rows are PRs).
        assert_eq!(result.events.len(), 0, "PR-only page must emit no events");

        // But the cursor must still advance so the next poll does not stall.
        // Expected: since = max_updated_at - 1s = ts - 1s.
        let expected_max = chrono::DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc);
        let expected_since = expected_max - Duration::seconds(1);
        assert_eq!(
            result.next_cursor.since,
            Some(expected_since),
            "cursor must advance to max_updated - 1s even when all rows are PRs"
        );
    }

    /// Fix 2 (round-8): steady-state — when three consecutive polls all return the
    /// same boundary issue (no new updates), the skip set must persist across ALL
    /// three polls so none of them re-emit.
    #[tokio::test]
    async fn issues_three_polls_no_new_updates_emits_each_issue_once() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let ts = "2026-05-25T12:00:00Z";

        // All three polls return the identical boundary issue.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": 1, "number": 1,
                    "title": "Steady-state issue",
                    "body": null, "state": "open",
                    "user": {"login": "alice"},
                    "created_at": ts,
                    "updated_at": ts,
                    "html_url": "https://github.com/o/r/issues/1"
                }])),
            )
            .mount(&server)
            .await;

        let handle = CredentialHandle::from_bytes(
            serde_json::json!({"kind": "pat", "token": "t"})
                .to_string()
                .into_bytes(),
        );
        let auth =
            std::sync::Arc::new(crate::auth::GitHubAuth::from_handle(&handle).expect("auth"));
        let base = url::Url::parse(&server.uri()).unwrap();
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };

        // Poll 1: emits 1 event (the issue), sets boundary_event_ids.
        let r1 = IssuesResource
            .poll(
                &GhClient::new(auth.clone(), base.clone()),
                &repo,
                &ResourceCursor {
                    page: Some(1),
                    ..Default::default()
                },
                50,
            )
            .await
            .expect("poll 1");
        assert_eq!(r1.events.len(), 1, "first poll emits 1 event");
        assert!(
            !r1.next_cursor.pending_boundary_event_ids.is_empty(),
            "boundary IDs must be recorded after poll 1"
        );

        // Poll 2: steady state — boundary skip must suppress re-emit.
        let r2 = IssuesResource
            .poll(
                &GhClient::new(auth.clone(), base.clone()),
                &repo,
                &r1.next_cursor,
                50,
            )
            .await
            .expect("poll 2");
        assert_eq!(
            r2.events.len(),
            0,
            "poll 2 must skip already-emitted boundary issue"
        );
        assert!(
            !r2.next_cursor.pending_boundary_event_ids.is_empty(),
            "boundary IDs must still be in cursor after poll 2 (Fix 2, round-8)"
        );

        // Poll 3: steady state continues — boundary IDs must STILL be present.
        let r3 = IssuesResource
            .poll(
                &GhClient::new(auth.clone(), base.clone()),
                &repo,
                &r2.next_cursor,
                50,
            )
            .await
            .expect("poll 3");
        assert_eq!(
            r3.events.len(),
            0,
            "poll 3 must continue skipping (Fix 2, round-8)"
        );
    }

    /// Fix 2 (round-5): a new issue appearing after the boundary is emitted while
    /// the unchanged boundary issue is skipped.
    #[tokio::test]
    async fn issues_new_update_after_boundary_emits_only_new() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let ts_t = "2026-05-25T12:00:00Z";
        let ts_t5 = "2026-05-25T12:05:00Z";

        // Server 1: first poll — only the boundary issue at T.
        let server1 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 1, "number": 1,
                    "title": "Boundary issue",
                    "body": null, "state": "open",
                    "user": {"login": "alice"},
                    "created_at": ts_t,
                    "updated_at": ts_t,
                    "html_url": "https://github.com/o/r/issues/1"
                }
            ])))
            .mount(&server1)
            .await;

        let handle = CredentialHandle::from_bytes(
            serde_json::json!({"kind": "pat", "token": "t"})
                .to_string()
                .into_bytes(),
        );
        let auth =
            std::sync::Arc::new(crate::auth::GitHubAuth::from_handle(&handle).expect("auth"));
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };

        let result1 = IssuesResource
            .poll(
                &GhClient::new(auth.clone(), url::Url::parse(&server1.uri()).unwrap()),
                &repo,
                &ResourceCursor {
                    page: Some(1),
                    ..Default::default()
                },
                50,
            )
            .await
            .expect("poll 1");
        assert_eq!(result1.events.len(), 1, "first poll emits 1 event");
        assert!(!result1.next_cursor.pending_boundary_event_ids.is_empty());

        // Server 2: second poll — same boundary issue at T plus a new one at T+5min.
        let server2 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 1, "number": 1,
                    "title": "Boundary issue",
                    "body": null, "state": "open",
                    "user": {"login": "alice"},
                    "created_at": ts_t,
                    "updated_at": ts_t,
                    "html_url": "https://github.com/o/r/issues/1"
                },
                {
                    "id": 2, "number": 2,
                    "title": "New issue",
                    "body": null, "state": "open",
                    "user": {"login": "bob"},
                    "created_at": ts_t5,
                    "updated_at": ts_t5,
                    "html_url": "https://github.com/o/r/issues/2"
                }
            ])))
            .mount(&server2)
            .await;

        let result2 = IssuesResource
            .poll(
                &GhClient::new(auth.clone(), url::Url::parse(&server2.uri()).unwrap()),
                &repo,
                &result1.next_cursor,
                50,
            )
            .await
            .expect("poll 2");

        // Only the new issue (id=2) must be emitted; boundary issue (id=1) skipped.
        assert_eq!(
            result2.events.len(),
            1,
            "only the new issue must be emitted"
        );
        let emitted_id = match &result2.events[0].payload {
            cairn_connectors_core::ConnectorPayload::Json { body, .. } => body
                .get("number")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            _ => 0,
        };
        assert_eq!(emitted_id, 2, "emitted event must be the new issue #2");
    }
}
