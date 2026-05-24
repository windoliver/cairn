//! CLI E2E coverage for `cairn admin workflow run-evolution`.
//!
//! This drives the real `cairn` binary against a bootstrapped vault and
//! verifies the evolution workflow's persisted audit files, not just the
//! in-process handler.

use assert_cmd::Command;
use serde_json::json;
use std::path::Path;

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn artifact(version: u32, digest_byte: char) -> serde_json::Value {
    json!({
        "kind": "skill",
        "artifact_id": "skill:deploy-hotfix",
        "version": version,
        "content_sha256": format!(
            "sha256:{}",
            std::iter::repeat_n(digest_byte, 64).collect::<String>()
        )
    })
}

fn gate(kind: &str, status: &str, evidence: &str) -> serde_json::Value {
    json!({
        "kind": kind,
        "status": status,
        "evidence_refs": [evidence]
    })
}

fn passing_payload(proposal_id: &str) -> serde_json::Value {
    json!({
        "proposal_id": proposal_id,
        "previous_artifact": artifact(3, 'a'),
        "candidate_artifact": artifact(4, 'b'),
        "rollback_plan": {
            "plan_id": "rollback:deploy-hotfix:v4",
            "restores_artifact": artifact(3, 'a'),
            "evidence_refs": ["eval:rollback-dry-run"]
        },
        "gates": [
            gate("eval", "passed", "eval:main"),
            gate("privacy", "passed", "privacy:trace"),
            gate("version", "passed", "version:compat"),
            gate("rollback_plan", "passed", "rollback:dry-run"),
            gate("canary", "passed", "canary:24h-pass")
        ],
        "canary_ref": "canary:24h-pass",
        "reviewer": "hmn:reviewer:v1",
        "decision_ref": "decision:approve"
    })
}

fn run_evolution(vault: &Path, payload_path: &Path) -> std::process::Output {
    Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", vault)
        .env_remove("CAIRN_REGISTRY")
        .args([
            "admin",
            "workflow",
            "run-evolution",
            "--payload",
            payload_path.to_str().expect("payload path utf-8"),
        ])
        .output()
        .expect("run cli")
}

#[test]
fn run_evolution_promotes_and_rolls_back_from_cli_payloads() {
    let temp = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(temp.path());

    let promote_path = temp.path().join("promote.json");
    std::fs::write(
        &promote_path,
        serde_json::to_vec_pretty(&passing_payload("evo_cli_promote")).expect("payload json"),
    )
    .expect("write promote payload");

    let promote = run_evolution(temp.path(), &promote_path);
    assert!(
        promote.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&promote.stdout),
        String::from_utf8_lossy(&promote.stderr)
    );
    assert!(
        String::from_utf8_lossy(&promote.stdout).contains("decision=promoted"),
        "stdout should report promoted decision: {}",
        String::from_utf8_lossy(&promote.stdout)
    );

    let promote_root = temp.path().join(".cairn/evolution/evolve/evo_cli_promote");
    let lineage: serde_json::Value =
        serde_json::from_slice(&std::fs::read(promote_root.join("lineage.json")).expect("lineage"))
            .expect("lineage json");
    assert_eq!(lineage["previous_artifact"]["version"], 3);
    assert_eq!(lineage["promoted_artifact"]["version"], 4);
    assert_eq!(
        lineage["eval_result_refs"],
        json!(["eval:main", "canary:24h-pass"])
    );

    let rollback_path = temp.path().join("rollback.json");
    let mut rollback_payload = passing_payload("evo_cli_rollback");
    rollback_payload["gates"] = json!([
        gate("eval", "passed", "eval:main"),
        gate("privacy", "passed", "privacy:trace"),
        gate("version", "passed", "version:compat"),
        gate("rollback_plan", "passed", "rollback:dry-run")
    ]);
    rollback_payload["canary_ref"] = json!("canary:5pct");
    rollback_payload["canary_failure_ref"] = json!("slo:latency-regression");
    std::fs::write(
        &rollback_path,
        serde_json::to_vec_pretty(&rollback_payload).expect("payload json"),
    )
    .expect("write rollback payload");

    let rollback = run_evolution(temp.path(), &rollback_path);
    assert!(
        rollback.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&rollback.stdout),
        String::from_utf8_lossy(&rollback.stderr)
    );
    assert!(
        String::from_utf8_lossy(&rollback.stdout).contains("decision=rolled_back"),
        "stdout should report rolled_back decision: {}",
        String::from_utf8_lossy(&rollback.stdout)
    );

    let rollback_root = temp.path().join(".cairn/evolution/evolve/evo_cli_rollback");
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(rollback_root.join("state.json")).expect("state"))
            .expect("state json");
    assert_eq!(state["state"], "rolled_back");
    assert_eq!(state["active_artifact"]["version"], 3);
    assert_eq!(
        state["decision_evidence"],
        json!(["canary:5pct", "slo:latency-regression"])
    );
}
