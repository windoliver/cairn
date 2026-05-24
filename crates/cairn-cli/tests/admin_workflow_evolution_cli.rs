//! CLI E2E coverage for `cairn admin workflow run-evolution`.
//!
//! This drives the real `cairn` binary against a bootstrapped vault and
//! verifies the evolution workflow's persisted audit files, not just the
//! in-process handler.

use assert_cmd::Command;
use cairn_core::contract::job_store::{EnqueueRequest, JobId, JobKind, JobStore, RetryPolicy};
use cairn_workflows::{EVOLUTION_KIND, SqliteJobStore};
use serde_json::json;
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn enable_mcp_single_tenant(vault: &Path) {
    let config_path = vault.join(".cairn/config.yaml");
    let mut cfg = std::fs::read_to_string(&config_path).expect("read config");
    if let Some(idx) = cfg.find("\nmcp:") {
        cfg.truncate(idx);
    }
    cfg.push_str(
        "\nmcp:\n  stdio:\n    single_tenant: true\n    principal:\n      tenant: evolution-e2e\n",
    );
    std::fs::write(&config_path, cfg).expect("write config");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_scheduler_drains_queued_evolution_job() {
    let temp = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(temp.path());
    enable_mcp_single_tenant(temp.path());

    let db_path = temp.path().join(".cairn/cairn.db");
    let jobs_conn = cairn_store_sqlite::open_sync(&db_path).expect("open jobs db");
    let jobs = SqliteJobStore::new(jobs_conn).expect("jobs");
    jobs.enqueue(EnqueueRequest {
        job_id: JobId::new("evo-cli-mcp-scheduler"),
        kind: JobKind::new(EVOLUTION_KIND),
        payload: serde_json::to_vec(&passing_payload("evo_cli_mcp_scheduler"))
            .expect("payload json"),
        queue_key: None,
        dedupe_key: None,
        not_before_ms: 0,
        retry: RetryPolicy::DEFAULT,
    })
    .await
    .expect("enqueue evolution job");

    let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_cairn"))
        .arg("--vault")
        .arg(temp.path())
        .arg("mcp")
        .env_remove("CAIRN_VAULT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cairn mcp");
    let child_stdin = child.stdin.take().expect("stdin pipe");

    let state_path = temp
        .path()
        .join(".cairn/evolution/evolve/evo_cli_mcp_scheduler/state.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !state_path.exists() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    drop(child_stdin);
    let shutdown_deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < shutdown_deadline {
        match child.try_wait() {
            Ok(None) => tokio::time::sleep(Duration::from_millis(50)).await,
            Ok(Some(_)) | Err(_) => break,
        }
    }
    if child.try_wait().expect("try wait").is_none() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("cairn mcp did not exit after stdin EOF");
    }

    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).expect("state materialized"))
            .expect("state json");
    assert_eq!(state["state"], "promoted");
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
