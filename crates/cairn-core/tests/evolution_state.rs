#![allow(missing_docs)]

use cairn_core::pipeline::evolution::{
    EvolutionArtifactKind, EvolutionArtifactRef, EvolutionGateKind, EvolutionGateReport,
    EvolutionGateResult, EvolutionGateStatus, EvolutionRun, EvolutionState,
    EvolutionTransitionError, RollbackPlan,
};
use cairn_core::wal::{WalKind, graph_for};

fn previous_skill() -> EvolutionArtifactRef {
    EvolutionArtifactRef {
        kind: EvolutionArtifactKind::Skill,
        artifact_id: "skill:deploy-hotfix".to_owned(),
        version: 3,
        content_sha256: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_owned(),
    }
}

fn candidate_skill() -> EvolutionArtifactRef {
    EvolutionArtifactRef {
        kind: EvolutionArtifactKind::Skill,
        artifact_id: "skill:deploy-hotfix".to_owned(),
        version: 4,
        content_sha256: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            .to_owned(),
    }
}

fn rollback_plan() -> RollbackPlan {
    RollbackPlan {
        plan_id: "rollback:deploy-hotfix:v4".to_owned(),
        restores_artifact: previous_skill(),
        evidence_refs: vec!["eval:rollback-dry-run".to_owned()],
    }
}

fn passing_gate(kind: EvolutionGateKind, evidence: &str) -> EvolutionGateResult {
    EvolutionGateResult {
        kind,
        status: EvolutionGateStatus::Passed,
        message: None,
        evidence_refs: vec![evidence.to_owned()],
    }
}

#[test]
fn promotion_requires_eval_privacy_version_and_rollback_gates() {
    let mut run = EvolutionRun::new(
        "evo_proposal_1",
        previous_skill(),
        candidate_skill(),
        rollback_plan(),
    )
    .expect("valid run");

    run.record_gate(passing_gate(EvolutionGateKind::Eval, "eval:main"));
    run.record_gate(passing_gate(EvolutionGateKind::Privacy, "privacy:trace"));
    run.record_gate(passing_gate(EvolutionGateKind::Version, "version:compat"));

    let err = run
        .promote("hmn:reviewer:v1", "decision:promote")
        .expect_err("missing rollback and canary gates must block promotion");

    match err {
        EvolutionTransitionError::PromotionBlocked { missing, failed } => {
            assert!(failed.is_empty());
            assert!(missing.contains(&EvolutionGateKind::RollbackPlan));
            assert!(missing.contains(&EvolutionGateKind::Canary));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(run.state(), EvolutionState::Proposed);
}

#[test]
fn canary_failure_rolls_back_to_previous_artifact_and_records_evidence() {
    let mut run = EvolutionRun::new(
        "evo_proposal_2",
        previous_skill(),
        candidate_skill(),
        rollback_plan(),
    )
    .expect("valid run");
    run.record_gate(passing_gate(EvolutionGateKind::Eval, "eval:main"));
    run.record_gate(passing_gate(EvolutionGateKind::Privacy, "privacy:trace"));
    run.record_gate(passing_gate(EvolutionGateKind::Version, "version:compat"));
    run.record_gate(passing_gate(
        EvolutionGateKind::RollbackPlan,
        "rollback:dry-run",
    ));

    run.start_canary("canary:5pct").expect("start canary");
    run.fail_canary("slo:latency-regression")
        .expect("rollback canary");

    assert_eq!(run.state(), EvolutionState::RolledBack);
    assert_eq!(run.active_artifact(), &previous_skill());
    assert_eq!(
        run.decision_evidence(),
        &[
            "canary:5pct".to_owned(),
            "slo:latency-regression".to_owned()
        ]
    );
}

#[test]
fn promoted_lineage_links_old_proposal_eval_and_promoted_artifacts() {
    let mut run = EvolutionRun::new(
        "evo_proposal_3",
        previous_skill(),
        candidate_skill(),
        rollback_plan(),
    )
    .expect("valid run");
    let report = EvolutionGateReport {
        gates: vec![
            passing_gate(EvolutionGateKind::Eval, "eval:main"),
            passing_gate(EvolutionGateKind::Privacy, "privacy:trace"),
            passing_gate(EvolutionGateKind::Version, "version:compat"),
            passing_gate(EvolutionGateKind::RollbackPlan, "rollback:dry-run"),
            passing_gate(EvolutionGateKind::Canary, "canary:24h-pass"),
        ],
    };
    run.extend_gates(report);

    run.promote("hmn:reviewer:v1", "decision:approve")
        .expect("promote");
    let lineage = run.lineage();

    assert_eq!(lineage.previous_artifact, previous_skill());
    assert_eq!(lineage.proposal_id, "evo_proposal_3");
    assert_eq!(
        lineage.eval_result_refs,
        vec!["eval:main".to_owned(), "canary:24h-pass".to_owned()]
    );
    assert_eq!(lineage.promoted_artifact.as_ref(), Some(&candidate_skill()));
    assert!(
        lineage
            .decision_evidence
            .contains(&"decision:approve".to_owned())
    );
}

#[test]
fn failed_configured_optional_gate_blocks_promotion() {
    let mut run = EvolutionRun::new(
        "evo_proposal_4",
        previous_skill(),
        candidate_skill(),
        rollback_plan(),
    )
    .expect("valid run");
    let report = EvolutionGateReport {
        gates: vec![
            passing_gate(EvolutionGateKind::Eval, "eval:main"),
            passing_gate(EvolutionGateKind::Privacy, "privacy:trace"),
            passing_gate(EvolutionGateKind::Version, "version:compat"),
            passing_gate(EvolutionGateKind::RollbackPlan, "rollback:dry-run"),
            passing_gate(EvolutionGateKind::Canary, "canary:24h-pass"),
            EvolutionGateResult {
                kind: EvolutionGateKind::Review,
                status: EvolutionGateStatus::Failed,
                message: Some("human review rejected the candidate".to_owned()),
                evidence_refs: vec!["review:reject".to_owned()],
            },
        ],
    };
    run.extend_gates(report);

    let err = run
        .promote("hmn:reviewer:v1", "decision:approve")
        .expect_err("failed configured review gate must block promotion");

    match err {
        EvolutionTransitionError::PromotionBlocked { missing, failed } => {
            assert!(missing.is_empty());
            assert_eq!(failed, vec![EvolutionGateKind::Review]);
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(run.state(), EvolutionState::Proposed);
}

#[test]
fn evolve_wal_graph_has_gated_canary_promotion_steps() {
    let graph = graph_for(WalKind::Evolve);
    let names: Vec<&str> = graph.steps.iter().map(|step| step.name).collect();

    assert_eq!(
        names,
        vec![
            "proposal.stage",
            "eval.run",
            "gates.verify",
            "canary.start",
            "canary.observe",
            "artifact.promote",
        ]
    );
    assert!(
        graph
            .steps
            .iter()
            .all(|step| step.name == "proposal.stage" || step.idempotent),
        "all retryable side-effect steps after proposal staging must be idempotent"
    );
}
