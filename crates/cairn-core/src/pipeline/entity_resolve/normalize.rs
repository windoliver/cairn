//! Tier 1 — name normalization and exact match.

use crate::domain::graph::{EntityId, EntityNode};

/// Normalize an entity name for exact comparison.
///
/// Pipeline:
/// 1. Lowercase (ASCII only — non-ASCII letters are stripped).
/// 2. Retain only `[a-z0-9 ]` (ASCII alphanumeric + space).
/// 3. Collapse runs of whitespace to single spaces.
/// 4. Trim leading + trailing whitespace.
///
/// `normalize` is idempotent: `normalize(normalize(s)) == normalize(s)`
/// (proptest in `proptests.rs`).
#[must_use]
// Task 6 wires this into EntityResolver; suppress dead_code until then.
#[allow(dead_code)]
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = true; // suppresses leading whitespace
    for c in s.chars() {
        let lc = c.to_ascii_lowercase();
        match lc {
            'a'..='z' | '0'..='9' => {
                out.push(lc);
                last_was_space = false;
            }
            ' ' | '\t' | '\n' | '\r' if !last_was_space => {
                out.push(' ');
                last_was_space = true;
            }
            _ => {}
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Tier 1 exact match: linear scan for `existing[i].name_norm == norm`.
/// Returns the first hit (caller is responsible for ensuring `name_norm`
/// is unique within scope; uniqueness is enforced upstream by the store).
#[must_use]
// Task 6 wires this into EntityResolver; suppress dead_code until then.
#[allow(dead_code)]
pub fn exact_match<'a>(norm: &str, existing: &'a [EntityNode]) -> Option<&'a EntityId> {
    existing.iter().find(|n| n.name_norm == norm).map(|n| &n.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, name: &str, name_norm: &str) -> EntityNode {
        EntityNode {
            id: EntityId::from(id),
            name: name.to_owned(),
            name_norm: name_norm.to_owned(),
            summary: None,
            created_at: 0,
            embedding_id: None,
        }
    }

    #[test]
    fn lowercases_and_strips_punct() {
        assert_eq!(normalize("AuthService"), "authservice");
        assert_eq!(normalize("auth_service"), "authservice");
        assert_eq!(normalize("Auth-Service"), "authservice");
    }

    #[test]
    fn collapses_runs_of_whitespace() {
        assert_eq!(normalize("Auth   Service"), "auth service");
        assert_eq!(normalize("\tauth\nservice "), "auth service");
    }

    #[test]
    fn trims_edges() {
        assert_eq!(normalize("   AuthService   "), "authservice");
    }

    #[test]
    fn empty_input() {
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("   "), "");
    }

    #[test]
    fn preserves_alphanumeric_with_space() {
        assert_eq!(normalize("Auth Service v2"), "auth service v2");
    }

    #[test]
    fn strips_non_ascii_letters() {
        // Documented limitation: Latin-1 letters drop; Tier 2/3 still see them via name_norm.
        assert_eq!(normalize("AuthSérvice"), "authsrvice");
    }

    #[test]
    fn exact_match_finds_existing() {
        let nodes = vec![
            node("01HZE7JV5N0000000000000001", "AuthService", "authservice"),
            node("01HZE7JV5N0000000000000002", "Auth Service", "auth service"),
        ];
        let id = exact_match("authservice", &nodes)
            .expect("invariant: existing nodes contain a name_norm match for `authservice`");
        assert_eq!(id.as_str(), "01HZE7JV5N0000000000000001");
    }

    #[test]
    fn exact_match_misses_when_absent() {
        let nodes = vec![node(
            "01HZE7JV5N0000000000000001",
            "AuthService",
            "authservice",
        )];
        assert!(exact_match("billing", &nodes).is_none());
    }
}
