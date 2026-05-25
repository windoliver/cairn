#![allow(missing_docs)]

use cairn_core::pipeline::skillify::{
    SkillArtifact, SkillArtifactBundle, SkillArtifactKind, SkillSpecDraft, SkillifyGate,
    SkillifyGateStatus, SkillifyPipelineState, SkillifyStage, SkillifyStageError,
};

fn draft() -> SkillSpecDraft {
    SkillSpecDraft {
        lane: "deploy.hotfix".to_owned(),
        slug: "deploy-hotfix".to_owned(),
        decision_tree: serde_json::json!({"root": "check_env"}),
        triggers: vec!["deploy hotfix".to_owned()],
        success_criteria: vec!["script exits 0".to_owned()],
        source_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
        requires: vec![],
        provides: vec!["deploy.hotfix".to_owned()],
    }
}

fn passing_gates() -> Vec<SkillifyGate> {
    SkillArtifactKind::required()
        .iter()
        .map(|kind| SkillifyGate {
            name: kind.as_str().to_owned(),
            status: SkillifyGateStatus::Passed,
            message: None,
        })
        .collect()
}

fn valid_bundle() -> SkillArtifactBundle {
    SkillArtifactBundle {
        candidate_id: "skc_test".to_owned(),
        version: 1,
        artifacts: SkillArtifactKind::required()
            .iter()
            .map(|kind| SkillArtifact {
                kind: *kind,
                path: kind.default_relative_path("deploy-hotfix"),
                content_sha256: "sha256:aaaa".to_owned(),
                evidence_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
                status: "generated".to_owned(),
            })
            .collect(),
    }
}

#[test]
fn new_state_starts_at_extract() {
    let state = SkillifyPipelineState::new("skc_test".to_owned());
    assert_eq!(state.stage(), SkillifyStage::Extract);
}

#[test]
fn happy_path_extract_to_promote() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    state.advance_to_author(draft()).unwrap();
    assert_eq!(state.stage(), SkillifyStage::Author);
    state.advance_to_gate(valid_bundle()).unwrap();
    assert_eq!(state.stage(), SkillifyStage::Gate);
    for gate in passing_gates() {
        state.record_gate(gate);
    }
    state.advance_to_promote().unwrap();
    assert_eq!(state.stage(), SkillifyStage::Promote);
    state.advance_to_health("plan_ref_001".to_owned()).unwrap();
    assert_eq!(state.stage(), SkillifyStage::HealthCheck);
}

#[test]
fn cannot_skip_from_extract_to_gate() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    let err = state.advance_to_gate(valid_bundle()).unwrap_err();
    assert!(matches!(err, SkillifyStageError::InvalidTransition { .. }));
}

#[test]
fn cannot_skip_from_extract_to_promote() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    let err = state.advance_to_promote().unwrap_err();
    assert!(matches!(err, SkillifyStageError::InvalidTransition { .. }));
}

#[test]
fn promote_fails_without_gates() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    state.advance_to_author(draft()).unwrap();
    state.advance_to_gate(valid_bundle()).unwrap();
    let err = state.advance_to_promote().unwrap_err();
    assert!(matches!(err, SkillifyStageError::GatesNotSatisfied { .. }));
}

#[test]
fn promote_fails_with_failing_gate() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    state.advance_to_author(draft()).unwrap();
    state.advance_to_gate(valid_bundle()).unwrap();
    let mut gates = passing_gates();
    gates[0].status = SkillifyGateStatus::Failed;
    for gate in gates {
        state.record_gate(gate);
    }
    let err = state.advance_to_promote().unwrap_err();
    assert!(matches!(err, SkillifyStageError::GatesNotSatisfied { .. }));
}

#[test]
fn fail_from_any_non_terminal() {
    for start_stage in [
        SkillifyStage::Extract,
        SkillifyStage::Author,
        SkillifyStage::Gate,
    ] {
        let mut state = SkillifyPipelineState::new("skc_test".to_owned());
        match start_stage {
            SkillifyStage::Author => {
                state.advance_to_author(draft()).unwrap();
            }
            SkillifyStage::Gate => {
                state.advance_to_author(draft()).unwrap();
                state.advance_to_gate(valid_bundle()).unwrap();
            }
            _ => {}
        }
        state.fail("test failure".to_owned()).unwrap();
        assert_eq!(state.stage(), SkillifyStage::Failed);
    }
}

#[test]
fn block_from_any_non_terminal() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    state.block("no llm".to_owned()).unwrap();
    assert_eq!(state.stage(), SkillifyStage::Blocked);
}

#[test]
fn cannot_transition_from_failed() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    state.fail("done".to_owned()).unwrap();
    let err = state.advance_to_author(draft()).unwrap_err();
    assert!(matches!(err, SkillifyStageError::InvalidTransition { .. }));
}

#[test]
fn cannot_transition_from_blocked() {
    let mut state = SkillifyPipelineState::new("skc_test".to_owned());
    state.block("done".to_owned()).unwrap();
    let err = state.fail("double".to_owned()).unwrap_err();
    assert!(matches!(err, SkillifyStageError::InvalidTransition { .. }));
}

#[test]
fn serde_stage_round_trip() {
    let json = serde_json::to_string(&SkillifyStage::Gate).unwrap();
    assert_eq!(json, "\"gate\"");
    let parsed: SkillifyStage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, SkillifyStage::Gate);
}
