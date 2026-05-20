#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::job_store::FailureClass;
use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_workflows::scheduler::{HandlerOutcome, JobHandler};
use cairn_workflows::{SkillifyHandler, SkillifyPayload, SkillifyTrigger};
use serde_json::json;
use tempfile::TempDir;

struct JsonLlm {
    output: CompletionOutput,
}

impl JsonLlm {
    fn bundle(slug: &str) -> Self {
        Self {
            output: CompletionOutput::Json(json!({
                "lane": "deploy.hotfix",
                "slug": slug,
                "skill_markdown": format!("---\nname: {slug}\nlane: deploy.hotfix\ntriggers: [\"deploy hotfix\"]\nuses: scripts/{slug}.sh\nfiles_to: wiki/summaries/\n---\nRun the script."),
                "script": format!("#!/usr/bin/env bash\nset -euo pipefail\necho {slug}\n"),
                "unit_tests": {"command": format!("bash scripts/{slug}.sh"), "expected_stdout": format!("{slug}\n")},
                "integration_tests": {"command": format!("bash scripts/{slug}.sh"), "expected_stdout": format!("{slug}\n")},
                "llm_evals": [{"intent": "deploy hotfix", "must_call": slug}],
                "resolver_triggers": ["deploy hotfix"],
                "resolver_eval": [{"intent": "deploy hotfix", "expected_skill": slug}],
                "smoke": {"prompt": "deploy hotfix", "expected_skill": slug},
                "filing_rules": {"files_to": "wiki/summaries/"}
            })),
        }
    }
}

#[async_trait::async_trait]
impl LLMProvider for JsonLlm {
    fn name(&self) -> &str {
        "json-llm"
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
        Ok(self.output.clone())
    }
}

fn payload() -> SkillifyPayload {
    SkillifyPayload {
        trigger: SkillifyTrigger::Explicit,
        key: "session-1".to_owned(),
        candidate_id: Some("skc_fixture".to_owned()),
        bound_scope: None,
        source_record_ids: vec!["01HQZX9F5N0000000000000001".to_owned()],
    }
}

#[tokio::test]
async fn handler_materializes_candidate_bundle_from_llm_json() {
    let temp = TempDir::new().expect("temp");
    let handler = SkillifyHandler::new(
        temp.path().to_path_buf(),
        Some(Arc::new(JsonLlm::bundle("deploy-hotfix"))),
    );

    handler.run_once(payload()).await.expect("run");

    let root = temp.path().join(".cairn/evolution/skillify/skc_fixture");
    assert!(root.join("manifest.json").exists());
    assert!(root.join("gate-report.json").exists());

    for path in [
        "bundle/skills/skill_deploy-hotfix.md",
        "bundle/scripts/deploy-hotfix.sh",
        "bundle/tests/unit/deploy-hotfix.json",
        "bundle/tests/integration/deploy-hotfix.json",
        "bundle/evals/llm/deploy-hotfix.json",
        "bundle/resolver/triggers.json",
        "bundle/resolver/eval.json",
        "bundle/audits/check-resolvable.json",
        "bundle/smoke/deploy-hotfix.json",
        "bundle/filing-rules.json",
    ] {
        assert!(root.join(path).exists(), "{path}");
    }

    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("gate-report.json")).expect("report"))
            .expect("json");
    assert!(
        report["gates"]
            .as_array()
            .expect("gates")
            .iter()
            .all(|gate| gate["status"] == "passed")
    );
}

#[tokio::test]
async fn handler_rejects_llm_slug_that_could_escape_bundle() {
    let temp = TempDir::new().expect("temp");
    let handler = SkillifyHandler::new(
        temp.path().to_path_buf(),
        Some(Arc::new(JsonLlm::bundle("../escape"))),
    );

    let err = handler.run_once(payload()).await.expect_err("unsafe slug");

    assert!(err.to_string().contains("invalid slug"));
    assert!(
        !temp
            .path()
            .join(".cairn/evolution/skillify/skc_fixture")
            .exists()
    );
}

#[tokio::test]
async fn job_handler_maps_invalid_payload_and_missing_llm_to_validation_permanent() {
    let temp = TempDir::new().expect("temp");
    let handler = SkillifyHandler::new(temp.path().to_path_buf(), None);

    assert!(matches!(
        handler.handle(&b"not-json".to_vec()).await,
        HandlerOutcome::Permanent {
            class: FailureClass::Validation,
            ..
        }
    ));

    assert!(matches!(
        handler
            .handle(&payload().to_bytes().expect("payload"))
            .await,
        HandlerOutcome::Permanent {
            class: FailureClass::Validation,
            ..
        }
    ));
}
