//! Canonical normalization for entity-node `name_norm`.
//!
//! Single source of truth for the dedup key used by every
//! `MemoryStore::upsert_entity` insertion site and every read-side
//! lookup (e.g. the `graph.get_entity` `ByName` arm, spec §3.1).
//!
//! Behaviour:
//! - NFC unicode normalization
//! - lowercase (Unicode-aware via `char::to_lowercase`)
//! - strip ASCII punctuation (`char::is_ascii_punctuation`)
//! - collapse runs of whitespace to a single ASCII space
//! - trim leading + trailing whitespace
//!
//! Pure: no I/O, no global state, no allocations beyond the return value.

use unicode_normalization::UnicodeNormalization;

/// Canonical form used as the `entity_nodes.name_norm` dedup key.
///
/// Returns `None` when the input collapses to an empty key (whitespace- or
/// punctuation-only inputs like `""`, `"   "`, `"!!!"` would otherwise share
/// the same empty `name_norm` and silently dedup distinct entities onto a
/// single row at write time, or resolve a `ByName` lookup to an arbitrary
/// existing empty-key entity at read time). All callers MUST check for
/// `Some(_)` before binding the value as a `name_norm` parameter.
///
/// Idempotent on the `Some` branch: if `normalize_entity_name(x) ==
/// Some(s)` then `normalize_entity_name(&s) == Some(s)` (covered by
/// proptest below).
#[must_use]
pub fn normalize_entity_name(input: &str) -> Option<String> {
    // 1. NFC-normalize so visually identical strings compare equal.
    let nfc: String = input.nfc().collect();

    // 2. Build the output: lowercase, drop punctuation, collapse whitespace.
    let mut out = String::with_capacity(nfc.len());
    let mut prev_was_space = true; // start true so leading WS is dropped
    for ch in nfc.chars() {
        if ch.is_ascii_punctuation() {
            // Treat punctuation as a soft separator: collapse to a space.
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
            continue;
        }
        if ch.is_whitespace() {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
            continue;
        }
        for lower in ch.to_lowercase() {
            out.push(lower);
        }
        prev_was_space = false;
    }

    // 3. Trim trailing whitespace produced by the loop above.
    if out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_ascii() {
        assert_eq!(normalize_entity_name("Alice").as_deref(), Some("alice"));
    }

    #[test]
    fn strips_punctuation_and_collapses_whitespace() {
        assert_eq!(
            normalize_entity_name("Auth Service (v2)").as_deref(),
            Some("auth service v2")
        );
    }

    #[test]
    fn trims_and_collapses_runs_of_whitespace() {
        assert_eq!(
            normalize_entity_name("  foo \t\n bar  ").as_deref(),
            Some("foo bar")
        );
    }

    #[test]
    fn nfc_normalises_decomposed_unicode() {
        // "café" composed (U+00E9) vs decomposed (e + U+0301)
        let composed = "Caf\u{00E9}";
        let decomposed = "Cafe\u{0301}";
        assert_eq!(
            normalize_entity_name(composed),
            normalize_entity_name(decomposed)
        );
    }

    #[test]
    fn empty_canonical_key_is_rejected() {
        // Round 4 review: empty/punctuation-only inputs MUST NOT collapse to
        // the same dedup key — that would let unrelated entities merge at
        // upsert time and let punctuation-only ByName lookups resolve to
        // arbitrary rows. Caller is required to handle `None`.
        assert_eq!(normalize_entity_name(""), None);
        assert_eq!(normalize_entity_name("   "), None);
        assert_eq!(normalize_entity_name("!!!"), None);
    }

    #[test]
    fn ascii_only_inputs_are_byte_identical_after_one_pass() {
        // Regression guard: existing fixtures use "alice" — must round-trip unchanged.
        assert_eq!(normalize_entity_name("alice").as_deref(), Some("alice"));
        assert_eq!(normalize_entity_name("acme").as_deref(), Some("acme"));
    }

    use proptest::prelude::*;

    proptest! {
        /// `norm(norm(x)) == norm(x)` for every input that produces Some.
        #[test]
        fn idempotent(s in ".{0,128}") {
            let once = normalize_entity_name(&s);
            if let Some(ref once_str) = once {
                let twice = normalize_entity_name(once_str);
                prop_assert_eq!(once.clone(), twice);
            }
        }

        /// Trailing whitespace or punctuation never affects the result —
        /// i.e. the function is invariant under right-padding by junk.
        #[test]
        fn trailing_junk_is_absorbed(s in "[A-Za-z0-9 ]{0,32}", junk in "[ \t\n!?.,;:]{0,8}") {
            let plain = normalize_entity_name(&s);
            let padded = normalize_entity_name(&format!("{s}{junk}"));
            prop_assert_eq!(plain, padded);
        }

        /// Output never contains ASCII punctuation.
        #[test]
        fn output_has_no_punctuation(s in ".{0,128}") {
            if let Some(out) = normalize_entity_name(&s) {
                prop_assert!(!out.chars().any(|c| c.is_ascii_punctuation()));
            }
        }

        /// Output never contains uppercase ASCII letters.
        #[test]
        fn output_is_lowercased(s in ".{0,128}") {
            if let Some(out) = normalize_entity_name(&s) {
                prop_assert!(!out.chars().any(|c| c.is_ascii_uppercase()));
            }
        }

        /// Output is never the empty string when Some.
        #[test]
        fn never_returns_empty_some(s in ".{0,128}") {
            if let Some(out) = normalize_entity_name(&s) {
                prop_assert!(!out.is_empty());
            }
        }
    }
}
