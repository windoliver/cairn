//! Pull-request resource.

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

pub(crate) struct PrsResource;

#[derive(Debug, Deserialize)]
struct PrDto {
    id: u64,
    number: u64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    state: String,
    user: UserDto,
    updated_at: DateTime<Utc>,
    html_url: String,
    head: RefDto,
    base: RefDto,
    /// List endpoint returns `merged_at: Option<DateTime>`. Single-PR endpoint
    /// also exposes a `merged: bool` — we ignore that and derive from
    /// `merged_at` so the same DTO works for both list and single endpoints.
    #[serde(default)]
    merged_at: Option<DateTime<Utc>>,
    #[serde(default)]
    draft: bool,
}

#[derive(Debug, Deserialize)]
struct UserDto {
    login: String,
}

#[derive(Debug, Deserialize)]
struct RefDto {
    sha: String,
    #[serde(rename = "ref")]
    ref_name: String,
}

#[derive(Debug, Deserialize)]
struct WebhookPrEnvelope {
    action: String,
    pull_request: PrDto,
    repository: RepositoryDto,
}

#[derive(Debug, Deserialize)]
struct RepositoryDto {
    full_name: String,
}

#[async_trait]
impl GhResource for PrsResource {
    fn kind(&self) -> &'static str {
        "pr"
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
        let query: Vec<(&str, String)> = vec![
            ("state", "all".into()),
            ("sort", "updated".into()),
            // Newest-first ordering: breaks early as soon as a row older than
            // `since` is encountered, avoiding a full re-walk on every poll.
            ("direction", "desc".into()),
            ("per_page", per_page.to_string()),
            ("page", page.to_string()),
        ];

        let path = format!("/repos/{}/{}/pulls", repo.owner, repo.name);
        let prs: Vec<PrDto> = client.get_json(&path, &query).await?;

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
            Vec::with_capacity(prs.len());
        // Carry forward any in-progress max seen from prior pages; initialize
        // from `pending_since` so exact-full-page windows accumulate correctly
        // across multiple poll calls before exhaustion.
        let mut max_updated = sub_cursor.pending_since.or(sub_cursor.since);
        // Track whether we hit the stale break (updated_at <= since) — even on a
        // full page this means all remaining pages are older, so we should treat
        // this as window exhaustion and NOT advance page to N+1 (which would
        // request older rows instead of re-starting from the head on the next poll).
        let mut stale_break_hit = false;
        for dto in &prs {
            // Always advance the high-water timestamp for every returned row so
            // the cursor advances even for rows we won't emit.
            if max_updated.is_none_or(|t| dto.updated_at > t) {
                max_updated = Some(dto.updated_at);
            }
            // With direction=desc (newest first), any row with updated_at <= since
            // means all remaining rows are also old — break immediately rather than
            // continuing to walk stale history.
            if let Some(since) = sub_cursor.since
                && dto.updated_at <= since
            {
                stale_break_hit = true;
                break;
            }
            let event = pr_to_event(dto, repo, None);
            // Skip events already emitted at the boundary timestamp so that the
            // deliberate 1-second cursor overlap doesn't re-spool the same rows
            // in a no-update steady state.
            if boundary_skip.contains(event.event_id.as_str()) {
                continue;
            }
            events_with_meta.push((event, dto.updated_at));
        }

        let events: Vec<ConnectorEvent> = events_with_meta.iter().map(|(e, _)| e.clone()).collect();

        let raw_exhausted = u32::try_from(prs.len()).unwrap_or(u32::MAX) < per_page;
        // A stale break means all subsequent pages are also stale — treat it as
        // window exhaustion so the cursor commits the high-water and resets to
        // page=1 instead of advancing to page N+1 (which would walk older history
        // and miss fresh updates at the head on the next poll).
        let exhausted = stale_break_hit || raw_exhausted;
        // Only advance `since` when the current window is exhausted (partial
        // page).  Keeping `since` stable while paginating a full window prevents
        // the result set from shifting mid-pagination.  `pending_since` carries
        // the in-progress high-water across polls so exact-full-page counts
        // (50, 100, …) commit the correct max on the exhaustion page.
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
            "pull_request" => {
                let env: WebhookPrEnvelope = serde_json::from_slice(body)?;
                let expected = repo.scope_value();
                if env.repository.full_name != expected {
                    return Err(GhError::Malformed(format!(
                        "webhook repo {} != configured {expected}",
                        env.repository.full_name
                    )));
                }
                Ok(vec![pr_to_event(
                    &env.pull_request,
                    repo,
                    Some((delivery_id, signature_id, &env.action)),
                )])
            }
            // pull_request_review and pull_request_review_comment are out of scope for this slice.
            _ => Ok(vec![]),
        }
    }
}

fn pr_to_event(
    dto: &PrDto,
    repo: &Repo,
    webhook_meta: Option<(&str, &str, &str)>,
) -> ConnectorEvent {
    let source_ref = SourceRef::new(
        "pr",
        format!("gh:{}/{}#{}", repo.owner, repo.name, dto.number),
        None,
    );
    let mut labels: BTreeSet<String> = BTreeSet::new();
    labels.insert("source:github".into());
    labels.insert("kind:pr".into());

    let payload_value = serde_json::json!({
        "id": dto.id,
        "number": dto.number,
        "title": dto.title,
        "body": dto.body,
        "state": dto.state,
        "user": dto.user.login,
        "html_url": dto.html_url,
        "head": {"sha": dto.head.sha, "ref": dto.head.ref_name},
        "base": {"sha": dto.base.sha, "ref": dto.base.ref_name},
        // Preserve boolean semantics for downstream consumers while using
        // merged_at (present on list endpoint) rather than merged (list-absent).
        "merged": dto.merged_at.is_some(),
        "draft": dto.draft,
    });
    // Deterministic event ID: always keyed on upstream-object identity so that
    // the same PR update is deduplicated regardless of whether it arrives via
    // poll or webhook.  `delivery_id` is NOT used here — it changes per-delivery
    // and would break cross-channel dedup.  Instead, timestamp + payload content
    // hash captures both "which version" and "which edit within 1s" invariants.
    // The `DeliveryMode::Webhook { signature_id }` field separately encodes the
    // per-delivery UUID for the substrate's webhook replay guard.
    let ts = dto.updated_at.timestamp().to_string();
    let payload_rev = crate::event_id::payload_revision(&payload_value);
    let event_id = crate::event_id::from_parts("pr", &source_ref.system_id, &[&ts, &payload_rev]);

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
    fn webhook_parse_extracts_pr_event() {
        let body = include_bytes!("../../tests/fixtures/webhook_pull_request_opened.json");
        let r = PrsResource;
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let events = r
            .parse_webhook("pull_request", "d", "s", body, &repo)
            .expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source_ref.kind, "pr");
        assert!(events[0].labels.contains("kind:pr"));
    }

    #[test]
    fn webhook_parse_rejects_mismatched_repo() {
        let body = include_bytes!("../../tests/fixtures/webhook_pull_request_opened.json");
        let r = PrsResource;
        let repo = Repo {
            owner: "x".into(),
            name: "y".into(),
        };
        let err = r
            .parse_webhook("pull_request", "d", "s", body, &repo)
            .expect_err("must reject");
        assert!(matches!(err, GhError::Malformed(_)));
    }

    #[test]
    fn webhook_review_returns_empty() {
        let r = PrsResource;
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let events = r
            .parse_webhook("pull_request_review", "d", "s", b"{}", &repo)
            .expect("review returns empty");
        assert!(events.is_empty());
    }

    #[test]
    fn webhook_review_comment_returns_empty() {
        let r = PrsResource;
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let events = r
            .parse_webhook("pull_request_review_comment", "d", "s", b"{}", &repo)
            .expect("review_comment returns empty");
        assert!(events.is_empty());
    }

    #[test]
    fn webhook_unknown_event_returns_empty() {
        let r = PrsResource;
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let events = r
            .parse_webhook("ping", "d", "s", b"{}", &repo)
            .expect("ping returns empty");
        assert!(events.is_empty());
    }

    /// Fix 3 (round-3): two PR payloads with the same `updated_at` but different
    /// `title` must produce different event IDs (payload hash breaks the tie).
    #[test]
    fn prs_event_id_differs_on_same_second_payload_change() {
        let make_dto = |title: &str| -> PrDto {
            serde_json::from_value(serde_json::json!({
                "id": 7,
                "number": 7,
                "title": title,
                "body": null,
                "state": "open",
                "user": {"login": "alice"},
                "updated_at": "2026-05-25T10:00:00Z",
                "html_url": "https://github.com/o/r/pull/7",
                "head": {"sha": "abc", "ref": "feat"},
                "base": {"sha": "def", "ref": "main"},
                "draft": false
            }))
            .expect("deserializes")
        };
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let ev_a = pr_to_event(&make_dto("Title v1"), &repo, None);
        let ev_b = pr_to_event(&make_dto("Title v2"), &repo, None);

        assert_ne!(
            ev_a.event_id.as_str(),
            ev_b.event_id.as_str(),
            "different payload content must produce different event IDs even at the same timestamp"
        );
    }

    /// Fix 2 (round-3): exact-full-page PR pagination must advance `since`
    /// correctly.  Same invariant as `issues_poll_advances_since_after_exact_full_page_then_empty`.
    #[tokio::test]
    async fn prs_poll_advances_since_after_exact_full_page_then_empty() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let ts = "2026-05-25T10:00:00Z";
        let full_page: Vec<serde_json::Value> = (0_u64..50)
            .map(|i| {
                serde_json::json!({
                    "id": i, "number": i,
                    "title": format!("PR {i}"),
                    "body": null, "state": "open",
                    "user": {"login": "alice"},
                    "updated_at": ts,
                    "html_url": format!("https://github.com/o/r/pull/{i}"),
                    "head": {"sha": "abc", "ref": "feat"},
                    "base": {"sha": "def", "ref": "main"},
                    "draft": false
                })
            })
            .collect();

        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&full_page))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
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

        let outcome1 = PrsResource
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
        // All 50 have updated_at == ts; since is None initially so none filtered.
        assert_eq!(outcome1.events.len(), 50);
        assert_eq!(outcome1.next_cursor.page, Some(2));
        assert!(
            outcome1.next_cursor.pending_since.is_some(),
            "pending_since set after full page"
        );
        assert_eq!(outcome1.next_cursor.since, initial.since, "since stable");

        let outcome2 = PrsResource
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
        assert!(outcome2.next_cursor.since.is_some(), "since advances");
        assert!(
            outcome2.next_cursor.pending_since.is_none(),
            "pending_since cleared"
        );
        assert_eq!(outcome2.next_cursor.page, Some(1), "page resets");
    }

    /// Fix 2 (round-2, confirmed): verify the real list-endpoint shape.
    #[test]
    fn list_endpoint_shape_open_and_merged_prs() {
        let body = include_bytes!("../../tests/fixtures/prs_list_real_shape.json");
        let prs: Vec<PrDto> = serde_json::from_slice(body).expect("deserializes");
        assert_eq!(prs.len(), 2);

        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };

        let open_event = pr_to_event(&prs[0], &repo, None);
        let merged_event = pr_to_event(&prs[1], &repo, None);

        let open_merged = match &open_event.payload {
            cairn_connectors_core::ConnectorPayload::Json { body, .. } => body
                .get("merged")
                .and_then(serde_json::Value::as_bool)
                .expect("merged field"),
            _ => panic!("expected Json payload"),
        };
        assert!(!open_merged, "open PR must have merged=false");

        let closed_merged = match &merged_event.payload {
            cairn_connectors_core::ConnectorPayload::Json { body, .. } => body
                .get("merged")
                .and_then(serde_json::Value::as_bool)
                .expect("merged field"),
            _ => panic!("expected Json payload"),
        };
        assert!(closed_merged, "merged PR must have merged=true");
    }

    /// Fix 1 (round-4): poll and webhook paths must produce the same `event_id`
    /// for the same upstream PR state (identical payload + timestamp).
    #[test]
    fn poll_and_webhook_paths_produce_same_event_id_for_same_pr() {
        let make_dto = || -> PrDto {
            serde_json::from_value(serde_json::json!({
                "id": 7,
                "number": 7,
                "title": "Cross-channel dedup",
                "body": "same content",
                "state": "open",
                "user": {"login": "alice"},
                "updated_at": "2026-05-25T10:00:00Z",
                "html_url": "https://github.com/o/r/pull/7",
                "head": {"sha": "abc", "ref": "feat"},
                "base": {"sha": "def", "ref": "main"},
                "draft": false
            }))
            .expect("deserializes")
        };
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let poll_event = pr_to_event(&make_dto(), &repo, None);
        let webhook_event = pr_to_event(
            &make_dto(),
            &repo,
            Some(("delivery-uuid-xyz", "sig-1", "edited")),
        );

        assert_eq!(
            poll_event.event_id.as_str(),
            webhook_event.event_id.as_str(),
            "poll and webhook must produce the same event_id for identical PR content"
        );
        assert!(matches!(poll_event.delivery, DeliveryMode::Poll { .. }));
        assert!(matches!(
            webhook_event.delivery,
            DeliveryMode::Webhook { .. }
        ));
    }

    /// Fix 3 (round-4): `since` advances to `max_updated - 1s` on exhaustion.
    #[tokio::test]
    async fn prs_advance_since_overlaps_by_one_second() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let ts = "2026-05-25T12:00:00Z";

        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 1, "number": 1,
                    "title": "PR",
                    "body": null, "state": "open",
                    "user": {"login": "alice"},
                    "updated_at": ts,
                    "html_url": "https://github.com/o/r/pull/1",
                    "head": {"sha": "abc", "ref": "feat"},
                    "base": {"sha": "def", "ref": "main"},
                    "draft": false
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
        let outcome = PrsResource
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

    /// Fix 3 (round-4): a PR updated twice at the same second produces two
    /// distinct event IDs (payload hash breaks the tie), and the second poll
    /// (with `since = max_updated - 1s`) re-serves both so the substrate can
    /// dedupe via `event_id`.
    #[test]
    fn prs_same_second_double_update_both_captured() {
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let make_dto = |title: &str| -> PrDto {
            serde_json::from_value(serde_json::json!({
                "id": 7,
                "number": 7,
                "title": title,
                "body": null,
                "state": "open",
                "user": {"login": "alice"},
                "updated_at": "2026-05-25T10:00:00Z",
                "html_url": "https://github.com/o/r/pull/7",
                "head": {"sha": "abc", "ref": "feat"},
                "base": {"sha": "def", "ref": "main"},
                "draft": false
            }))
            .expect("deserializes")
        };
        let ev1 = pr_to_event(&make_dto("Title v1"), &repo, None);
        let ev2 = pr_to_event(&make_dto("Title v2"), &repo, None);

        // Both events must have distinct IDs (payload hash separates same-second edits).
        assert_ne!(
            ev1.event_id.as_str(),
            ev2.event_id.as_str(),
            "same-second edits must produce distinct event IDs"
        );

        // On re-poll (same payload), the second event's ID is stable — the
        // substrate can dedupe by ID.
        let ev2_repoll = pr_to_event(&make_dto("Title v2"), &repo, None);
        assert_eq!(
            ev2.event_id.as_str(),
            ev2_repoll.event_id.as_str(),
            "deterministic event ID must be stable across polls"
        );
    }

    /// Fix 2 (round-6): poll uses direction=desc and breaks as soon as a row with
    /// `updated_at <= since` is encountered, so stale history is never traversed.
    #[tokio::test]
    async fn prs_poll_uses_desc_and_breaks_on_stale_row() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Three PRs returned newest-first (as direction=desc gives).
        // PR_A: 2026-06-01 (newer than since)
        // PR_B: 2026-05-15 (newer than since)
        // PR_C: 2026-05-01 (older than or equal to since=2026-05-10)
        let prs = serde_json::json!([
            {
                "id": 1, "number": 1,
                "title": "PR_A",
                "body": null, "state": "open",
                "user": {"login": "alice"},
                "updated_at": "2026-06-01T00:00:00Z",
                "html_url": "https://github.com/o/r/pull/1",
                "head": {"sha": "aaa", "ref": "feat-a"},
                "base": {"sha": "000", "ref": "main"},
                "draft": false
            },
            {
                "id": 2, "number": 2,
                "title": "PR_B",
                "body": null, "state": "open",
                "user": {"login": "bob"},
                "updated_at": "2026-05-15T00:00:00Z",
                "html_url": "https://github.com/o/r/pull/2",
                "head": {"sha": "bbb", "ref": "feat-b"},
                "base": {"sha": "000", "ref": "main"},
                "draft": false
            },
            {
                "id": 3, "number": 3,
                "title": "PR_C",
                "body": null, "state": "open",
                "user": {"login": "carol"},
                "updated_at": "2026-05-01T00:00:00Z",
                "html_url": "https://github.com/o/r/pull/3",
                "head": {"sha": "ccc", "ref": "feat-c"},
                "base": {"sha": "000", "ref": "main"},
                "draft": false
            }
        ]);

        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .and(query_param("direction", "desc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&prs))
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

        // since = 2026-05-10 → PR_A and PR_B are newer; PR_C (2026-05-01) is older → break.
        let since = chrono::DateTime::parse_from_rfc3339("2026-05-10T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let result = PrsResource
            .poll(
                &client,
                &Repo {
                    owner: "o".into(),
                    name: "r".into(),
                },
                &ResourceCursor {
                    since: Some(since),
                    page: Some(1),
                    ..Default::default()
                },
                50,
            )
            .await
            .expect("poll succeeds");

        // Only PR_A and PR_B must be emitted; PR_C is stale and breaks the loop.
        assert_eq!(
            result.events.len(),
            2,
            "only PR_A and PR_B emitted; loop breaks at PR_C"
        );
        let numbers: Vec<u64> = result
            .events
            .iter()
            .filter_map(|e| {
                if let cairn_connectors_core::ConnectorPayload::Json { body, .. } = &e.payload {
                    body.get("number").and_then(serde_json::Value::as_u64)
                } else {
                    None
                }
            })
            .collect();
        assert!(numbers.contains(&1), "PR_A (#1) must be emitted");
        assert!(numbers.contains(&2), "PR_B (#2) must be emitted");
        assert!(!numbers.contains(&3), "PR_C (#3) must not be emitted");
    }

    /// Fix 1 (round-7): when a full page contains a stale row, the stale break
    /// must trigger window exhaustion so the cursor resets to page=1 rather than
    /// advancing to page=2 (which would walk older history and miss fresh updates).
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Wiremock fixture setup is verbose by nature.
    async fn prs_full_page_with_stale_row_resets_to_page_1() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Five PRs returned newest-first (per_page=5 for simplicity).
        // PR #1 at 2026-06-01 (newer than since=2026-05-15).
        // PRs #2–#5 at 2026-05-01 (older than since=2026-05-15) — stale break fires on PR #2.
        // The page is full (5 items == per_page), so without the stale_break_hit fix
        // raw_exhausted would be false and page would advance to 2.
        let prs = serde_json::json!([
            {
                "id": 1, "number": 1,
                "title": "PR_fresh",
                "body": null, "state": "open",
                "user": {"login": "alice"},
                "updated_at": "2026-06-01T00:00:00Z",
                "html_url": "https://github.com/o/r/pull/1",
                "head": {"sha": "aaa", "ref": "feat-a"},
                "base": {"sha": "000", "ref": "main"},
                "draft": false
            },
            {
                "id": 2, "number": 2,
                "title": "PR_stale_a",
                "body": null, "state": "open",
                "user": {"login": "bob"},
                "updated_at": "2026-05-01T00:00:00Z",
                "html_url": "https://github.com/o/r/pull/2",
                "head": {"sha": "bbb", "ref": "feat-b"},
                "base": {"sha": "000", "ref": "main"},
                "draft": false
            },
            {
                "id": 3, "number": 3,
                "title": "PR_stale_b",
                "body": null, "state": "open",
                "user": {"login": "carol"},
                "updated_at": "2026-05-01T00:00:00Z",
                "html_url": "https://github.com/o/r/pull/3",
                "head": {"sha": "ccc", "ref": "feat-c"},
                "base": {"sha": "000", "ref": "main"},
                "draft": false
            },
            {
                "id": 4, "number": 4,
                "title": "PR_stale_c",
                "body": null, "state": "open",
                "user": {"login": "dave"},
                "updated_at": "2026-05-01T00:00:00Z",
                "html_url": "https://github.com/o/r/pull/4",
                "head": {"sha": "ddd", "ref": "feat-d"},
                "base": {"sha": "000", "ref": "main"},
                "draft": false
            },
            {
                "id": 5, "number": 5,
                "title": "PR_stale_d",
                "body": null, "state": "open",
                "user": {"login": "eve"},
                "updated_at": "2026-05-01T00:00:00Z",
                "html_url": "https://github.com/o/r/pull/5",
                "head": {"sha": "eee", "ref": "feat-e"},
                "base": {"sha": "000", "ref": "main"},
                "draft": false
            }
        ]);

        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&prs))
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

        let since = chrono::DateTime::parse_from_rfc3339("2026-05-15T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let result = PrsResource
            .poll(
                &client,
                &Repo {
                    owner: "o".into(),
                    name: "r".into(),
                },
                &ResourceCursor {
                    since: Some(since),
                    page: Some(1),
                    ..Default::default()
                },
                5, // per_page=5 so 5 PRs == full page
            )
            .await
            .expect("poll");

        // Only PR #1 (fresh) emitted; stale break fires on PR #2.
        assert_eq!(result.events.len(), 1, "only the fresh PR must be emitted");
        let emitted_number = match &result.events[0].payload {
            cairn_connectors_core::ConnectorPayload::Json { body, .. } => body
                .get("number")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            _ => 0,
        };
        assert_eq!(emitted_number, 1, "emitted event must be PR #1");

        // Cursor must reset to page=1, NOT advance to page=2.
        assert_eq!(
            result.next_cursor.page,
            Some(1),
            "stale_break_hit must trigger exhaustion and reset page to 1, not 2"
        );

        // `since` must have advanced past PR #1's timestamp.
        assert!(
            result.next_cursor.since.is_some(),
            "since must be set after stale-break exhaustion"
        );
        let new_since = result.next_cursor.since.unwrap();
        assert!(
            new_since > since,
            "since must advance beyond the original cursor since"
        );
    }

    /// Fix 2 (round-8): steady-state — when three consecutive polls all return the
    /// same boundary PR (no new updates), the skip set must persist across ALL
    /// three polls so none of them re-emit.
    #[tokio::test]
    async fn prs_three_polls_no_new_updates_emits_each_pr_once() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let ts = "2026-05-25T12:00:00Z";

        // All three polls return the identical boundary PR.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": 1, "number": 1,
                    "title": "Steady-state PR",
                    "body": null, "state": "open",
                    "user": {"login": "alice"},
                    "updated_at": ts,
                    "html_url": "https://github.com/o/r/pull/1",
                    "head": {"sha": "abc", "ref": "feat"},
                    "base": {"sha": "def", "ref": "main"},
                    "draft": false
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

        // Poll 1: emits 1 event (the PR), sets boundary_event_ids.
        let r1 = PrsResource
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
        let r2 = PrsResource
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
            "poll 2 must skip already-emitted boundary PR"
        );
        assert!(
            !r2.next_cursor.pending_boundary_event_ids.is_empty(),
            "boundary IDs must still be in cursor after poll 2 (Fix 2, round-8)"
        );

        // Poll 3: steady state continues — boundary IDs must STILL be present.
        let r3 = PrsResource
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

    /// Fix 2 (round-5): in steady state (no new PRs), the boundary PR must NOT
    /// be re-emitted on the second poll.  The 1-second cursor overlap re-serves
    /// it, but `pending_boundary_event_ids` causes it to be skipped.
    #[tokio::test]
    async fn prs_steady_state_no_new_updates_emits_nothing() {
        use cairn_connectors_core::CredentialHandle;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let ts = "2026-05-25T12:00:00Z";

        let pr_payload = serde_json::json!([{
            "id": 1, "number": 1,
            "title": "Steady-state PR",
            "body": null, "state": "open",
            "user": {"login": "alice"},
            "updated_at": ts,
            "html_url": "https://github.com/o/r/pull/1",
            "head": {"sha": "abc", "ref": "feat"},
            "base": {"sha": "def", "ref": "main"},
            "draft": false
        }]);

        // Both polls return the identical PR.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&pr_payload))
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

        // Poll 1: emits 1 event (the PR), sets boundary_event_ids.
        let result1 = PrsResource
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
        let result2 = PrsResource
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
}
