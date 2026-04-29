//! Per-row rebac visibility predicate (#46 subset).
//!
//! The full `ReBAC` rule set lives in `cairn-core::rebac` (separate issue).
//! This module implements only the P0 visibility-tier + scope-user/agent
//! match needed to satisfy brief lines 2557/3287/4136 ("non-readable rows
//! never surface; hidden count is reported").

use cairn_core::domain::principal::Principal;
use serde_json::Value;

/// Returns `true` if `principal` may read a row whose `scope` JSON and
/// `taxonomy` JSON are supplied.
///
/// System principals bypass all checks (WAL executor + tests).
///
/// P0 rules:
/// - `"private"` visibility  → owner match: `scope.user` == principal's
///   identity string. `scope.agent` is **not** an authorization grant —
///   it names the lifecycle-owning agent runtime, which can be shared
///   across users, so allowing agent-equality reads would leak rows
///   between tenants on the same agent identity.
/// - `"session"` visibility  → session match: the verified
///   `principal.session_id()` must equal `scope.session_id`. The
///   verb layer must populate this from a trusted session token
///   before forwarding into the store; the store itself trusts the
///   value as authoritative.
/// - `"project"` visibility  → project match: verified
///   `principal.project_id()` must equal `scope.project`.
/// - `"team"`, `"org"` visibility  → fail closed for non-system
///   principals. The current `ScopeTuple` does not carry team or org
///   fields, so the store has no row-side data to authorize against.
///   Adding those dimensions is a brief-level change and must be
///   resolved before these tiers are admitted.
/// - `"public"` → any identified principal may read (public-by-design).
/// - Unknown / missing visibility → deny (fail closed).
#[must_use]
pub fn principal_can_read(principal: &Principal, scope_json: &str, taxonomy_json: &str) -> bool {
    if principal.is_system() {
        return true;
    }
    // Require an identified principal for non-system reads.
    let Some(identity) = principal.identity() else {
        return false;
    };
    let id_str = identity.as_str();

    let scope: Value = serde_json::from_str(scope_json).unwrap_or(Value::Null);
    let taxonomy: Value = serde_json::from_str(taxonomy_json).unwrap_or(Value::Null);

    // Visibility lives in the taxonomy JSON, not the scope JSON.
    // TODO(#46-followup): visibility lives in a synthesized taxonomy
    // JSON document keyed off the writer's convention rather than a
    // dedicated column. Future migration should add `visibility TEXT`
    // with a CHECK constraint, eliminating JSON parse on every rebac
    // check and resolving the schema/domain divergence.
    let visibility = taxonomy
        .get("visibility")
        .and_then(Value::as_str)
        .unwrap_or("private");

    // For every non-public tier, P0 reduces to "row.scope.user matches
    // the calling principal's identity string". `scope.agent` is the
    // lifecycle-owning runtime id and is shared across users (e.g. a
    // single Claude Code instance writing on behalf of many people), so
    // it cannot stand alone as an authorization grant. Tier semantics
    // expand once `Principal` carries verified session and membership
    // context (separate issue).
    match visibility {
        "private" => {
            let scope_user = scope.get("user").and_then(Value::as_str).unwrap_or("");
            !scope_user.is_empty() && id_str == scope_user
        }
        "session" => {
            let scope_session = scope
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let principal_session = principal.session_id().unwrap_or("");
            !scope_session.is_empty() && scope_session == principal_session
        }
        "project" => {
            let scope_project = scope.get("project").and_then(Value::as_str).unwrap_or("");
            let principal_project = principal.project_id().unwrap_or("");
            !scope_project.is_empty() && scope_project == principal_project
        }
        "public" => true,
        // team/org: fail closed until ScopeTuple gains team/org
        // dimensions (brief-level change).
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::domain::{identity::Identity, principal::Principal};

    fn principal_for(id: &str) -> Principal {
        Principal::from_identity(Identity::parse(id).expect("valid identity"))
    }

    #[test]
    fn system_bypasses_all() {
        let p = Principal::system(&cairn_core::wal::test_apply_token());
        assert!(principal_can_read(&p, "{}", r#"{"visibility":"private"}"#));
    }

    #[test]
    fn team_non_owner_denied() {
        let p = principal_for("usr:bob");
        let scope = r#"{"user":"usr:alice"}"#;
        let tax = r#"{"visibility":"team"}"#;
        assert!(!principal_can_read(&p, scope, tax));
    }

    #[test]
    fn team_owner_fails_closed_until_membership_context_exists() {
        // Even the owner is denied: `team` requires verified team
        // membership in `Principal`, which P0 does not carry. Aliasing
        // to owner-match would silently broaden access to non-team
        // members who happen to be the writer.
        let p = principal_for("usr:alice");
        let scope = r#"{"user":"usr:alice"}"#;
        let tax = r#"{"visibility":"team"}"#;
        assert!(!principal_can_read(&p, scope, tax));
    }

    #[test]
    fn private_owner_match() {
        let p = principal_for("usr:alice");
        let scope = r#"{"user":"usr:alice"}"#;
        let tax = r#"{"visibility":"private"}"#;
        assert!(principal_can_read(&p, scope, tax));
    }

    #[test]
    fn private_non_owner_denied() {
        let p = principal_for("usr:bob");
        let scope = r#"{"user":"usr:alice"}"#;
        let tax = r#"{"visibility":"private"}"#;
        assert!(!principal_can_read(&p, scope, tax));
    }

    #[test]
    fn public_visible_to_any_identified() {
        let p = principal_for("usr:bob");
        let scope = r#"{"user":"usr:alice"}"#;
        let tax = r#"{"visibility":"public"}"#;
        assert!(principal_can_read(&p, scope, tax));
    }

    #[test]
    fn unknown_visibility_denied() {
        let p = principal_for("usr:alice");
        assert!(!principal_can_read(&p, "{}", r#"{"visibility":"secret"}"#));
    }

    #[test]
    fn project_visibility_requires_matching_project_context() {
        // Without a project context, the principal cannot read
        // project-scoped rows (even when the row scope's user
        // matches).
        let no_ctx = principal_for("usr:alice");
        let scope = r#"{"user":"usr:alice","project":"proj:foo"}"#;
        let tax = r#"{"visibility":"project"}"#;
        assert!(!principal_can_read(&no_ctx, scope, tax));

        // With matching project context, access is granted.
        let in_proj = principal_for("usr:alice").with_project("proj:foo");
        assert!(principal_can_read(&in_proj, scope, tax));

        // Wrong project context is denied.
        let other_proj = principal_for("usr:alice").with_project("proj:bar");
        assert!(!principal_can_read(&other_proj, scope, tax));
    }

    #[test]
    fn session_visibility_requires_matching_session_id() {
        let scope = r#"{"user":"usr:alice","session_id":"sess:abc"}"#;
        let tax = r#"{"visibility":"session"}"#;

        let no_ctx = principal_for("usr:alice");
        assert!(!principal_can_read(&no_ctx, scope, tax));

        let in_sess = principal_for("usr:alice").with_session("sess:abc");
        assert!(principal_can_read(&in_sess, scope, tax));

        let other_sess = principal_for("usr:alice").with_session("sess:xyz");
        assert!(!principal_can_read(&other_sess, scope, tax));
    }

    #[test]
    fn team_and_org_still_fail_closed() {
        // ScopeTuple does not yet carry team/org dimensions — adding
        // them is a brief-level change, so these tiers continue to
        // fail closed for all non-system principals.
        let p = principal_for("usr:alice")
            .with_teams(["team:eng"])
            .with_orgs(["org:acme"]);
        let scope = r#"{"user":"usr:alice"}"#;
        for tier in ["team", "org"] {
            let tax = format!(r#"{{"visibility":"{tier}"}}"#);
            assert!(
                !principal_can_read(&p, scope, &tax),
                "{tier} must fail closed until ScopeTuple is extended"
            );
        }
    }

    #[test]
    fn shared_agent_identity_cannot_read_other_users_private_rows() {
        // Regression: an agent identity (e.g. a shared `agt:claude-code`
        // runtime) writes records on behalf of multiple users. The
        // agent's principal must NOT be granted read access to a private
        // row owned by a different user just because `scope.agent`
        // matches the agent identity. Authorization is owner-match on
        // `scope.user` only.
        let agent = principal_for("agt:claude-code:opus-4-7:main:v1");
        let scope = r#"{"user":"usr:alice","agent":"agt:claude-code:opus-4-7:main:v1"}"#;
        for tier in ["private", "session", "team", "org"] {
            let tax = format!(r#"{{"visibility":"{tier}"}}"#);
            assert!(
                !principal_can_read(&agent, scope, &tax),
                "agent identity must not authorize {tier} reads on usr:alice's row"
            );
        }
    }
}
