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
        resolver_eval: json!({"intents": [{"intent": "deploy hotfix", "expected_lane": "deploy.hotfix"}]}),
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
    materialize_script(temp.path(), "deploy-hotfix", "#!/usr/bin/env bash\necho ok\n");
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
    materialize_script(temp.path(), "deploy-hotfix", "#!/usr/bin/env bash\necho deploy-hotfix\n");
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
    assert_eq!(result.status, SkillifyGateStatus::Passed, "{:?}", result.message);
}

#[tokio::test]
async fn unit_test_runner_fails_on_stdout_mismatch() {
    let temp = TempDir::new().unwrap();
    materialize_script(temp.path(), "deploy-hotfix", "#!/usr/bin/env bash\necho wrong\n");
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
    materialize_script(temp.path(), "deploy-hotfix", "#!/usr/bin/env bash\necho ok\n");
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
    materialize_script(temp.path(), "deploy-hotfix", "#!/usr/bin/env bash\necho deploy-hotfix\n");
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
    assert_eq!(result.status, SkillifyGateStatus::Passed, "{:?}", result.message);
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
    assert_eq!(result.status, SkillifyGateStatus::Passed, "{:?}", result.message);
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
        Ok(CompletionOutput::Json(serde_json::json!({"pass": true, "reason": "ok"})))
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
        Ok(CompletionOutput::Json(serde_json::json!({"pass": false, "reason": "bad"})))
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
    assert_eq!(result.status, SkillifyGateStatus::Passed, "{:?}", result.message);
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
    assert_eq!(result.status, SkillifyGateStatus::Passed, "{:?}", result.message);
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
    materialize_script(temp.path(), "deploy-hotfix", "#!/usr/bin/env bash\necho deploy-hotfix\n");
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
    assert_eq!(result.status, SkillifyGateStatus::Passed, "{:?}", result.message);
}

#[tokio::test]
async fn e2e_smoke_runner_fails_on_output_mismatch() {
    let temp = TempDir::new().unwrap();
    materialize_script(temp.path(), "deploy-hotfix", "#!/usr/bin/env bash\necho wrong-output\n");
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
