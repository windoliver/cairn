#![allow(missing_docs)]

use cairn_core::pipeline::skillify::{
    SkillifyCandidateInput, SkillifyOutcome, SkillifySource, SkillifyStatus, SkillifyTrigger,
};

fn input(outcome: SkillifyOutcome) -> SkillifyCandidateInput {
    SkillifyCandidateInput {
        trigger: SkillifyTrigger::Explicit,
        lane: "deploy.hotfix".to_owned(),
        triggers: vec![
            "deploy hotfix".to_owned(),
            "ship emergency patch".to_owned(),
        ],
        source_record_ids: vec![
            "01HQZX9F5N0000000000000001".to_owned(),
            "01HQZX9F5N0000000000000002".to_owned(),
        ],
        sources: vec![
            SkillifySource {
                record_id: "01HQZX9F5N0000000000000001".to_owned(),
                kind: "trace".to_owned(),
                body_sha256:
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        .to_owned(),
            },
            SkillifySource {
                record_id: "01HQZX9F5N0000000000000002".to_owned(),
                kind: "strategy_success".to_owned(),
                body_sha256:
                    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                        .to_owned(),
            },
        ],
        success_criteria: vec![
            "rollback command completed".to_owned(),
            "health check returned 200".to_owned(),
        ],
        requires: vec!["shell".to_owned()],
        provides: vec!["deploy.hotfix.runbook".to_owned()],
        outcome,
        confidence: 0.94,
    }
}

#[test]
fn candidate_id_is_stable_for_same_inputs() {
    let a = input(SkillifyOutcome::Success)
        .into_candidate()
        .expect("candidate");
    let b = input(SkillifyOutcome::Success)
        .into_candidate()
        .expect("candidate");

    assert_eq!(a.candidate_id, b.candidate_id);
    assert_eq!(a.status, SkillifyStatus::Candidate);
    assert!(a.candidate_id.starts_with("skc_"));
}

#[test]
fn candidate_id_frames_source_ids_and_success_criteria_separately() {
    let mut first = input(SkillifyOutcome::Success);
    first.source_record_ids = vec!["a".to_owned()];
    first.success_criteria = vec!["b".to_owned(), "c".to_owned()];

    let mut second = input(SkillifyOutcome::Success);
    second.source_record_ids = vec!["a".to_owned(), "b".to_owned()];
    second.success_criteria = vec!["c".to_owned()];

    let first = first.into_candidate().expect("first candidate");
    let second = second.into_candidate().expect("second candidate");

    assert_ne!(first.candidate_id, second.candidate_id);
}

#[test]
fn failed_trajectory_is_rejected_before_authoring() {
    let err = input(SkillifyOutcome::Failure)
        .into_candidate()
        .expect_err("failure is ineligible");

    assert_eq!(
        err.to_string(),
        "skillify candidate rejected: outcome failure is not eligible for authoring"
    );
}

#[test]
fn unverified_trajectory_is_rejected_before_authoring() {
    let err = input(SkillifyOutcome::Unverified)
        .into_candidate()
        .expect_err("unverified is ineligible");

    assert_eq!(
        err.to_string(),
        "skillify candidate rejected: outcome unverified is not eligible for authoring"
    );
}
