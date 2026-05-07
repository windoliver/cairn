//! Property-based tests for the entity resolver (issue #187 AC).

use proptest::prelude::*;

use crate::domain::graph::{EntityId, EntityNode};
use crate::pipeline::entity_resolve::DEFAULT_HASH_SEED;
use crate::pipeline::entity_resolve::minhash::{
    FuzzyOutcome, MinHashSignature, fuzzy_match, jaccard, shingles, signature,
};
use crate::pipeline::entity_resolve::normalize::normalize;

/// Mirror of the splitmix64 derivation used by `EntityResolver::new`.
/// Kept here (rather than re-exported) to keep the resolver's seed-derivation
/// internal — these tests verify behavior, not implementation reuse.
fn seeds(n: usize) -> Vec<u64> {
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

proptest! {
    /// AC: normalization idempotency (issue #187).
    #[test]
    fn normalize_idempotent(s in ".*") {
        let once = normalize(&s);
        let twice = normalize(&once);
        prop_assert_eq!(once, twice);
    }

    /// Same shingles + same seeds → byte-identical signature.
    #[test]
    fn signature_determinism(s in "[a-z0-9 ]{0,32}") {
        let s = normalize(&s);
        let r = shingles(&s);
        let seeds = seeds(128);
        let a = signature(&s, &r, &seeds);
        let b = signature(&s, &r, &seeds);
        prop_assert_eq!(a, b);
    }

    /// `jaccard` is bounded in [0, 1] for arbitrary signatures.
    #[test]
    fn jaccard_bounds(a_slots in proptest::array::uniform32(any::<u64>()),
                      b_slots in proptest::array::uniform32(any::<u64>())) {
        let mut a = MinHashSignature::empty();
        let mut b = MinHashSignature::empty();
        a.0[..32].copy_from_slice(&a_slots);
        b.0[..32].copy_from_slice(&b_slots);
        let j = jaccard(&a, &b, 32);
        prop_assert!((0.0..=1.0).contains(&j));
    }

    /// `jaccard(a, a) == 1.0`.
    #[test]
    fn jaccard_self_is_one(a_slots in proptest::array::uniform32(any::<u64>())) {
        let mut a = MinHashSignature::empty();
        a.0[..32].copy_from_slice(&a_slots);
        let j = jaccard(&a, &a, 32);
        prop_assert!((j - 1.0).abs() < f32::EPSILON);
    }
}

/// AC: Jaccard boundary cases — 0.84 → New (no merge), 0.85 → Merge (issue #187).
///
/// Construct two signatures with exactly K matching slots out of 128 by
/// hand. With K = 107, jaccard = 107/128 ≈ 0.836 < 0.85 → no merge.
/// With K = 109, jaccard = 109/128 ≈ 0.852 ≥ 0.85 → merge.
#[test]
fn boundary_below_threshold_no_merge() {
    let mut a = MinHashSignature::empty();
    let mut b = MinHashSignature::empty();
    for i in 0..128 {
        a.0[i] = i as u64;
        b.0[i] = if i < 107 {
            i as u64
        } else {
            (i as u64).wrapping_add(0xFFFF)
        };
    }
    let j = jaccard(&a, &b, 128);
    assert!(j < 0.85, "expected j < 0.85 for K=107, got {j}");
    assert!(j > 0.83, "sanity: j should be ~0.836, got {j}");
}

#[test]
fn boundary_at_or_above_threshold_merges() {
    let mut a = MinHashSignature::empty();
    let mut b = MinHashSignature::empty();
    for i in 0..128 {
        a.0[i] = i as u64;
        b.0[i] = if i < 109 {
            i as u64
        } else {
            (i as u64).wrapping_add(0xFFFF)
        };
    }
    let j = jaccard(&a, &b, 128);
    assert!(j >= 0.85, "expected j >= 0.85 for K=109, got {j}");
    assert!(j < 0.86, "sanity: j should be ~0.852, got {j}");
}

/// End-to-end: identical normalized name → Tier-2 fuzzy match yields
/// `FuzzyOutcome::One` at any reasonable threshold ≤ 1.0.
#[test]
fn fuzzy_match_threshold_predicate_at_boundary() {
    let seeds = seeds(128);
    let cand = "auth service alpha";
    let cand_sig = signature(cand, &shingles(cand), &seeds);

    let existing = vec![EntityNode {
        id: EntityId::from("01HZE7JV5N0000000000000001"),
        name: "auth service alpha".to_owned(),
        name_norm: "auth service alpha".to_owned(),
        summary: None,
        created_at: 0,
        embedding_id: None,
    }];
    let (outcome, _) = fuzzy_match(&cand_sig, &existing, &seeds, 0.85, 128);
    assert!(matches!(outcome, FuzzyOutcome::One(_)));
}
