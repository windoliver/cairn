//! `explain_filter` partitions visible candidates into kept and
//! excluded sets, attaching a `RecordExclusion` to each filtered
//! record. Tier-1-invisible records must already be absent from the
//! candidate set — this function does not see them.

use cairn_core::domain::TargetId;
use cairn_core::pipeline::explain::{Candidate, ExplainConfig, ReadFilterReason, explain_filter};
use cairn_core::policy_trace::{PolicyDetail, PolicyGate};

// Build a valid 26-char ULID-shaped TargetId. Crockford base32 (no I L O U),
// leading char 0..=7. Trailing char(s) replaced by `suffix` for uniqueness.
fn id(suffix: char) -> TargetId {
    let mut s = String::from("01HQZX9F5N0000000000000000");
    s.pop();
    s.push(suffix);
    TargetId::parse(s).expect("valid ULID")
}

#[test]
fn empty_candidates_yields_empty_kept_and_excluded() {
    let cfg = ExplainConfig {
        staleness_threshold_days: 30,
    };
    let (kept, excluded) = explain_filter(Vec::<Candidate>::new(), &cfg);
    assert!(kept.is_empty());
    assert!(excluded.is_empty());
}

#[test]
fn stale_candidate_is_excluded_with_staleness_gate() {
    let cfg = ExplainConfig {
        staleness_threshold_days: 30,
    };
    let candidates = vec![Candidate::from_scope_filter(
        id('A'),
        90,
        0.8,
        "h1".to_owned(),
    )];
    let (kept, excluded) = explain_filter(candidates, &cfg);
    assert!(kept.is_empty());
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].gate, PolicyGate::ReadFilterStaleness);
    assert_eq!(excluded[0].detail, PolicyDetail::None);
}

#[test]
fn duplicate_content_hash_excluded_by_dedup() {
    let cfg = ExplainConfig {
        staleness_threshold_days: 30,
    };
    let candidates = vec![
        Candidate::from_scope_filter(id('A'), 1, 0.9, "h".to_owned()),
        Candidate::from_scope_filter(id('B'), 1, 0.8, "h".to_owned()),
    ];
    let (kept, excluded) = explain_filter(candidates, &cfg);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].target_id(), &id('A'));
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].target_id, id('B'));
    assert_eq!(excluded[0].gate, PolicyGate::ReadFilterDedup);
}

#[test]
fn stale_takes_precedence_over_dedup() {
    let cfg = ExplainConfig {
        staleness_threshold_days: 30,
    };
    let candidates = vec![
        Candidate::from_scope_filter(id('A'), 90, 0.9, "h".to_owned()),
        Candidate::from_scope_filter(id('B'), 1, 0.5, "h".to_owned()),
    ];
    let (kept, excluded) = explain_filter(candidates, &cfg);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].target_id(), &id('B'));
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].gate, PolicyGate::ReadFilterStaleness);
}

#[test]
fn nan_score_loses_to_non_nan_regardless_of_arrival_order() {
    let cfg = ExplainConfig {
        staleness_threshold_days: 30,
    };
    // NaN seen first; non-NaN second — non-NaN must win the dedup.
    let candidates = vec![
        Candidate::from_scope_filter(id('A'), 1, f32::NAN, "h".to_owned()),
        Candidate::from_scope_filter(id('B'), 1, 0.1, "h".to_owned()),
    ];
    let (kept, excluded) = explain_filter(candidates, &cfg);
    assert_eq!(kept.len(), 1, "exactly one survives dedup");
    assert_eq!(kept[0].target_id(), &id('B'), "non-NaN wins over NaN");
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].target_id, id('A'));
    assert_eq!(excluded[0].gate, PolicyGate::ReadFilterDedup);
}

#[test]
fn nan_score_loses_to_non_nan_when_seen_second() {
    let cfg = ExplainConfig {
        staleness_threshold_days: 30,
    };
    // Non-NaN first, NaN second — NaN must lose to the non-NaN that came first.
    let candidates = vec![
        Candidate::from_scope_filter(id('A'), 1, 0.1, "h".to_owned()),
        Candidate::from_scope_filter(id('B'), 1, f32::NAN, "h".to_owned()),
    ];
    let (kept, excluded) = explain_filter(candidates, &cfg);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].target_id(), &id('A'), "non-NaN keeps its win");
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].target_id, id('B'));
}

#[test]
fn two_nan_scores_resolve_to_first_seen() {
    let cfg = ExplainConfig {
        staleness_threshold_days: 30,
    };
    let candidates = vec![
        Candidate::from_scope_filter(id('A'), 1, f32::NAN, "h".to_owned()),
        Candidate::from_scope_filter(id('B'), 1, f32::NAN, "h".to_owned()),
    ];
    let (kept, _excluded) = explain_filter(candidates, &cfg);
    assert_eq!(kept[0].target_id(), &id('A'), "first NaN wins tie-break");
}

// Compile-time assertion that `Candidate` has no public field access.
// The struct-literal form below would be rejected by the compiler
// because all fields are private; only `Candidate::from_scope_filter`
// is callable from outside the module. If a future change makes any
// field `pub`, the visibility-precondition leak codex flagged in
// PR #237 round 3 returns. This is a compile-time assertion only —
// uncommenting must fail to build:
//
//     let _ = Candidate { target_id: id('Z'), age_days: 0,
//                         relevance_score: 0.0, content_hash: String::new() };

#[test]
fn read_filter_reason_round_trips() {
    let cases = [
        (ReadFilterReason::Staleness, PolicyGate::ReadFilterStaleness),
        (ReadFilterReason::Dedup, PolicyGate::ReadFilterDedup),
        (ReadFilterReason::Relevance, PolicyGate::ReadFilterRelevance),
    ];
    for (reason, expected_gate) in cases {
        assert_eq!(reason.as_gate(), expected_gate);
    }
}
