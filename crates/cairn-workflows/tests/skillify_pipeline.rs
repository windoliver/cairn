#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::pipeline::skillify::SkillifyStage;
use cairn_workflows::skillify::pipeline::SkillifyPipeline;
use cairn_workflows::{SkillifyPayload, SkillifyTrigger};
use serde_json::json;
use tempfile::TempDir;

struct PipelineLlm {
    call_count: std::sync::atomic::AtomicU32,
}

impl PipelineLlm {
    fn new() -> Self {
        Self {
            call_count: std::sync::atomic::AtomicU32::new(0),
        }
    }
}

#[async_trait::async_trait]
impl LLMProvider for PipelineLlm {
    fn name(&self) -> &'static str {
        "pipeline-llm"
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
        let n = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n == 0 {
            // STAGE 1: extraction → spec draft
            Ok(CompletionOutput::Json(json!({
                "lane": "deploy.hotfix",
                "slug": "deploy-hotfix",
                "decision_tree": {"root": "check_env"},
                "triggers": ["deploy hotfix"],
                "success_criteria": ["script exits 0"],
                "source_refs": ["01HQZX9F5N0000000000000001"],
                "requires": [],
                "provides": ["deploy.hotfix"]
            })))
        } else if n == 1 {
            // STAGE 2: authoring → authored bundle
            Ok(CompletionOutput::Json(json!({
                "lane": "deploy.hotfix",
                "slug": "deploy-hotfix",
                "skill_markdown": "---\nname: deploy-hotfix\nlane: deploy.hotfix\ntriggers:\n  - deploy hotfix\nuses: scripts/deploy-hotfix.sh\nfiles_to: wiki/summaries/\n---\nRun the script.",
                "script": "#!/usr/bin/env bash\nset -euo pipefail\necho deploy-hotfix\n",
                "unit_tests": {"cases": [{"input": "", "expected_stdout": "deploy-hotfix\n", "timeout_ms": 5000}]},
                "integration_tests": {"cases": [{"input": "", "expected_stdout": "deploy-hotfix\n", "timeout_ms": 10000}]},
                "llm_evals": {"rubric": [{"prompt": "deploy hotfix", "expected_behavior": "calls script", "scoring_criteria": "invoked"}]},
                "resolver_triggers": ["deploy hotfix"],
                "resolver_eval": {"intents": [{"intent": "deploy hotfix", "expected_lane": "deploy.hotfix"}]},
                "smoke": {"cases": [{"trigger_phrase": "deploy hotfix", "expected_output": "deploy-hotfix\n"}]},
                "filing_rules": {"files_to": "wiki/summaries/"}
            })))
        } else {
            // STAGE 3 LLM eval: always pass
            Ok(CompletionOutput::Json(
                json!({"pass": true, "reason": "looks good"}),
            ))
        }
    }
}

fn payload() -> SkillifyPayload {
    SkillifyPayload {
        trigger: SkillifyTrigger::Explicit,
        key: "session-pipeline".to_owned(),
        candidate_id: Some("skc_pipeline_test".to_owned()),
        bound_scope: None,
        source_record_ids: vec!["01HQZX9F5N0000000000000001".to_owned()],
    }
}

#[tokio::test]
async fn pipeline_runs_all_stages_with_mock_llm() {
    let temp = TempDir::new().unwrap();
    let pipeline = SkillifyPipeline::new(
        temp.path().to_path_buf(),
        Some(Arc::new(PipelineLlm::new())),
    );

    let result = pipeline.run(payload()).await.unwrap();
    assert_eq!(result.final_stage, SkillifyStage::Promote);
    assert!(result.errors.is_empty());
    assert!(!result.gate_report.gates.is_empty());
}

#[tokio::test]
async fn pipeline_blocks_without_llm() {
    let temp = TempDir::new().unwrap();
    let pipeline = SkillifyPipeline::new(temp.path().to_path_buf(), None);

    let result = pipeline.run(payload()).await.unwrap();
    assert_eq!(result.final_stage, SkillifyStage::Blocked);
}
