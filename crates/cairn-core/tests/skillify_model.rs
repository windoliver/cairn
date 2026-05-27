#![allow(missing_docs)]

use cairn_core::pipeline::skillify::{
    SkillArtifact, SkillArtifactBundle, SkillArtifactKind, SkillGraphIssueKind, SkillGraphResolver,
    SkillLintIssueKind, SkillLintSkill, SkillLintSnapshot, SkillifyCandidateInput, SkillifyGate,
    SkillifyGateReport, SkillifyGateStatus, SkillifyOutcome, SkillifySource, SkillifyStatus,
    SkillifyTrigger, lint_skill_snapshot,
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
fn empty_artifact_path_is_rejected() {
    let bad = artifact(SkillArtifactKind::DeterministicScript, "");
    let err = bad.validate_path().expect_err("empty path rejected");
    assert_eq!(
        err.to_string(),
        "skillify artifact invalid path ``: path must stay inside the candidate bundle"
    );
}

#[test]
fn artifact_path_must_be_under_bundle_directory() {
    let bad = artifact(
        SkillArtifactKind::DeterministicScript,
        "scripts/deploy-hotfix.sh",
    );
    let err = bad.validate_path().expect_err("non-bundle path rejected");
    assert_eq!(
        err.to_string(),
        "skillify artifact invalid path `scripts/deploy-hotfix.sh`: path must stay inside the candidate bundle"
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
fn skill_lint_skill_graph_metadata_round_trips() {
    let snapshot = SkillLintSnapshot {
        skills: vec![SkillLintSkill {
            skill_id: "deploy-hotfix".to_owned(),
            lane: "deploy.hotfix".to_owned(),
            path: "skills/skill_deploy-hotfix.md".to_owned(),
            uses: Some("skills/scripts/deploy-hotfix.sh".to_owned()),
            resolver_triggers: vec!["deploy hotfix".to_owned()],
            files_to: Some("wiki/summaries/".to_owned()),
            gate_report_passed: true,
            rollback_version_count: 1,
            existing_paths: vec!["skills/scripts/deploy-hotfix.sh".to_owned()],
            requires: vec!["shell.exec".to_owned()],
            provides: vec!["deploy.hotfix".to_owned()],
            conflicts: vec!["deploy.rollback".to_owned()],
        }],
    };

    let yaml = yaml_serde::to_string(&snapshot).expect("serialize");
    assert!(yaml.contains("requires:"));
    assert!(yaml.contains("provides:"));
    assert!(yaml.contains("conflicts:"));

    let parsed: SkillLintSnapshot = yaml_serde::from_str(&yaml).expect("deserialize");
    let parsed_skill = parsed.skills.first().expect("skill");
    assert_eq!(parsed_skill.requires, ["shell.exec"]);
    assert_eq!(parsed_skill.provides, ["deploy.hotfix"]);
    assert_eq!(parsed_skill.conflicts, ["deploy.rollback"]);
}

fn graph_skill(
    skill_id: &str,
    lane: &str,
    requires: &[&str],
    provides: &[&str],
    conflicts: &[&str],
) -> SkillLintSkill {
    SkillLintSkill {
        skill_id: skill_id.to_owned(),
        lane: lane.to_owned(),
        path: format!("skills/skill_{skill_id}.md"),
        uses: None,
        resolver_triggers: vec![format!("run {skill_id}")],
        files_to: Some("wiki/summaries/".to_owned()),
        gate_report_passed: true,
        rollback_version_count: 1,
        existing_paths: vec![],
        requires: requires.iter().map(|s| (*s).to_owned()).collect(),
        provides: provides.iter().map(|s| (*s).to_owned()).collect(),
        conflicts: conflicts.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[test]
fn skill_graph_resolver_orders_transitive_prereqs() {
    let snapshot = SkillLintSnapshot {
        skills: vec![
            graph_skill("run-tests", "test.run", &[], &["cap.test"], &[]),
            graph_skill("lint-diff", "lint.diff", &["cap.test"], &["cap.lint"], &[]),
            graph_skill("ship-pr", "ship.pr", &["cap.lint"], &["cap.ship"], &[]),
        ],
    };

    let resolver = SkillGraphResolver::new(&snapshot);
    let closure = resolver.resolve_prerequisites("ship-pr");

    assert_eq!(closure.prerequisites, ["run-tests", "lint-diff"]);
    assert!(closure.issues.is_empty());
}

#[test]
fn skill_graph_resolver_reports_missing_ambiguous_cycle_and_conflict() {
    let snapshot = SkillLintSnapshot {
        skills: vec![
            graph_skill("a", "lane.a", &["cap.missing"], &["cap.a"], &[]),
            graph_skill("b1", "lane.b1", &[], &["cap.shared"], &[]),
            graph_skill("b2", "lane.b2", &[], &["cap.shared"], &[]),
            graph_skill("c", "lane.c", &["cap.shared"], &["cap.c"], &[]),
            graph_skill(
                "cycle-a",
                "lane.cycle.a",
                &["cycle-b"],
                &["cap.cycle.a"],
                &[],
            ),
            graph_skill(
                "cycle-b",
                "lane.cycle.b",
                &["cycle-a"],
                &["cap.cycle.b"],
                &[],
            ),
            graph_skill(
                "conflict-a",
                "lane.conflict.a",
                &["conflict-b"],
                &["cap.conflict.a"],
                &["conflict-b"],
            ),
            graph_skill(
                "conflict-b",
                "lane.conflict.b",
                &[],
                &["cap.conflict.b"],
                &[],
            ),
        ],
    };

    let resolver = SkillGraphResolver::new(&snapshot);
    let issues = resolver.lint_all();
    let kinds: Vec<_> = issues.iter().map(|issue| issue.kind).collect();

    assert!(kinds.contains(&SkillGraphIssueKind::MissingDependency));
    assert!(kinds.contains(&SkillGraphIssueKind::AmbiguousDependency));
    assert!(kinds.contains(&SkillGraphIssueKind::Cycle));
    assert!(kinds.contains(&SkillGraphIssueKind::Conflict));
}

#[test]
fn skill_graph_resolver_reports_duplicate_lane_ambiguity() {
    let snapshot = SkillLintSnapshot {
        skills: vec![
            graph_skill("root", "lane.root", &["lane.shared"], &[], &[]),
            graph_skill("shared-a", "lane.shared", &[], &[], &[]),
            graph_skill("shared-b", "lane.shared", &[], &[], &[]),
        ],
    };

    let resolver = SkillGraphResolver::new(&snapshot);
    let closure = resolver.resolve_prerequisites("root");

    assert!(closure.issues.iter().any(|issue| {
        issue.kind == SkillGraphIssueKind::AmbiguousDependency
            && issue.skill_id == "root"
            && issue.reference == "lane.shared"
    }));
}

#[test]
fn skill_graph_resolver_reports_prerequisite_declared_conflict_with_root() {
    let snapshot = SkillLintSnapshot {
        skills: vec![
            graph_skill("root", "lane.root", &["leaf"], &[], &[]),
            graph_skill("leaf", "lane.leaf", &[], &[], &["root"]),
        ],
    };

    let resolver = SkillGraphResolver::new(&snapshot);
    let closure = resolver.resolve_prerequisites("root");

    assert_eq!(closure.prerequisites, ["leaf"]);
    assert!(closure.issues.iter().any(|issue| {
        issue.kind == SkillGraphIssueKind::Conflict
            && issue.skill_id == "leaf"
            && issue.reference == "root"
    }));
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
                requires: vec![],
                provides: vec![],
                conflicts: vec![],
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
                requires: vec![],
                provides: vec![],
                conflicts: vec![],
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
                requires: vec![],
                provides: vec![],
                conflicts: vec![],
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
                requires: vec![],
                provides: vec![],
                conflicts: vec![],
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
