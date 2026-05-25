#![allow(missing_docs)]

use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::pipeline::skillify::{
    SkillArtifact, SkillArtifactBundle, SkillArtifactKind, SkillLintSkill, SkillLintSnapshot,
    SkillifyGateStatus,
};
use cairn_workflows::skillify::gate_runner::{
    CheckResolvableAndDryRunner, DeterministicScriptRunner, E2eSmokeRunner, FilingRulesRunner,
    GateRunContext, GateRunner, IntegrationTestRunner, LlmEvalRunner, ResolverEvalRunner,
    ResolverTriggerRunner, SkillContractRunner, UnitTestRunner,
};
use cairn_workflows::skillify::materialize::AuthoredSkillBundle;
use serde_json::json;
use tempfile::TempDir;

fn authored(slug: &str) -> AuthoredSkillBundle {
    AuthoredSkillBundle {
        lane: "deploy.hotfix".to_owned(),
        slug: slug.to_owned(),
        skill_markdown: format!(
            "---\nname: {slug}\nlane: deploy.hotfix\ntriggers:\n  - deploy hotfix\nuses: scripts/{slug}.sh\nfiles_to: wiki/summaries/\n---\nRun the script."
        ),
        script: format!("#!/usr/bin/env bash\nset -euo pipefail\necho {slug}\n"),
        unit_tests: json!({"cases": [{"input": "", "expected_stdout": format!("{slug}\n"), "timeout_ms": 5000}]}),
        integration_tests: json!({"cases": [{"input": "", "expected_stdout": format!("{slug}\n"), "timeout_ms": 10000}]}),
        llm_evals: json!({"rubric": [{"prompt": "deploy hotfix", "expected_behavior": "calls the script", "scoring_criteria": "script invoked"}]}),
        resolver_triggers: json!(["deploy hotfix"]),
        resolver_eval: json!({"intents": [
            {"intent": "deploy hotfix", "expected_lane": "deploy.hotfix"},
            {"intent": "restart api server", "expected_lane": "ops.restart"},
        ]}),
        smoke: json!({"cases": [{"trigger_phrase": "deploy hotfix", "expected_output": format!("{slug}\n")}]}),
        filing_rules: json!({"files_to": "wiki/summaries/"}),
    }
}

fn bundle(slug: &str) -> SkillArtifactBundle {
    SkillArtifactBundle {
        candidate_id: "skc_test".to_owned(),
        version: 1,
        artifacts: SkillArtifactKind::required()
            .iter()
            .map(|kind| SkillArtifact {
                kind: *kind,
                path: kind.default_relative_path(slug),
                content_sha256: "sha256:aaaa".to_owned(),
                evidence_refs: vec![],
                status: "generated".to_owned(),
            })
            .collect(),
    }
}

fn empty_snapshot() -> SkillLintSnapshot {
    SkillLintSnapshot { skills: vec![] }
}

fn materialize_script(dir: &std::path::Path, slug: &str, content: &str) {
    let scripts_dir = dir.join("bundle/scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    let script_path = scripts_dir.join(format!("{slug}.sh"));
    std::fs::write(&script_path, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

// -- SkillContractRunner --

#[tokio::test]
async fn skill_contract_passes_valid_markdown() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = SkillContractRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Passed);
}

#[tokio::test]
async fn skill_contract_fails_missing_lane() {
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    a.skill_markdown = "---\nname: test\n---\nNo lane.".to_owned();
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = SkillContractRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    assert!(result.message.unwrap().contains("lane"));
}

// -- DeterministicScriptRunner --

#[tokio::test]
async fn script_runner_passes_valid_script() {
    let temp = TempDir::new().unwrap();
    materialize_script(
        temp.path(),
        "deploy-hotfix",
        "#!/usr/bin/env bash\necho ok\n",
    );
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = DeterministicScriptRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Passed);
}

#[tokio::test]
async fn script_runner_fails_missing_shebang() {
    let temp = TempDir::new().unwrap();
    materialize_script(temp.path(), "deploy-hotfix", "echo no shebang\n");
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = DeterministicScriptRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
}

// -- FilingRulesRunner --

#[tokio::test]
async fn filing_rules_passes_valid_path() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = FilingRulesRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Passed);
}

#[tokio::test]
async fn filing_rules_fails_absolute_path() {
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    a.filing_rules = json!({"files_to": "/etc/passwd/"});
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = FilingRulesRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
}

// -- ResolverTriggerRunner --

#[tokio::test]
async fn resolver_trigger_passes_valid_triggers() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = ResolverTriggerRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Passed);
}

#[tokio::test]
async fn resolver_trigger_fails_collision_with_snapshot() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let snapshot = SkillLintSnapshot {
        skills: vec![SkillLintSkill {
            skill_id: "existing".to_owned(),
            lane: "other.lane".to_owned(),
            path: "skills/skill_existing.md".to_owned(),
            uses: None,
            resolver_triggers: vec!["deploy hotfix".to_owned()],
            files_to: Some("wiki/".to_owned()),
            gate_report_passed: true,
            rollback_version_count: 1,
            existing_paths: vec![],
        }],
    };
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &snapshot,
    };
    let result = ResolverTriggerRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
}

// -- CheckResolvableAndDryRunner --

#[tokio::test]
async fn check_resolvable_passes_no_conflicts() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = CheckResolvableAndDryRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Passed);
}

// -- UnitTestRunner --

#[tokio::test]
async fn unit_test_runner_passes_matching_output() {
    let temp = TempDir::new().unwrap();
    materialize_script(
        temp.path(),
        "deploy-hotfix",
        "#!/usr/bin/env bash\necho deploy-hotfix\n",
    );
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = UnitTestRunner.run(&ctx).await;
    assert_eq!(
        result.status,
        SkillifyGateStatus::Passed,
        "{:?}",
        result.message
    );
}

#[tokio::test]
async fn unit_test_runner_fails_on_stdout_mismatch() {
    let temp = TempDir::new().unwrap();
    materialize_script(
        temp.path(),
        "deploy-hotfix",
        "#!/usr/bin/env bash\necho wrong\n",
    );
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = UnitTestRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
}

#[tokio::test]
async fn unit_test_runner_blocked_when_cases_missing() {
    let temp = TempDir::new().unwrap();
    materialize_script(
        temp.path(),
        "deploy-hotfix",
        "#!/usr/bin/env bash\necho ok\n",
    );
    let mut a = authored("deploy-hotfix");
    a.unit_tests = serde_json::json!({});
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = UnitTestRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Blocked);
}

// -- IntegrationTestRunner --

#[tokio::test]
async fn integration_test_runner_passes_matching_output() {
    let temp = TempDir::new().unwrap();
    materialize_script(
        temp.path(),
        "deploy-hotfix",
        "#!/usr/bin/env bash\necho deploy-hotfix\n",
    );
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = IntegrationTestRunner.run(&ctx).await;
    assert_eq!(
        result.status,
        SkillifyGateStatus::Passed,
        "{:?}",
        result.message
    );
}

#[tokio::test]
async fn integration_test_runner_sees_cairn_integration_env() {
    // Script echoes value of CAIRN_INTEGRATION env var; expects "1"
    let temp = TempDir::new().unwrap();
    materialize_script(
        temp.path(),
        "deploy-hotfix",
        "#!/usr/bin/env bash\necho ${CAIRN_INTEGRATION:-unset}\n",
    );
    let mut a = authored("deploy-hotfix");
    a.integration_tests = serde_json::json!({
        "cases": [{"input": "", "expected_stdout": "1\n", "timeout_ms": 5000}]
    });
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = IntegrationTestRunner.run(&ctx).await;
    assert_eq!(
        result.status,
        SkillifyGateStatus::Passed,
        "{:?}",
        result.message
    );
}

// -- LlmEvalRunner --

struct PassLlm;
struct FailLlm;

#[async_trait::async_trait]
impl LLMProvider for PassLlm {
    fn name(&self) -> &'static str {
        "pass-llm"
    }
    fn capabilities(&self) -> &LLMProviderCapabilities {
        static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
            json_mode: true,
            streaming: false,
            tool_calls: false,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        Ok(CompletionOutput::Json(
            serde_json::json!({"pass": true, "reason": "ok"}),
        ))
    }
}

#[async_trait::async_trait]
impl LLMProvider for FailLlm {
    fn name(&self) -> &'static str {
        "fail-llm"
    }
    fn capabilities(&self) -> &LLMProviderCapabilities {
        static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
            json_mode: true,
            streaming: false,
            tool_calls: false,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        Ok(CompletionOutput::Json(
            serde_json::json!({"pass": false, "reason": "bad"}),
        ))
    }
}

#[tokio::test]
async fn llm_eval_runner_passes_when_judge_returns_pass() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let llm = PassLlm;
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: Some(&llm),
        snapshot: &empty_snapshot(),
    };
    let result = LlmEvalRunner.run(&ctx).await;
    assert_eq!(
        result.status,
        SkillifyGateStatus::Passed,
        "{:?}",
        result.message
    );
}

#[tokio::test]
async fn llm_eval_runner_fails_when_judge_returns_fail() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let llm = FailLlm;
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: Some(&llm),
        snapshot: &empty_snapshot(),
    };
    let result = LlmEvalRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
}

#[tokio::test]
async fn llm_eval_runner_blocked_without_llm() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = LlmEvalRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Blocked);
}

// -- ResolverEvalRunner --

#[tokio::test]
async fn resolver_eval_passes_with_matching_intents() {
    let temp = TempDir::new().unwrap();
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = ResolverEvalRunner.run(&ctx).await;
    assert_eq!(
        result.status,
        SkillifyGateStatus::Passed,
        "{:?}",
        result.message
    );
}

#[tokio::test]
async fn resolver_eval_fails_when_recall_too_low() {
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    // Intents that don't match any trigger
    a.resolver_eval = serde_json::json!({
        "intents": [
            {"intent": "completely unrelated phrase xyz", "expected_lane": "deploy.hotfix"},
            {"intent": "another unrelated phrase abc", "expected_lane": "deploy.hotfix"},
        ]
    });
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = ResolverEvalRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
}

// -- E2eSmokeRunner --

#[tokio::test]
async fn e2e_smoke_runner_passes_when_script_output_matches() {
    let temp = TempDir::new().unwrap();
    materialize_script(
        temp.path(),
        "deploy-hotfix",
        "#!/usr/bin/env bash\necho deploy-hotfix\n",
    );
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = E2eSmokeRunner.run(&ctx).await;
    assert_eq!(
        result.status,
        SkillifyGateStatus::Passed,
        "{:?}",
        result.message
    );
}

#[tokio::test]
async fn e2e_smoke_runner_fails_on_output_mismatch() {
    let temp = TempDir::new().unwrap();
    materialize_script(
        temp.path(),
        "deploy-hotfix",
        "#!/usr/bin/env bash\necho wrong-output\n",
    );
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = E2eSmokeRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
}

// -- Timeout/process-kill behavior --

#[cfg(unix)]
#[tokio::test]
async fn unit_test_runner_kills_script_on_timeout() {
    // Script writes its PID to a file then sleeps 30s. The unit test has a
    // 200ms timeout. After the gate returns, the PID must NOT be running —
    // run_script should explicitly kill the subprocess on timeout.
    let temp = TempDir::new().unwrap();
    let pidfile = temp.path().join("script.pid");
    let pidfile_str = pidfile.display().to_string();
    let script_body = format!("#!/usr/bin/env bash\necho $$ > {pidfile_str}\nsleep 30\n");
    materialize_script(temp.path(), "slowscript", &script_body);

    let mut a = authored("slowscript");
    a.unit_tests = serde_json::json!({
        "cases": [{"input": "", "expected_stdout": "x\n", "timeout_ms": 200}]
    });
    let b = bundle("slowscript");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };

    let start = std::time::Instant::now();
    let result = UnitTestRunner.run(&ctx).await;
    let elapsed = start.elapsed();

    assert_eq!(result.status, SkillifyGateStatus::Failed);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or("")
            .contains("timed out"),
        "expected timeout message, got: {:?}",
        result.message
    );
    assert!(
        elapsed.as_secs() < 5,
        "should return promptly after timeout, took {elapsed:?}"
    );

    // Wait for the file to be written (script wrote its pid before sleeping).
    let mut attempts = 0;
    while !pidfile.exists() && attempts < 20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        attempts += 1;
    }
    if !pidfile.exists() {
        // Script may not have written pid in time; we can't verify kill, but
        // the timeout behavior itself is verified by elapsed/message above.
        return;
    }

    let pid_str = std::fs::read_to_string(&pidfile).unwrap();
    let pid: i32 = pid_str.trim().parse().expect("pid is int");

    // Give the kill signal a moment to take effect.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // kill -0 returns 0 if the process exists, non-zero if not.
    let status = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .expect("kill -0");
    assert!(
        !status.success(),
        "subprocess PID {pid} should be dead after timeout but is still running"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unit_test_runner_kills_descendant_on_timeout() {
    // Script spawns a long-running descendant (`sleep 30 &`) and records the
    // descendant's PID before sleeping itself. After the gate times out the
    // DESCENDANT must also be dead — killing only the shell is insufficient.
    let temp = TempDir::new().unwrap();
    let pidfile = temp.path().join("descendant.pid");
    let pidfile_str = pidfile.display().to_string();
    let script_body = format!("#!/usr/bin/env bash\nsleep 30 &\necho $! > {pidfile_str}\nwait\n");
    materialize_script(temp.path(), "spawner", &script_body);

    let mut a = authored("spawner");
    a.unit_tests = serde_json::json!({
        "cases": [{"input": "", "expected_stdout": "x\n", "timeout_ms": 200}]
    });
    let b = bundle("spawner");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let _ = UnitTestRunner.run(&ctx).await;

    // Wait briefly for the script to write the descendant pid.
    let mut attempts = 0;
    while !pidfile.exists() && attempts < 20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        attempts += 1;
    }
    if !pidfile.exists() {
        // Script raced — can't verify, but no false-positive.
        return;
    }
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .expect("pid is int");

    // Give the group-kill a moment to take effect.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let status = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .expect("kill -0");
    assert!(
        !status.success(),
        "descendant PID {pid} should be dead after timeout but is still running"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unit_test_runner_times_out_when_script_ignores_large_stdin() {
    // Round 3 regression: script never reads stdin, and the input is much
    // larger than the typical pipe buffer (64KB on Linux, 16KB on macOS).
    // The old run_script wrote stdin BEFORE the timeout future and would
    // deadlock here. With the fix, the outer timeout must trip.
    let temp = TempDir::new().unwrap();
    materialize_script(temp.path(), "noinput", "#!/usr/bin/env bash\nsleep 30\n");

    let big_input: String = "x".repeat(256 * 1024); // 256 KB
    let mut a = authored("noinput");
    a.unit_tests = serde_json::json!({
        "cases": [{"input": big_input, "expected_stdout": "x\n", "timeout_ms": 300}]
    });
    let b = bundle("noinput");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };

    let start = std::time::Instant::now();
    let result = UnitTestRunner.run(&ctx).await;
    let elapsed = start.elapsed();

    assert_eq!(result.status, SkillifyGateStatus::Failed);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or("")
            .contains("timed out"),
        "expected timeout message, got: {:?}",
        result.message
    );
    assert!(
        elapsed.as_secs() < 5,
        "should time out promptly even with large unread stdin, took {elapsed:?}"
    );
}

#[tokio::test]
async fn resolver_eval_fails_when_broad_trigger_has_low_precision() {
    // Round 4 regression: a broad trigger that matches lots of unrelated
    // intents but covers positives at recall ≥ 0.8 must still fail because
    // precision < 0.9. With recall-only check, this would have passed.
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    a.resolver_triggers = json!(["deploy"]);
    a.resolver_eval = json!({
        "intents": [
            // One positive that matches (1/1 recall).
            {"intent": "deploy hotfix", "expected_lane": "deploy.hotfix"},
            // Three negatives that also match the broad "deploy" trigger.
            {"intent": "deploy database", "expected_lane": "ops.db"},
            {"intent": "deploy frontend", "expected_lane": "ui.deploy"},
            {"intent": "deploy monitoring", "expected_lane": "ops.metrics"},
        ]
    });
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = ResolverEvalRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    let msg = result.message.unwrap_or_default();
    assert!(
        msg.contains("precision"),
        "expected precision-related failure message, got: {msg}"
    );
}

#[tokio::test]
async fn resolver_eval_passes_when_precise_trigger_only_matches_positives() {
    // Companion to the broad-trigger test: a precise trigger should pass
    // with both perfect recall and perfect precision.
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    a.resolver_triggers = json!(["deploy hotfix"]);
    a.resolver_eval = json!({
        "intents": [
            {"intent": "deploy hotfix", "expected_lane": "deploy.hotfix"},
            {"intent": "deploy database", "expected_lane": "ops.db"},
            {"intent": "deploy frontend", "expected_lane": "ui.deploy"},
        ]
    });
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = ResolverEvalRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Passed);
}

#[tokio::test]
async fn resolver_eval_fails_when_no_negative_intents_provided() {
    // Round 5 regression: precision is trivially 1.0 when there are no
    // negatives, so the gate must require at least one negative example
    // to validate precision meaningfully.
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    a.resolver_eval = json!({
        "intents": [
            {"intent": "deploy hotfix", "expected_lane": "deploy.hotfix"},
            {"intent": "deploy hotfix urgent", "expected_lane": "deploy.hotfix"},
        ]
    });
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = ResolverEvalRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    let msg = result.message.unwrap_or_default();
    assert!(
        msg.contains("negative"),
        "expected negative-examples failure, got: {msg}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unit_test_runner_works_with_relative_candidate_dir() {
    // Round 6 regression: when the handler is constructed with a relative
    // vault root (--vault . or relative CAIRN_VAULT), candidate_dir and the
    // script path are relative. The sandbox-lite cwd switch to a scratch
    // dir would make bash look for the script under scratch, not the
    // original cwd. The fix canonicalizes script_path before spawning.
    let temp = TempDir::new().unwrap();
    let abs_root = temp.path();
    materialize_script(abs_root, "relpath", "#!/usr/bin/env bash\necho ok\n");

    // Build a RELATIVE candidate_dir by combining cwd-relative pieces.
    let saved_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(temp.path().parent().unwrap()).unwrap();
    let rel_root = std::path::PathBuf::from(temp.path().file_name().unwrap());
    let mut a = authored("relpath");
    a.unit_tests = serde_json::json!({
        "cases": [{"input": "", "expected_stdout": "ok\n", "timeout_ms": 5000}]
    });
    let b = bundle("relpath");
    let ctx = GateRunContext {
        vault_root: &rel_root,
        candidate_id: "skc_test",
        candidate_dir: rel_root.clone(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = UnitTestRunner.run(&ctx).await;
    std::env::set_current_dir(saved_cwd).unwrap();
    assert_eq!(
        result.status,
        SkillifyGateStatus::Passed,
        "relative vault root should work, got: {:?}",
        result.message
    );
}

#[tokio::test]
async fn skill_contract_fails_when_triggers_empty_list() {
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    a.skill_markdown = "---\nname: x\nlane: deploy.hotfix\ntriggers: []\nuses: scripts/x.sh\nfiles_to: wiki/x/\n---\nBody.".to_owned();
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = SkillContractRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    assert!(result.message.unwrap().contains("triggers"));
}

#[tokio::test]
async fn skill_contract_fails_when_lane_only_nested_not_top_level() {
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    // Nested `lane:` under another mapping is NOT a top-level frontmatter key.
    a.skill_markdown = "---\nname: x\nmeta:\n  lane: nested.deploy\ntriggers:\n  - x\nuses: scripts/x.sh\nfiles_to: wiki/x/\n---\nBody.".to_owned();
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = SkillContractRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    assert!(result.message.unwrap().contains("lane"));
}

#[tokio::test]
async fn skill_contract_fails_when_lane_in_body_only() {
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    // Body mentions `lane:` but frontmatter doesn't include it.
    a.skill_markdown = "---\nname: x\ntriggers:\n  - x\nuses: scripts/x.sh\nfiles_to: wiki/x/\n---\nThis skill manages a `lane:` reference in body prose.".to_owned();
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = SkillContractRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    assert!(result.message.unwrap().contains("lane"));
}

#[tokio::test]
async fn skill_contract_fails_when_frontmatter_close_is_not_a_delimiter_line() {
    // Round 10 regression: `---suffix` must NOT count as the closing
    // delimiter. Previously `find("\n---")` matched it and the gate
    // accepted malformed frontmatter that downstream YAML readers reject.
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    // No real closing `---` line; only `---not-a-delimiter` near the end.
    a.skill_markdown =
        "---\nlane: deploy.hotfix\ntriggers:\n  - x\nuses: scripts/x.sh\nfiles_to: wiki/x/\n---suffix\nBody.".to_owned();
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = SkillContractRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    let msg = result.message.unwrap_or_default();
    assert!(
        msg.contains("frontmatter"),
        "expected frontmatter error, got: {msg}"
    );
}

#[tokio::test]
async fn e2e_smoke_runner_fails_when_trigger_phrase_does_not_match() {
    // Round-11 regression: smoke must require trigger_phrase to actually
    // resolve to the candidate. A phrase that doesn't match any of the
    // candidate's resolver_triggers must fail.
    let temp = TempDir::new().unwrap();
    materialize_script(
        temp.path(),
        "deploy-hotfix",
        "#!/usr/bin/env bash\necho deploy-hotfix\n",
    );
    let mut a = authored("deploy-hotfix");
    a.smoke = json!({
        "cases": [{"trigger_phrase": "completely unrelated phrase xyz", "expected_output": "deploy-hotfix\n"}]
    });
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = E2eSmokeRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    assert!(
        result
            .message
            .unwrap_or_default()
            .contains("trigger_phrase"),
        "expected trigger_phrase error"
    );
}

#[tokio::test]
async fn e2e_smoke_runner_fails_when_trigger_resolves_ambiguously() {
    // If another live skill in the snapshot also matches the trigger
    // phrase, the candidate must not promote.
    let temp = TempDir::new().unwrap();
    materialize_script(
        temp.path(),
        "deploy-hotfix",
        "#!/usr/bin/env bash\necho deploy-hotfix\n",
    );
    let a = authored("deploy-hotfix");
    let b = bundle("deploy-hotfix");
    let snapshot = SkillLintSnapshot {
        skills: vec![SkillLintSkill {
            skill_id: "other-skill".to_owned(),
            lane: "other.lane".to_owned(),
            path: "skills/skill_other.md".to_owned(),
            uses: None,
            resolver_triggers: vec!["deploy hotfix".to_owned()], // collision
            files_to: Some("wiki/other/".to_owned()),
            gate_report_passed: true,
            rollback_version_count: 1,
            existing_paths: vec![],
        }],
    };
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &snapshot,
    };
    let result = E2eSmokeRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    assert!(
        result.message.unwrap_or_default().contains("ambiguous"),
        "expected ambiguous-resolution error"
    );
}

#[tokio::test]
async fn unit_test_runner_rejects_oversized_authored_timeout() {
    // Round-11 regression: an LLM-authored timeout above MAX_CASE_TIMEOUT_MS
    // must fail the gate rather than block the worker.
    let temp = TempDir::new().unwrap();
    materialize_script(
        temp.path(),
        "deploy-hotfix",
        "#!/usr/bin/env bash\necho x\n",
    );
    let mut a = authored("deploy-hotfix");
    a.unit_tests = json!({
        "cases": [{"input": "", "expected_stdout": "x\n", "timeout_ms": 3_600_000_u64}]
    });
    let b = bundle("deploy-hotfix");
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &empty_snapshot(),
    };
    let result = UnitTestRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    assert!(
        result
            .message
            .unwrap_or_default()
            .contains("MAX_CASE_TIMEOUT_MS"),
        "expected timeout-exceeded error"
    );
}

#[tokio::test]
async fn resolver_trigger_rejects_substring_shadowing() {
    // Round-12 regression: a candidate trigger `deploy` would shadow an
    // existing trigger `deploy hotfix` at runtime (resolver uses
    // substring match). The gate must reject this.
    let temp = TempDir::new().unwrap();
    let mut a = authored("deploy-hotfix");
    a.resolver_triggers = json!(["deploy"]);
    let b = bundle("deploy-hotfix");
    let snapshot = SkillLintSnapshot {
        skills: vec![SkillLintSkill {
            skill_id: "existing-deploy-hotfix".to_owned(),
            lane: "other.deploy".to_owned(),
            path: "skills/skill_existing.md".to_owned(),
            uses: None,
            resolver_triggers: vec!["deploy hotfix".to_owned()],
            files_to: Some("wiki/".to_owned()),
            gate_report_passed: true,
            rollback_version_count: 1,
            existing_paths: vec![],
        }],
    };
    let ctx = GateRunContext {
        vault_root: temp.path(),
        candidate_id: "skc_test",
        candidate_dir: temp.path().to_path_buf(),
        bundle: &b,
        authored: &a,
        llm: None,
        snapshot: &snapshot,
    };
    let result = ResolverTriggerRunner.run(&ctx).await;
    assert_eq!(result.status, SkillifyGateStatus::Failed);
    assert!(
        result.message.unwrap_or_default().contains("overlaps"),
        "expected overlap rejection"
    );
}
