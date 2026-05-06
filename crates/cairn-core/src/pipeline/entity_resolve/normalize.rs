//! Tier 1 — name normalization and exact match.

use unicode_normalization::UnicodeNormalization;

use crate::domain::graph::{EntityId, EntityNode};

/// Normalize an entity name for exact comparison.
///
/// Pipeline (Unicode-aware):
/// 1. NFKC normalization — collapse canonical AND compatibility
///    equivalents. NFC alone leaves visually-identical lookalikes
///    distinct: full-width Latin `ＡｕｔｈＳｅｒｖｉｃｅ`, ligatures `ﬁ`,
///    superscripts, presentation forms, etc. NFKC folds them to
///    ASCII / standard form so a lookalike attack cannot create
///    a duplicate entity under a different `name_norm`
///    (codex-review R4.2 + R6.2). Known limitation: German `ß` is
///    NOT folded to `ss` because Rust's `char::to_lowercase` is
///    locale-independent and Unicode case-folding (a separate
///    operation) is not in `unicode-normalization`. Acceptable for
///    P0; tracked for a follow-up if it becomes load-bearing.
/// 2. Lowercase via `char::to_lowercase` (Unicode, multi-codepoint).
/// 3. Retain Unicode alphanumerics (`char::is_alphanumeric`) plus a
///    small set of identity-bearing ASCII symbols (`+ # & . * '`)
///    when adjacent to an already-emitted alphanumeric. This keeps
///    `C`, `C++`, `C#`, `F#`, `AT&T`, `.NET`, `O'Brien` distinct
///    rather than collapsing them all to bare `c` / `at` / `obrien`
///    and forcing Tier-1 false-merges (codex-review R8.1). All
///    other symbols and ASCII whitespace fold to a single `' '`.
/// 4. Collapse runs of whitespace to single spaces.
/// 5. Trim leading + trailing whitespace.
///
/// `normalize` is idempotent: `normalize(normalize(s)) == normalize(s)`
/// (proptest in `proptests.rs`).
///
/// Empty / all-punctuation / all-whitespace input yields an empty string;
/// the orchestrator returns `EntityResolutionError::EmptyNormalizedName`
/// rather than allow an empty key to be persisted (would otherwise
/// collide on the store's `UNIQUE(name_norm)` index).
#[must_use]
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = true; // suppresses leading whitespace
    // NFKC ensures canonical AND compatibility forms compare equal;
    // e.g. `Café` (U+00E9) ≡ `Cafe\u{0301}`, full-width `Ａ` ≡ `A`,
    // ligature `ﬁ` ≡ `fi`. R6.2 hardening.
    for c in s.nfkc() {
        if c.is_alphanumeric() {
            // `char::to_lowercase` returns an iterator (some codepoints
            // expand to multiple lowercase codepoints, e.g. German ß
            // → "ss"). Some letters lowercase into an alphanumeric +
            // combining mark — Turkish `İ` → `i` + U+0307 — so each
            // emitted codepoint is re-checked against the alphanumeric
            // filter. Without this re-filter, the combining mark would
            // survive but be dropped on a second `normalize` pass,
            // breaking idempotency (codex-review R3.1).
            for lc in c.to_lowercase() {
                if lc.is_alphanumeric() {
                    out.push(lc);
                    last_was_space = false;
                }
            }
        } else if matches!(c, '+' | '#' | '&' | '.' | '*' | '\'') && !last_was_space {
            // R8.1: preserve identity-bearing ASCII symbols when
            // adjacent to an already-emitted alphanumeric. Keeps
            // `C++`/`C#`/`AT&T`/`.NET`/`O'Brien` distinct from `C`/
            // `AT`/`NET`/`OBrien`. Leading or post-space symbols are
            // still dropped (no `&foo`).
            out.push(c);
            // `last_was_space` stays false — these symbols don't
            // separate words; they decorate the previous one.
        } else if matches!(c, ' ' | '\t' | '\n' | '\r') && !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
        // All other characters (punctuation, symbols, control) are dropped.
    }
    if out.ends_with(' ') {
        out.pop();
    }
    // R9.2: strip trailing sentence/wrapper punctuation that the
    // symbol-preservation rule may have admitted. `AuthService.` and
    // `*C#*` are formatting noise around the real identity, not
    // identity suffixes themselves. `+`, `#`, `&`, `'` stay (`C++`,
    // `C#`, `AT&T`, `O'`-style abbreviations are real identity
    // tokens). Conservative: only `.` and `*` strip — extending the
    // set risks false-positive identity collisions.
    while out.ends_with('.') || out.ends_with('*') {
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
    fn preserves_identity_bearing_symbols() {
        // Codex-review R8.1: distinct programming-language entities
        // and brand names share an alphanumeric prefix but differ
        // only in trailing/embedded symbols. Must not collapse to
        // the same `name_norm`.
        assert_eq!(normalize("C"), "c");
        assert_eq!(normalize("C++"), "c++");
        assert_eq!(normalize("C#"), "c#");
        assert_eq!(normalize("F#"), "f#");
        assert_eq!(normalize("AT&T"), "at&t");
        // `.NET` — leading `.` is a "post-space" symbol so dropped;
        // we keep "net" distinct from "net foundation". An exact
        // match against another `.NET` still works because both
        // sides apply the same rule.
        assert_eq!(normalize(".NET"), "net");
        assert_eq!(normalize("O'Brien"), "o'brien");
        // Distinctness checks that close the false-merge loophole.
        assert_ne!(normalize("C"), normalize("C++"));
        assert_ne!(normalize("C++"), normalize("C#"));
        assert_ne!(normalize("AT&T"), normalize("AT"));
    }

    #[test]
    fn strips_trailing_formatting_punctuation() {
        // Codex-review R9.2: extracted text often has trailing
        // sentence punctuation or markdown wrappers around real
        // identity tokens. These must NOT survive normalization or
        // they'd force duplicate entities.
        assert_eq!(normalize("AuthService."), normalize("AuthService"));
        assert_eq!(normalize("AuthService..."), normalize("AuthService"));
        assert_eq!(normalize("*C#*"), normalize("C#"));
        assert_eq!(normalize("*AuthService*"), normalize("AuthService"));
        // Mixed trailing wrappers strip.
        assert_eq!(normalize("AuthService.*"), normalize("AuthService"));
        // Identity-bearing suffixes that LOOK like wrappers are
        // preserved when not in the strip set: `C++` keeps both `+`,
        // `C#` keeps `#`, `AT&T` keeps `&t`.
        assert_eq!(normalize("C++"), "c++");
        assert_eq!(normalize("C#"), "c#");
        assert_eq!(normalize("AT&T"), "at&t");
    }

    #[test]
    fn nfkc_collapses_compatibility_equivalents() {
        // Codex-review R6.2: NFC-only left lookalikes distinct.
        // Full-width Latin must fold to ASCII Latin.
        assert_eq!(
            normalize("ＡｕｔｈＳｅｒｖｉｃｅ"),
            normalize("AuthService")
        );
        // Ligatures fold to their component letters.
        assert_eq!(normalize("ﬁsh"), normalize("fish"));
        // Superscripts / presentation forms collapse via NFKC's
        // compatibility map.
        assert_eq!(normalize("H²O"), normalize("H2O"));
    }

    #[test]
    fn nfc_collapses_canonical_equivalents() {
        // Codex-review R4.2: precomposed `é` (U+00E9) and decomposed
        // `e` + U+0301 are visually identical and must produce the
        // same `name_norm` — otherwise Tier 1 misses and a duplicate
        // entity is created.
        assert_eq!(normalize("Café"), normalize("Cafe\u{0301}"));
        // NFC also collapses Korean syllable forms.
        assert_eq!(
            normalize("\u{AC00}"),         // 가 (precomposed)
            normalize("\u{1100}\u{1161}")  // ᄀ + ᅡ (jamo)
        );
    }

    #[test]
    fn turkish_capital_i_lowercases_idempotently() {
        // Codex-review R3.1: `İ` (U+0130) → `i` + U+0307 via to_lowercase.
        // The combining mark must not survive — both passes must produce
        // the same `name_norm`.
        let once = normalize("İstanbul");
        let twice = normalize(&once);
        assert_eq!(once, twice, "normalize must be idempotent over Unicode");
        // Spot-check: combining mark dropped, base letter retained.
        assert!(once.contains('i'), "expected base 'i' in {once}");
        assert!(
            !once.contains('\u{0307}'),
            "combining mark survived: {once:?}"
        );
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
