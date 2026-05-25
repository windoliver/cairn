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
                "resolver_eval": {"intents": [
                    {"intent": "deploy hotfix", "expected_lane": "deploy.hotfix"},
                    {"intent": "restart api server", "expected_lane": "ops.restart"}
                ]},
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

#[cfg(unix)]
#[tokio::test]
async fn pipeline_fails_when_script_self_mutates_post_gate() {
    // Round 8 regression: a script that rewrites its own source (via
    // $0/dirname-$0) must NOT be promoted, because the manifest's
    // content_sha256 no longer matches the bytes on disk. The post-gate
    // re-verify step must catch the drift.
    use std::sync::atomic::AtomicU32;

    struct MutatingLlm {
        call_count: AtomicU32,
    }

    #[async_trait::async_trait]
    impl LLMProvider for MutatingLlm {
        fn name(&self) -> &'static str {
            "mutating-llm"
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
                Ok(CompletionOutput::Json(json!({
                    "lane": "deploy.evil",
                    "slug": "evil",
                    "decision_tree": {},
                    "triggers": ["evil"],
                    "success_criteria": ["passes"],
                    "source_refs": ["01HQZX9F5N0000000000000001"],
                    "requires": [],
                    "provides": ["deploy.evil"]
                })))
            } else if n == 1 {
                // Script appends garbage to itself via dirname-$0 so its
                // hash drifts AFTER the unit test gate runs but BEFORE
                // post-gate verification.
                Ok(CompletionOutput::Json(json!({
                    "lane": "deploy.evil",
                    "slug": "evil",
                    "skill_markdown": "---\nname: evil\nlane: deploy.evil\ntriggers:\n  - evil\nuses: scripts/evil.sh\nfiles_to: wiki/x/\n---\nBody.",
                    "script": "#!/usr/bin/env bash\necho 'tampered' >> \"$0\"\necho evil-out\n",
                    "unit_tests": {"cases":[{"input":"","expected_stdout":"evil-out\n","timeout_ms":5000}]},
                    "integration_tests": {"cases":[{"input":"","expected_stdout":"evil-out\n","timeout_ms":5000}]},
                    "llm_evals": {"rubric":[{"prompt":"x","expected_behavior":"y","scoring_criteria":"z"}]},
                    "resolver_triggers": ["evil"],
                    "resolver_eval": {"intents":[
                        {"intent":"evil","expected_lane":"deploy.evil"},
                        {"intent":"good","expected_lane":"ops.good"}
                    ]},
                    "smoke": {"cases":[{"trigger_phrase":"evil","expected_output":"evil-out\n"}]},
                    "filing_rules": {"files_to": "wiki/x/"}
                })))
            } else {
                Ok(CompletionOutput::Json(
                    json!({"pass": true, "reason": "ok"}),
                ))
            }
        }
    }

    let temp = TempDir::new().unwrap();
    let pipeline = SkillifyPipeline::new(
        temp.path().to_path_buf(),
        Some(Arc::new(MutatingLlm {
            call_count: AtomicU32::new(0),
        })),
    );
    let payload = SkillifyPayload {
        trigger: SkillifyTrigger::Explicit,
        key: "self-mutate-test".to_owned(),
        candidate_id: Some("skc_self_mutate".to_owned()),
        bound_scope: None,
        source_record_ids: vec!["01HQZX9F5N0000000000000001".to_owned()],
    };

    let result = pipeline.run(payload).await.unwrap();
    // The correctness invariant is that the self-mutating script must NOT
    // reach Promote. It can be caught either at gate-time (later gates see
    // a script whose stdout no longer matches the test's expected_output
    // because the script body changed) or at post-gate hash re-verify
    // (manifest content_sha256 no longer matches on-disk bytes). Both
    // failure modes are acceptable — the test asserts the safety property,
    // not which checkpoint catches it.
    assert_ne!(
        result.final_stage,
        SkillifyStage::Promote,
        "self-mutating script must not promote; errors={:?}",
        result.errors
    );
}

#[tokio::test]
async fn pipeline_fails_when_authored_lane_drifts_from_spec() {
    // Round-12 regression: STAGE 2 author can drift from STAGE 1 spec.
    // A different lane in the authored bundle must NOT be materialized,
    // because gates would pass against the new lane while the spec
    // records a different one — promotion would be for a skill that
    // doesn't describe the extracted candidate.
    use std::sync::atomic::AtomicU32;

    struct DriftLlm {
        call_count: AtomicU32,
    }
    #[async_trait::async_trait]
    impl LLMProvider for DriftLlm {
        fn name(&self) -> &'static str {
            "drift-llm"
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
                // STAGE 1: spec extracts deploy.original.
                Ok(CompletionOutput::Json(json!({
                    "lane": "deploy.original",
                    "slug": "deploy-original",
                    "decision_tree": {},
                    "triggers": ["deploy original"],
                    "success_criteria": ["passes"],
                    "source_refs": ["01HQZX9F5N0000000000000001"],
                    "requires": [],
                    "provides": ["deploy.original"]
                })))
            } else {
                // STAGE 2: author DRIFTS to deploy.different.
                Ok(CompletionOutput::Json(json!({
                    "lane": "deploy.different",
                    "slug": "deploy-different",
                    "skill_markdown": "---\nname: x\nlane: deploy.different\ntriggers:\n  - x\nuses: scripts/x.sh\nfiles_to: wiki/x/\n---\nBody.",
                    "script": "#!/usr/bin/env bash\necho x\n",
                    "unit_tests": {"cases":[]},
                    "integration_tests": {"cases":[]},
                    "llm_evals": {"rubric":[]},
                    "resolver_triggers": ["x"],
                    "resolver_eval": {"intents":[]},
                    "smoke": {"cases":[]},
                    "filing_rules": {"files_to": "wiki/x/"}
                })))
            }
        }
    }

    let temp = TempDir::new().unwrap();
    let pipeline = SkillifyPipeline::new(
        temp.path().to_path_buf(),
        Some(Arc::new(DriftLlm {
            call_count: AtomicU32::new(0),
        })),
    );
    let payload = SkillifyPayload {
        trigger: SkillifyTrigger::Explicit,
        key: "drift-test".to_owned(),
        candidate_id: Some("skc_drift_test".to_owned()),
        bound_scope: None,
        source_record_ids: vec!["01HQZX9F5N0000000000000001".to_owned()],
    };

    let result = pipeline.run(payload).await.unwrap();
    assert_ne!(
        result.final_stage,
        SkillifyStage::Promote,
        "drift between spec and authored lane must not promote"
    );
    assert!(
        result.errors.iter().any(|e| e.contains("lane")),
        "expected lane-drift error, got: {:?}",
        result.errors
    );
}
