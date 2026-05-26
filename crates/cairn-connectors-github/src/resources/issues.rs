//! Issues + `issue_comment` resource.

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

        let mut events: Vec<ConnectorEvent> = Vec::with_capacity(issues.len());
        let mut max_updated = sub_cursor.since;
        for dto in &issues {
            if max_updated.is_none_or(|t| dto.updated_at > t) {
                max_updated = Some(dto.updated_at);
            }
            // Fix 3: skip pull requests returned by /issues.
            // GitHub's /repos/{o}/{r}/issues endpoint returns both issues and
            // PRs.  PRs carry a non-null `pull_request` marker field.
            // `PrsResource` already owns PRs via /repos/{o}/{r}/pulls — emitting
            // them here too would produce duplicate, misclassified records.
            if dto.pull_request.is_some() {
                continue;
            }
            events.push(issue_to_event(dto, repo, None));
        }

        let exhausted = u32::try_from(issues.len()).unwrap_or(u32::MAX) < per_page;
        // Fix 2: only advance `since` when the current `since`-window is
        // exhausted (partial page).  While paginating through a full window
        // keep `since` stable so that page 2, 3, … are all fetched from the
        // same window.  Advancing `since` mid-window would shift the result
        // set and cause page N of the new window to skip items that were on
        // page N-1 of the old window.
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
            "issue_comment" => {
                // Out of scope for this slice; substrate logs at debug.
                Ok(vec![])
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
    // Deterministic event ID: updated_at timestamp as revision marker.
    let event_id = crate::event_id::deterministic(
        "issue",
        &source_ref.system_id,
        &dto.updated_at.timestamp().to_string(),
    );
    let mut labels: BTreeSet<String> = BTreeSet::new();
    labels.insert("source:github".into());
    labels.insert("kind:issue".into());

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

    /// Fix 3: `IssueDto` entries carrying a `pull_request` marker field must be
    /// skipped in the unit-level deserialization path.  This test verifies that a
    /// DTO with `pull_request: Some(...)` is not converted to an event, and that
    /// the real issue alongside it still is.
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
}
