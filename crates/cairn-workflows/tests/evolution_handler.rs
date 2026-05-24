#![allow(missing_docs)]

use cairn_core::pipeline::evolution::{
    EvolutionArtifactKind, EvolutionArtifactRef, EvolutionGateKind, EvolutionGateResult,
    EvolutionGateStatus, RollbackPlan,
};
use cairn_workflows::evolution::{
    EvolutionHandler, EvolutionPayload, MaterializedEvolutionDecision,
};
use serde_json::json;
use tempfile::TempDir;

fn artifact(version: u32, digest_byte: char) -> EvolutionArtifactRef {
    EvolutionArtifactRef {
        kind: EvolutionArtifactKind::Skill,
        artifact_id: "skill:deploy-hotfix".to_owned(),
        version,
        content_sha256: format!(
            "sha256:{}",
            std::iter::repeat_n(digest_byte, 64).collect::<String>()
        ),
    }
}

fn rollback_plan() -> RollbackPlan {
    RollbackPlan {
        plan_id: "rollback:deploy-hotfix:v4".to_owned(),
        restores_artifact: artifact(3, 'a'),
        evidence_refs: vec!["eval:rollback-dry-run".to_owned()],
    }
}

fn gate(
    kind: EvolutionGateKind,
    status: EvolutionGateStatus,
    evidence: &str,
) -> EvolutionGateResult {
    EvolutionGateResult {
        kind,
        status,
        message: None,
        evidence_refs: vec![evidence.to_owned()],
    }
}

fn passing_payload(proposal_id: &str) -> EvolutionPayload {
    EvolutionPayload {
        proposal_id: proposal_id.to_owned(),
        previous_artifact: artifact(3, 'a'),
        candidate_artifact: artifact(4, 'b'),
        rollback_plan: rollback_plan(),
        gates: vec![
            gate(
                EvolutionGateKind::Eval,
                EvolutionGateStatus::Passed,
                "eval:main",
            ),
            gate(
                EvolutionGateKind::Privacy,
                EvolutionGateStatus::Passed,
                "privacy:trace",
            ),
            gate(
                EvolutionGateKind::Version,
                EvolutionGateStatus::Passed,
                "version:compat",
            ),
            gate(
                EvolutionGateKind::RollbackPlan,
                EvolutionGateStatus::Passed,
                "rollback:dry-run",
            ),
            gate(
                EvolutionGateKind::Canary,
                EvolutionGateStatus::Passed,
                "canary:24h-pass",
            ),
        ],
        canary_ref: Some("canary:24h-pass".to_owned()),
        canary_failure_ref: None,
        reviewer: Some("hmn:reviewer:v1".to_owned()),
        decision_ref: "decision:approve".to_owned(),
    }
}

#[test]
fn handler_persists_promoted_lineage_and_decision_evidence() {
    let temp = TempDir::new().expect("temp");
    let handler = EvolutionHandler::new(temp.path().to_path_buf());

    let decision = handler
        .run_once(passing_payload("evo_promote"))
        .expect("run");
    assert_eq!(decision, MaterializedEvolutionDecision::Promoted);

    let root = temp.path().join(".cairn/evolution/evolve/evo_promote");
    let lineage: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("lineage.json")).expect("lineage"))
            .expect("lineage json");
    assert_eq!(lineage["proposal_id"], "evo_promote");
    assert_eq!(lineage["previous_artifact"]["version"], 3);
    assert_eq!(lineage["promoted_artifact"]["version"], 4);
    assert_eq!(
        lineage["eval_result_refs"],
        json!(["eval:main", "canary:24h-pass"])
    );
    assert_eq!(
        lineage["decision_evidence"],
        json!(["canary:24h-pass", "decision:approve"])
    );

    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("state.json")).expect("state"))
            .expect("state json");
    assert_eq!(state["state"], "promoted");
}

#[test]
fn handler_rolls_back_and_persists_failed_canary_evidence() {
    let temp = TempDir::new().expect("temp");
    let handler = EvolutionHandler::new(temp.path().to_path_buf());
    let mut payload = passing_payload("evo_rollback");
    payload
        .gates
        .retain(|gate| gate.kind != EvolutionGateKind::Canary);
    payload.canary_ref = Some("canary:5pct".to_owned());
    payload.canary_failure_ref = Some("slo:latency-regression".to_owned());

    let decision = handler.run_once(payload).expect("run");
    assert_eq!(decision, MaterializedEvolutionDecision::RolledBack);

    let root = temp.path().join(".cairn/evolution/evolve/evo_rollback");
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("state.json")).expect("state"))
            .expect("state json");
    assert_eq!(state["state"], "rolled_back");
    assert_eq!(state["active_artifact"]["version"], 3);
    assert_eq!(
        state["decision_evidence"],
        json!(["canary:5pct", "slo:latency-regression"])
    );
}

#[test]
fn handler_rejects_gate_failure_without_promoting_candidate() {
    let temp = TempDir::new().expect("temp");
    let handler = EvolutionHandler::new(temp.path().to_path_buf());
    let mut payload = passing_payload("evo_rejected");
    payload
        .gates
        .retain(|gate| gate.kind != EvolutionGateKind::Privacy);
    payload.gates.push(gate(
        EvolutionGateKind::Privacy,
        EvolutionGateStatus::Failed,
        "privacy:raw-body-leak",
    ));

    let decision = handler.run_once(payload).expect("run");
    assert_eq!(decision, MaterializedEvolutionDecision::Rejected);

    let root = temp.path().join(".cairn/evolution/evolve/evo_rejected");
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("state.json")).expect("state"))
            .expect("state json");
    assert_eq!(state["state"], "rejected");
    assert_eq!(state["active_artifact"]["version"], 3);
    assert_eq!(state["promoted_artifact"], serde_json::Value::Null);
}
