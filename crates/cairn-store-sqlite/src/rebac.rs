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
///   identity string, OR `scope.agent` == principal's identity string.
/// - `"session"` visibility  → `scope.session_id` must be non-empty
///   (P0: no per-session principal field yet; any identified principal
///   can read session-scoped rows within the single-author vault).
/// - `"team"`, `"org"`, `"public"` → any identified principal may read.
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

    match visibility {
        "private" => {
            // Row is readable if the principal's identity matches the scope
            // user OR scope agent dimension.
            let scope_user = scope.get("user").and_then(Value::as_str).unwrap_or("");
            let scope_agent = scope.get("agent").and_then(Value::as_str).unwrap_or("");
            id_str == scope_user || id_str == scope_agent
        }
        "session" => {
            // TODO(#46-followup): session tier is over-permissive — any
            // identified principal can read session-scoped rows. Tighten to
            // require scope.user == principal.identity (collapse to private)
            // once Principal carries a session id.
            //
            // P0 single-author vault: any identified principal may read
            // session-scoped rows. The row must actually have a session
            // dimension set; rowless session is treated as private.
            let row_session = scope
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            !row_session.is_empty()
        }
        "team" | "org" | "public" => true,
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
        let p = Principal::system();
        assert!(principal_can_read(&p, "{}", r#"{"visibility":"private"}"#));
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
}
