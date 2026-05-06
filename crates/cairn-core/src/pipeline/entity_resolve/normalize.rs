//! Tier 1 — name normalization and exact match.

use crate::domain::graph::{EntityId, EntityNode};

/// Normalize an entity name for exact comparison.
///
/// Pipeline (Unicode-aware, per issue #187 verbatim):
/// 1. Lowercase via `char::to_lowercase` (Unicode, multi-codepoint).
/// 2. Retain only Unicode alphanumerics (`char::is_alphanumeric`) plus
///    ASCII whitespace (folded to a single `' '`).
/// 3. Collapse runs of whitespace to single spaces.
/// 4. Trim leading + trailing whitespace.
///
/// `normalize` is idempotent: `normalize(normalize(s)) == normalize(s)`
/// (proptest in `proptests.rs`).
///
/// Empty / all-punctuation / all-whitespace input yields an empty string;
/// callers MUST treat that as "no candidate identity" — the resolver short-
/// circuits to `Resolution::New` rather than allow an empty key to collide
/// with another empty key (would otherwise force a false merge between
/// e.g. `"!!!"` and `"???"`).
#[must_use]
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = true; // suppresses leading whitespace
    for c in s.chars() {
        if c.is_alphanumeric() {
            // `char::to_lowercase` returns an iterator (some codepoints
            // expand to multiple lowercase codepoints, e.g. German ß).
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            last_was_space = false;
        } else if matches!(c, ' ' | '\t' | '\n' | '\r') && !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
        // All other characters (punctuation, symbols, control) are dropped.
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
    fn preserves_unicode_letters() {
        // Unicode-aware normalize: accented + non-Latin letters survive.
        assert_eq!(normalize("AuthSérvice"), "authsérvice");
        assert_eq!(normalize("Кириллица"), "кириллица");
        assert_eq!(normalize("日本語"), "日本語");
    }

    #[test]
    fn distinct_non_ascii_names_do_not_collapse_to_empty() {
        // Regression for codex-review finding R2.3: in the ASCII-only
        // implementation, `Кириллица` and `日本語` both normalized to `""`
        // and would collide as duplicates. Unicode-aware normalize keeps
        // them distinct.
        assert_ne!(normalize("Кириллица"), normalize("日本語"));
        assert!(!normalize("Кириллица").is_empty());
        assert!(!normalize("日本語").is_empty());
    }

    #[test]
    fn punctuation_only_input_yields_empty() {
        // Empty `name_norm` is the resolver's signal for "no candidate
        // identity"; the orchestrator short-circuits to Resolution::New
        // rather than let two empty keys force a false merge.
        assert_eq!(normalize("!!!"), "");
        assert_eq!(normalize("???"), "");
        assert_eq!(normalize("---"), "");
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
