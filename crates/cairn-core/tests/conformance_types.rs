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
    assert_eq!(outcome.tier, Tier::Two);
    assert!(matches!(
        outcome.status,
        CaseStatus::Pending {
            reason: "real impl pending"
        }
    ));
}

#[test]
fn case_outcome_constructs_ok() {
    let outcome = CaseOutcome {
        id: "arc_pointer_stable",
        tier: Tier::One,
        status: CaseStatus::Ok,
    };
    assert_eq!(outcome.id, "arc_pointer_stable");
    assert_eq!(outcome.tier, Tier::One);
    assert!(matches!(outcome.status, CaseStatus::Ok));
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
    assert_eq!(outcome.id, "manifest_matches_host");
    assert_eq!(outcome.tier, Tier::One);
    assert!(
        matches!(&outcome.status, CaseStatus::Failed { message } if message == "version mismatch")
    );
}
