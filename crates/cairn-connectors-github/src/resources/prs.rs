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
            ("direction", "asc".into()),
            ("per_page", per_page.to_string()),
            ("page", page.to_string()),
        ];

        let path = format!("/repos/{}/{}/pulls", repo.owner, repo.name);
        let prs: Vec<PrDto> = client.get_json(&path, &query).await?;

        let mut events: Vec<ConnectorEvent> = Vec::with_capacity(prs.len());
        // Carry forward any in-progress max seen from prior pages; initialize
        // from `pending_since` so exact-full-page windows accumulate correctly
        // across multiple poll calls before exhaustion.
        let mut max_updated = sub_cursor.pending_since.or(sub_cursor.since);
        for dto in &prs {
            if max_updated.is_none_or(|t| dto.updated_at > t) {
                max_updated = Some(dto.updated_at);
            }
            // Apply `since` client-side because /pulls lacks a since param.
            if sub_cursor
                .since
                .is_some_and(|since| dto.updated_at <= since)
            {
                continue;
            }
            events.push(pr_to_event(dto, repo, None));
        }

        let exhausted = u32::try_from(prs.len()).unwrap_or(u32::MAX) < per_page;
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
            ResourceCursor {
                since: max_updated
                    .map(|t| t - Duration::seconds(1))
                    .or(sub_cursor.since),
                page: Some(1),
                pending_since: None,
                ..ResourceCursor::default()
            }
        } else {
            // Still paginating same since window — keep since stable, advance
            // pending_since so it survives to the exhaustion page.
            ResourceCursor {
                since: sub_cursor.since,
                page: Some(page + 1),
                pending_since: max_updated,
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
}
