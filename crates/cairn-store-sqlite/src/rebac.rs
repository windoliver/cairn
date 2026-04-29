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
/// - `"session"` visibility  → owner match (collapses to `private`
///   semantics until `Principal` carries a session id; granting access
///   by row-tag alone would leak across users).
/// - `"project"` visibility  → owner match (collapses to `private`
///   semantics until `Principal` carries a verified project context).
/// - `"team"`, `"org"` → owner match (fail closed until `Principal`
///   carries verifiable membership context — granting access by row
///   tag alone would expose data across tenants).
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
        "private" | "session" | "project" | "team" | "org" => {
            let scope_user = scope.get("user").and_then(Value::as_str).unwrap_or("");
            !scope_user.is_empty() && id_str == scope_user
        }
        "public" => true,
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
    fn team_owner_match() {
        let p = principal_for("usr:alice");
        let scope = r#"{"user":"usr:alice"}"#;
        let tax = r#"{"visibility":"team"}"#;
        assert!(principal_can_read(&p, scope, tax));
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
    fn project_visibility_owner_match() {
        // Regression: `project` is one of the 6 documented visibility
        // tiers and must not fall into the default deny path.
        let p = principal_for("usr:alice");
        let scope = r#"{"user":"usr:alice"}"#;
        let tax = r#"{"visibility":"project"}"#;
        assert!(principal_can_read(&p, scope, tax));
    }

    #[test]
    fn project_visibility_non_owner_denied() {
        let p = principal_for("usr:bob");
        let scope = r#"{"user":"usr:alice"}"#;
        let tax = r#"{"visibility":"project"}"#;
        assert!(!principal_can_read(&p, scope, tax));
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
