#![allow(missing_docs)]

use cairn_core::pipeline::skillify::{
    SkillArtifact, SkillArtifactBundle, SkillArtifactKind, SkillLintIssueKind, SkillLintSkill,
    SkillLintSnapshot, SkillifyCandidateInput, SkillifyGate, SkillifyGateReport,
    SkillifyGateStatus, SkillifyOutcome, SkillifySource, SkillifyStatus, SkillifyTrigger,
    lint_skill_snapshot,
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

fn artifact(kind: SkillArtifactKind, path: &str) -> SkillArtifact {
    SkillArtifact {
        kind,
        path: path.to_owned(),
        content_sha256: "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
            .to_owned(),
        evidence_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
        status: "generated".to_owned(),
    }
}

#[test]
fn complete_bundle_has_all_ten_artifacts() {
    let bundle = SkillArtifactBundle {
        candidate_id: "skc_fixture".to_owned(),
        version: 1,
        artifacts: SkillArtifactKind::required()
            .iter()
            .map(|kind| artifact(*kind, &kind.default_relative_path("deploy-hotfix")))
            .collect(),
    };

    bundle.validate().expect("complete bundle");
}

#[test]
fn missing_artifact_blocks_bundle_validation() {
    let bundle = SkillArtifactBundle {
        candidate_id: "skc_fixture".to_owned(),
        version: 1,
        artifacts: vec![artifact(
            SkillArtifactKind::SkillContract,
            "bundle/skills/skill_deploy.md",
        )],
    };

    let err = bundle.validate().expect_err("missing artifacts");
    assert!(
        err.to_string()
            .contains("missing artifact deterministic_script")
    );
}

#[test]
fn path_escape_is_rejected() {
    let bad = artifact(SkillArtifactKind::DeterministicScript, "../escape.sh");
    let err = bad.validate_path().expect_err("escape rejected");
    assert_eq!(
        err.to_string(),
        "skillify artifact invalid path `../escape.sh`: path must stay inside the candidate bundle"
    );
}

#[test]
fn gate_report_requires_every_gate_passed() {
    let report = SkillifyGateReport {
        candidate_id: "skc_fixture".to_owned(),
        gates: vec![
            SkillifyGate {
                name: "skill_contract".to_owned(),
                status: SkillifyGateStatus::Passed,
                message: None,
            },
            SkillifyGate {
                name: "unit_tests".to_owned(),
                status: SkillifyGateStatus::Failed,
                message: Some("test failed".to_owned()),
            },
        ],
    };

    assert!(!report.ready_for_promotion());
}

#[test]
fn gate_report_with_single_passed_gate_is_not_ready() {
    let report = SkillifyGateReport {
        candidate_id: "skc_fixture".to_owned(),
        gates: vec![SkillifyGate {
            name: "skill_contract".to_owned(),
            status: SkillifyGateStatus::Passed,
            message: None,
        }],
    };

    assert!(!report.ready_for_promotion());
}

#[test]
fn gate_report_with_all_required_gates_passed_is_ready() {
    let report = SkillifyGateReport {
        candidate_id: "skc_fixture".to_owned(),
        gates: SkillArtifactKind::required()
            .iter()
            .map(|kind| SkillifyGate {
                name: kind.as_str().to_owned(),
                status: SkillifyGateStatus::Passed,
                message: None,
            })
            .collect(),
    };

    assert!(report.ready_for_promotion());
}

#[test]
fn lint_reports_missing_script_and_duplicate_lane() {
    let snapshot = SkillLintSnapshot {
        skills: vec![
            SkillLintSkill {
                skill_id: "skill-a".to_owned(),
                lane: "deploy.hotfix".to_owned(),
                path: "skills/skill_a.md".to_owned(),
                uses: Some("skills/scripts/missing.sh".to_owned()),
                resolver_triggers: vec!["deploy hotfix".to_owned()],
                files_to: Some("wiki/summaries/".to_owned()),
                gate_report_passed: true,
                rollback_version_count: 1,
                existing_paths: vec!["skills/skill_a.md".to_owned()],
            },
            SkillLintSkill {
                skill_id: "skill-b".to_owned(),
                lane: "deploy.hotfix".to_owned(),
                path: "skills/skill_b.md".to_owned(),
                uses: Some("skills/scripts/b.sh".to_owned()),
                resolver_triggers: vec!["ship hotfix".to_owned()],
                files_to: Some("wiki/summaries/".to_owned()),
                gate_report_passed: true,
                rollback_version_count: 1,
                existing_paths: vec![
                    "skills/skill_b.md".to_owned(),
                    "skills/scripts/b.sh".to_owned(),
                ],
            },
        ],
    };

    let findings = lint_skill_snapshot(&snapshot);
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == SkillLintIssueKind::MissingArtifact)
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == SkillLintIssueKind::DuplicateLane)
    );
}

#[test]
fn lint_reports_invalid_filing_rules_and_resolver_triggers() {
    let snapshot = SkillLintSnapshot {
        skills: vec![
            SkillLintSkill {
                skill_id: "skill-b".to_owned(),
                lane: "deploy.rollback".to_owned(),
                path: "skills/skill_b.md".to_owned(),
                uses: None,
                resolver_triggers: vec!["deploy now".to_owned()],
                files_to: Some("../../outside".to_owned()),
                gate_report_passed: true,
                rollback_version_count: 1,
                existing_paths: vec!["skills/skill_b.md".to_owned()],
            },
            SkillLintSkill {
                skill_id: "skill-a".to_owned(),
                lane: "deploy.hotfix".to_owned(),
                path: "skills/skill_a.md".to_owned(),
                uses: None,
                resolver_triggers: vec!["   ".to_owned(), "deploy now".to_owned()],
                files_to: Some("wiki/summaries/".to_owned()),
                gate_report_passed: true,
                rollback_version_count: 1,
                existing_paths: vec!["skills/skill_a.md".to_owned()],
            },
        ],
    };

    let findings = lint_skill_snapshot(&snapshot);

    assert!(findings.iter().any(
        |finding| finding.kind == SkillLintIssueKind::MissingArtifact
            && finding.message.contains("invalid files_to")
    ));
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == SkillLintIssueKind::Unreachable
                && finding.message.contains("blank resolver trigger"))
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == SkillLintIssueKind::DuplicateLane
                && finding.message.contains("resolver trigger `deploy now`"))
    );
    assert_eq!(findings[0].skill_id, "skill-a");
}
