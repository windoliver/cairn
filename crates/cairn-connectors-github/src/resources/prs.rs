//! Pull-request resource.

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
        let mut max_updated = sub_cursor.since;
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
        // Fix 2 (mirrored from IssuesResource): only advance `since` when the
        // current window is exhausted.  Keeping `since` stable while paginating
        // a full window prevents the result set from shifting mid-pagination.
        let next_cursor = if exhausted {
            // Window done: advance since to max-seen, reset page for next cycle.
            ResourceCursor {
                since: max_updated.or(sub_cursor.since),
                page: Some(1),
                ..ResourceCursor::default()
            }
        } else {
            // Still paginating same since window — keep since stable.
            ResourceCursor {
                since: sub_cursor.since,
                page: Some(page + 1),
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
    // Deterministic event ID: updated_at timestamp as revision marker.
    let event_id = crate::event_id::deterministic(
        "pr",
        &source_ref.system_id,
        &dto.updated_at.timestamp().to_string(),
    );
    let mut labels: BTreeSet<String> = BTreeSet::new();
    labels.insert("source:github".into());
    labels.insert("kind:pr".into());

    let payload = ConnectorPayload::Json {
        mime: "application/json".into(),
        body: serde_json::json!({
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
        }),
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

    /// Fix 2: verify the real list-endpoint shape (`merged_at`, no merged field)
    /// deserializes correctly and that `merged_at` drives the "merged" boolean
    /// in the emitted event payload.
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
}
