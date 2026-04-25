//! Smoke test for conformance types — they exist and are constructible.

use cairn_core::contract::conformance::{CaseOutcome, CaseStatus, Tier};

#[test]
fn case_outcome_constructs_pending() {
    let outcome = CaseOutcome {
        id: "put_get_roundtrip",
        tier: Tier::Two,
        status: CaseStatus::Pending {
            reason: "real impl pending",
        },
    };
    assert_eq!(outcome.id, "put_get_roundtrip");
    matches!(outcome.tier, Tier::Two);
    matches!(outcome.status, CaseStatus::Pending { .. });
}

#[test]
fn case_outcome_constructs_ok() {
    let outcome = CaseOutcome {
        id: "register_round_trip",
        tier: Tier::One,
        status: CaseStatus::Ok,
    };
    matches!(outcome.tier, Tier::One);
    matches!(outcome.status, CaseStatus::Ok);
}

#[test]
fn case_outcome_constructs_failed() {
    let outcome = CaseOutcome {
        id: "manifest_matches_host",
        tier: Tier::One,
        status: CaseStatus::Failed {
            message: "version mismatch".to_string(),
        },
    };
    matches!(outcome.status, CaseStatus::Failed { .. });
}
