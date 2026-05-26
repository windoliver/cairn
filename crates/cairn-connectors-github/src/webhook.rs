//! `X-GitHub-Event` → `GhResource::parse_webhook` dispatcher.
//!
//! Substrate has already verified the HMAC signature and resolved the
//! `signature_id` + `delivery_id` before this dispatcher runs.

use cairn_connectors_core::{ConnectorEvent, WebhookRequest};

use crate::error::GhError;
use crate::resources::{
    GhResource, Repo, commits::CommitsResource, issues::IssuesResource, prs::PrsResource,
};

/// Dispatch a verified webhook to the right resource. Returns an empty Vec
/// for `ping`, `installation`, `issue_comment`, and unknown events — never errors on them.
pub(crate) fn dispatch(
    req: &WebhookRequest,
    signature_id: &str,
    repo: &Repo,
) -> Result<Vec<ConnectorEvent>, GhError> {
    let event_type = req
        .header("X-GitHub-Event")
        .ok_or_else(|| GhError::Malformed("missing X-GitHub-Event".into()))?;
    let delivery_id = req
        .header("X-GitHub-Delivery")
        .ok_or_else(|| GhError::Malformed("missing X-GitHub-Delivery".into()))?;

    match event_type {
        "issues" => {
            IssuesResource.parse_webhook(event_type, delivery_id, signature_id, &req.body, repo)
        }
        "pull_request" | "pull_request_review" | "pull_request_review_comment" => {
            PrsResource.parse_webhook(event_type, delivery_id, signature_id, &req.body, repo)
        }
        "push" => {
            CommitsResource.parse_webhook(event_type, delivery_id, signature_id, &req.body, repo)
        }
        // issue_comment / ping / installation: known but deliberately not ingested in
        // this slice (spec §1.2 out of scope). Returning empty Ok keeps GitHub's
        // delivery acks clean; operators MUST NOT subscribe these events.
        "issue_comment" | "ping" | "installation" | "installation_repositories" => Ok(vec![]),
        other => {
            tracing::debug!(event_type = other, "github: ignoring unhandled event type");
            Ok(vec![])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_req(event: &str, delivery: &str, body: &[u8]) -> WebhookRequest {
        WebhookRequest {
            connector: "github".into(),
            body: body.to_vec(),
            headers: vec![
                ("X-GitHub-Event".into(), event.into()),
                ("X-GitHub-Delivery".into(), delivery.into()),
            ],
        }
    }

    #[test]
    fn ping_returns_empty_no_body_parse() {
        let req = build_req("ping", "d-1", b"{\"zen\":\"hi\"}");
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let events = dispatch(&req, "sigid", &repo).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn missing_event_header_is_malformed() {
        let req = WebhookRequest {
            connector: "github".into(),
            body: b"{}".to_vec(),
            headers: vec![("X-GitHub-Delivery".into(), "d".into())],
        };
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        assert!(matches!(
            dispatch(&req, "s", &repo),
            Err(GhError::Malformed(_))
        ));
    }

    #[test]
    fn issues_event_routes_to_issues_resource() {
        let body = include_bytes!("../tests/fixtures/webhook_issues_opened.json");
        let req = build_req("issues", "d-abc", body);
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let events = dispatch(&req, "sigid-1", &repo).expect("dispatch");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source_ref.kind, "issue");
    }

    #[test]
    fn push_event_routes_to_commits_resource() {
        let body = include_bytes!("../tests/fixtures/webhook_push.json");
        let req = build_req("push", "d-push-1", body);
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let events = dispatch(&req, "sigid", &repo).expect("dispatch");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source_ref.kind, "commit");
    }

    #[test]
    fn issue_comment_is_explicitly_ignored_not_routed_to_issues() {
        // Round-7 fix: issue_comment is advertised as "known but out of scope"
        // per spec §1.2. The dispatcher returns Ok(vec![]) immediately rather
        // than routing to IssuesResource (which previously had its own empty arm).
        let req = build_req("issue_comment", "d-1", b"{}");
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let events = dispatch(&req, "sigid", &repo).expect("issue_comment is ignored");
        assert!(events.is_empty());
    }
}
