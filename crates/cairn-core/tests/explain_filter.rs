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
        dedup_window: 5,
    };
    let (kept, excluded) = explain_filter(Vec::<Candidate>::new(), &cfg);
    assert!(kept.is_empty());
    assert!(excluded.is_empty());
}

#[test]
fn stale_candidate_is_excluded_with_staleness_gate() {
    let cfg = ExplainConfig {
        staleness_threshold_days: 30,
        dedup_window: 5,
    };
    let candidates = vec![Candidate {
        target_id: id('A'),
        age_days: 90,
        relevance_score: 0.8,
        content_hash: "h1".to_owned(),
    }];
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
        dedup_window: 5,
    };
    let candidates = vec![
        Candidate {
            target_id: id('A'),
            age_days: 1,
            relevance_score: 0.9,
            content_hash: "h".to_owned(),
        },
        Candidate {
            target_id: id('B'),
            age_days: 1,
            relevance_score: 0.8,
            content_hash: "h".to_owned(),
        },
    ];
    let (kept, excluded) = explain_filter(candidates, &cfg);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].target_id, id('A'));
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].target_id, id('B'));
    assert_eq!(excluded[0].gate, PolicyGate::ReadFilterDedup);
}

#[test]
fn stale_takes_precedence_over_dedup() {
    let cfg = ExplainConfig {
        staleness_threshold_days: 30,
        dedup_window: 5,
    };
    let candidates = vec![
        Candidate {
            target_id: id('A'),
            age_days: 90,
            relevance_score: 0.9,
            content_hash: "h".to_owned(),
        },
        Candidate {
            target_id: id('B'),
            age_days: 1,
            relevance_score: 0.5,
            content_hash: "h".to_owned(),
        },
    ];
    let (kept, excluded) = explain_filter(candidates, &cfg);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].target_id, id('B'));
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].gate, PolicyGate::ReadFilterStaleness);
}

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
