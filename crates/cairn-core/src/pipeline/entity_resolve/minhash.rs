//! Tier 2 — `MinHash` 3-gram Jaccard fuzzy match.

use std::hash::Hasher as _;

use twox_hash::XxHash64;

use crate::domain::graph::{EntityId, EntityNode};
use crate::pipeline::entity_resolve::MAX_NUM_PERMUTATIONS;

/// 128-permutation `MinHash` signature. Only the first `num_permutations`
/// slots are populated; the remainder are filled with `u64::MAX` and
/// must be ignored by [`jaccard`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
#[allow(dead_code)] // Task 6 wires this into EntityResolver; suppress dead_code until then.
pub struct MinHashSignature(pub [u64; MAX_NUM_PERMUTATIONS]);

impl MinHashSignature {
    /// All-`u64::MAX` signature, used as the starting point for [`signature`].
    pub const fn empty() -> Self {
        Self([u64::MAX; MAX_NUM_PERMUTATIONS])
    }
}

/// Yield 3-gram shingle byte-ranges over `norm`. UTF-8 safe (uses
/// `char_indices`). Strings shorter than 3 chars produce a single
/// shingle covering the whole string. Empty input produces an empty vec.
#[must_use]
#[allow(dead_code)] // Task 6 wires this into EntityResolver; suppress dead_code until then.
pub fn shingles(norm: &str) -> Vec<(usize, usize)> {
    let chars: Vec<(usize, char)> = norm.char_indices().collect();
    if chars.len() < 3 {
        if norm.is_empty() {
            return Vec::new();
        }
        return vec![(0, norm.len())];
    }
    let mut out = Vec::with_capacity(chars.len() - 2);
    for window in chars.windows(3) {
        let start = window[0].0;
        let end_char_start = window[2].0;
        let end = end_char_start + window[2].1.len_utf8();
        out.push((start, end));
    }
    out
}

/// Compute a [`MinHashSignature`] for `norm` using the supplied
/// `seeds`. `seeds.len()` should equal the configured `num_permutations`;
/// slots beyond `seeds.len()` are left at `u64::MAX`.
#[allow(dead_code)] // Task 6 wires this into EntityResolver; suppress dead_code until then.
pub fn signature(norm: &str, shingle_ranges: &[(usize, usize)], seeds: &[u64]) -> MinHashSignature {
    let mut sig = MinHashSignature::empty();
    if shingle_ranges.is_empty() {
        return sig;
    }
    for (slot, &seed) in seeds.iter().enumerate().take(MAX_NUM_PERMUTATIONS) {
        let mut min_hash = u64::MAX;
        for &(s, e) in shingle_ranges {
            let bytes = &norm.as_bytes()[s..e];
            let mut hasher = XxHash64::with_seed(seed);
            hasher.write(bytes);
            let h = hasher.finish();
            if h < min_hash {
                min_hash = h;
            }
        }
        sig.0[slot] = min_hash;
    }
    sig
}

/// Jaccard similarity over the first `n` slots of two signatures.
/// Returns a value in `[0.0, 1.0]`. `n` is clamped to
/// `1..=MAX_NUM_PERMUTATIONS`. Caller's invariant is that `n` equals
/// `num_permutations` from `ResolverConfig` (validated upstream).
#[must_use]
#[allow(dead_code)] // Task 6 wires this into EntityResolver; suppress dead_code until then.
pub fn jaccard(a: &MinHashSignature, b: &MinHashSignature, n: usize) -> f32 {
    debug_assert!(
        n > 0 && n <= MAX_NUM_PERMUTATIONS,
        "invariant: n in 1..=MAX_NUM_PERMUTATIONS"
    );
    let n = n.clamp(1, MAX_NUM_PERMUTATIONS);
    let mut hits = 0u32;
    for i in 0..n {
        if a.0[i] == b.0[i] {
            hits += 1;
        }
    }
    // hits ≤ MAX_NUM_PERMUTATIONS = 128 fits exactly in f32 mantissa.
    #[allow(clippy::cast_precision_loss)]
    {
        hits as f32 / n as f32
    }
}

/// Per-existing scored entry returned by [`fuzzy_match`].
/// Sorted descending by `jaccard`; ties broken by `EntityId` lex order.
#[derive(Debug)]
#[must_use]
#[allow(dead_code)] // Task 6 wires this into EntityResolver; suppress dead_code until then.
pub struct Scored<'a> {
    /// Existing entity that was scored.
    pub node: &'a EntityNode,
    /// Jaccard score against the candidate signature (0.0..=1.0).
    pub jaccard: f32,
}

/// Outcome of the Tier-2 pass over the supplied existing entities.
#[derive(Debug)]
#[must_use]
#[allow(dead_code)] // Task 6 wires this into EntityResolver; suppress dead_code until then.
pub enum FuzzyOutcome {
    /// No existing entity scored at or above `threshold`.
    None,
    /// Exactly one existing entity scored at or above `threshold`.
    One(EntityId),
    /// Two or more existing entities scored at or above `threshold`.
    /// Caller (issue: ingest verb) decides how to disambiguate.
    Many(Vec<EntityId>),
}

/// Tier-2 fuzzy match. Returns the outcome plus all scored entities,
/// sorted descending by Jaccard with ties broken by `EntityId` lex
/// order. The `Scored` slice is the source of truth Tier 3 uses to
/// pick its top-1 in-band candidate.
#[allow(dead_code)] // Task 6 wires this into EntityResolver; suppress dead_code until then.
pub fn fuzzy_match<'a>(
    cand_sig: &MinHashSignature,
    existing: &'a [EntityNode],
    seeds: &[u64],
    threshold: f32,
    n: usize,
) -> (FuzzyOutcome, Vec<Scored<'a>>) {
    let mut scored: Vec<Scored<'a>> = existing
        .iter()
        .map(|node| {
            let ranges = shingles(&node.name_norm);
            let sig = signature(&node.name_norm, &ranges, seeds);
            let j = jaccard(cand_sig, &sig, n);
            Scored { node, jaccard: j }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.jaccard
            .partial_cmp(&a.jaccard)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.node.id.as_str().cmp(b.node.id.as_str()))
    });

    let hits: Vec<&Scored<'_>> = scored.iter().filter(|s| s.jaccard >= threshold).collect();
    let outcome = match hits.as_slice() {
        [] => FuzzyOutcome::None,
        [only] => FuzzyOutcome::One(only.node.id.clone()),
        many => FuzzyOutcome::Many(many.iter().map(|s| s.node.id.clone()).collect()),
    };
    (outcome, scored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::entity_resolve::DEFAULT_HASH_SEED;

    fn seeds(n: usize) -> Vec<u64> {
        // splitmix64 expansion of DEFAULT_HASH_SEED — same algorithm
        // EntityResolver::new will use to derive permutation seeds.
        let mut state = DEFAULT_HASH_SEED;
        (0..n)
            .map(|_| {
                state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^ (z >> 31)
            })
            .collect()
    }

    fn node(id: &str, name_norm: &str) -> EntityNode {
        EntityNode {
            id: EntityId::from(id),
            name: name_norm.to_owned(),
            name_norm: name_norm.to_owned(),
            summary: None,
            created_at: 0,
            embedding_id: None,
        }
    }

    #[test]
    fn shingles_for_short_strings() {
        assert!(shingles("").is_empty());
        let r1 = shingles("a");
        assert_eq!(r1, vec![(0, 1)]);
        let r2 = shingles("ab");
        assert_eq!(r2, vec![(0, 2)]);
    }

    #[test]
    fn shingles_for_three_chars_yields_one_window() {
        let r = shingles("abc");
        assert_eq!(r.len(), 1);
        let (s, e) = r[0];
        assert_eq!(&"abc"[s..e], "abc");
    }

    #[test]
    fn shingles_count_for_four_chars() {
        let r = shingles("abcd");
        assert_eq!(r.len(), 2);
        assert_eq!(&"abcd"[r[0].0..r[0].1], "abc");
        assert_eq!(&"abcd"[r[1].0..r[1].1], "bcd");
    }

    #[test]
    fn signature_is_deterministic() {
        let s = seeds(128);
        let r = shingles("authservice");
        let a = signature("authservice", &r, &s);
        let b = signature("authservice", &r, &s);
        assert_eq!(a, b);
    }

    #[test]
    fn jaccard_self_is_one() {
        let s = seeds(128);
        let r = shingles("authservice");
        let sig = signature("authservice", &r, &s);
        assert!((jaccard(&sig, &sig, 128) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn jaccard_in_unit_interval() {
        let s = seeds(128);
        let a = signature("authservice", &shingles("authservice"), &s);
        let b = signature("billing", &shingles("billing"), &s);
        let j = jaccard(&a, &b, 128);
        assert!((0.0..=1.0).contains(&j));
    }

    #[test]
    fn fuzzy_match_finds_high_jaccard_pair() {
        let s = seeds(128);
        let existing = vec![
            node("01HZE7JV5N0000000000000001", "auth service"),
            node("01HZE7JV5N0000000000000002", "billing service"),
        ];
        let cand = "auth service";
        let cand_sig = signature(cand, &shingles(cand), &s);
        let (outcome, scored) = fuzzy_match(&cand_sig, &existing, &s, 0.85, 128);
        match outcome {
            FuzzyOutcome::One(id) => {
                assert_eq!(id.as_str(), "01HZE7JV5N0000000000000001");
            }
            other => panic!("expected One, got {other:?}"),
        }
        let top = scored
            .first()
            .expect("invariant: scored non-empty when existing non-empty");
        assert_eq!(top.node.id.as_str(), "01HZE7JV5N0000000000000001");
    }

    #[test]
    fn fuzzy_match_returns_none_when_below_threshold() {
        let s = seeds(128);
        let existing = vec![node("01HZE7JV5N0000000000000001", "billing")];
        let cand = "auth service";
        let cand_sig = signature(cand, &shingles(cand), &s);
        let (outcome, _) = fuzzy_match(&cand_sig, &existing, &s, 0.85, 128);
        assert!(matches!(outcome, FuzzyOutcome::None));
    }

    #[test]
    fn fuzzy_match_empty_existing() {
        let s = seeds(128);
        let cand = "auth service";
        let cand_sig = signature(cand, &shingles(cand), &s);
        let (outcome, scored) = fuzzy_match(&cand_sig, &[], &s, 0.85, 128);
        assert!(matches!(outcome, FuzzyOutcome::None));
        assert!(scored.is_empty());
    }

    #[test]
    fn fuzzy_match_returns_many_when_multiple_above_threshold() {
        let s = seeds(128);
        // Two existing entities with name_norm identical to the candidate
        // → both score Jaccard 1.0 → FuzzyOutcome::Many.
        let existing = vec![
            node("01HZE7JV5N0000000000000001", "auth service"),
            node("01HZE7JV5N0000000000000002", "auth service"),
        ];
        let cand = "auth service";
        let cand_sig = signature(cand, &shingles(cand), &s);
        let (outcome, scored) = fuzzy_match(&cand_sig, &existing, &s, 0.85, 128);
        match outcome {
            FuzzyOutcome::Many(ids) => {
                assert_eq!(ids.len(), 2);
                // Sorted desc by jaccard (tied) then asc by EntityId lex.
                assert_eq!(ids[0].as_str(), "01HZE7JV5N0000000000000001");
                assert_eq!(ids[1].as_str(), "01HZE7JV5N0000000000000002");
            }
            other => panic!("expected Many, got {other:?}"),
        }
        assert_eq!(scored.len(), 2);
    }
}
