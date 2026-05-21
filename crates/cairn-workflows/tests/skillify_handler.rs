#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::job_store::FailureClass;
use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::flush_plan::{PersistedPlan, PlanReason};
use cairn_core::pipeline::skillify::{
    SkillArtifact, SkillArtifactBundle, SkillArtifactKind, SkillifyGate, SkillifyGateReport,
    SkillifyGateStatus,
};
use cairn_workflows::scheduler::{HandlerOutcome, JobHandler};
use cairn_workflows::skillify::materialize::{
    AuthoredSkillBundle, candidate_ready, materialize_bundle,
};
use cairn_workflows::skillify::planner::{SkillifyPlanSource, SkillifyPromotionInput};
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

struct FailingLlm;

#[async_trait::async_trait]
impl LLMProvider for FailingLlm {
    fn name(&self) -> &str {
        "failing-llm"
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
        Err(LlmError::NotConfigured {
            remediation: "cairn config set llm.provider ollama".to_owned(),
        })
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

fn authored_bundle(slug: &str) -> AuthoredSkillBundle {
    AuthoredSkillBundle {
        lane: "deploy.hotfix".to_owned(),
        slug: slug.to_owned(),
        skill_markdown: format!(
            "---\nname: {slug}\nlane: deploy.hotfix\ntriggers: [\"deploy hotfix\"]\nuses: scripts/{slug}.sh\nfiles_to: wiki/summaries/\n---\nRun the script."
        ),
        script: format!("#!/usr/bin/env bash\nset -euo pipefail\necho {slug}\n"),
        unit_tests: json!({"command": format!("bash scripts/{slug}.sh")}),
        integration_tests: json!({"command": format!("bash scripts/{slug}.sh")}),
        llm_evals: json!([{"intent": "deploy hotfix", "must_call": slug}]),
        resolver_triggers: json!(["deploy hotfix"]),
        resolver_eval: json!([{"intent": "deploy hotfix", "expected_skill": slug}]),
        smoke: json!({"prompt": "deploy hotfix", "expected_skill": slug}),
        filing_rules: json!({"files_to": "wiki/summaries/"}),
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
            .all(|gate| gate["status"] == "blocked")
    );
    assert!(!candidate_ready(temp.path(), "skc_fixture").expect("candidate readiness"));
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
async fn handler_replay_keeps_existing_bundle_without_calling_llm() {
    let temp = TempDir::new().expect("temp");
    let handler = SkillifyHandler::new(
        temp.path().to_path_buf(),
        Some(Arc::new(JsonLlm::bundle("deploy-hotfix"))),
    );
    handler.run_once(payload()).await.expect("first run");

    let replay = SkillifyHandler::new(
        temp.path().to_path_buf(),
        Some(Arc::new(JsonLlm {
            output: CompletionOutput::Text("not json".to_owned()),
        })),
    );
    replay.run_once(payload()).await.expect("replay");

    let script = std::fs::read_to_string(
        temp.path()
            .join(".cairn/evolution/skillify/skc_fixture/bundle/scripts/deploy-hotfix.sh"),
    )
    .expect("script");
    assert!(script.contains("deploy-hotfix"));
    assert!(
        !temp
            .path()
            .join(".cairn/evolution/skillify/skc_fixture/bundle/scripts/changed.sh")
            .exists()
    );
}

#[tokio::test]
async fn job_handler_maps_permanent_failures_to_validation_permanent() {
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

    let unsafe_handler = SkillifyHandler::new(
        temp.path().to_path_buf(),
        Some(Arc::new(JsonLlm::bundle("../escape"))),
    );
    assert!(matches!(
        unsafe_handler
            .handle(&payload().to_bytes().expect("payload"))
            .await,
        HandlerOutcome::Permanent {
            class: FailureClass::Validation,
            ..
        }
    ));

    let failing_handler =
        SkillifyHandler::new(temp.path().to_path_buf(), Some(Arc::new(FailingLlm)));
    assert!(matches!(
        failing_handler
            .handle(
                &SkillifyPayload {
                    candidate_id: Some("skc_missing_llm".to_owned()),
                    ..payload()
                }
                .to_bytes()
                .expect("payload")
            )
            .await,
        HandlerOutcome::Permanent {
            class: FailureClass::Validation,
            ..
        }
    ));
}

#[tokio::test]
async fn handler_without_llm_writes_lint_visible_blocked_candidate() {
    let temp = TempDir::new().expect("temp");
    let handler = SkillifyHandler::new(temp.path().to_path_buf(), None);

    let outcome = handler
        .handle(&payload().to_bytes().expect("payload"))
        .await;

    assert!(matches!(
        outcome,
        HandlerOutcome::Permanent {
            class: FailureClass::Validation,
            ..
        }
    ));
    let root = temp.path().join(".cairn/evolution/skillify/skc_fixture");
    assert!(root.join("gate-report.json").exists());
    assert!(!root.join("manifest.json").exists());
    let report: SkillifyGateReport =
        serde_json::from_slice(&std::fs::read(root.join("gate-report.json")).expect("report"))
            .expect("gate report");
    assert!(!report.ready_for_promotion());
    assert!(
        report
            .gates
            .iter()
            .all(|gate| gate.status == SkillifyGateStatus::Blocked)
    );
}

#[test]
fn existing_candidate_rejects_empty_artifact_paths() {
    let temp = TempDir::new().expect("temp");
    let candidate_id = "skc_empty_path";
    let root = temp
        .path()
        .join(".cairn/evolution/skillify")
        .join(candidate_id);
    std::fs::create_dir_all(&root).expect("candidate root");
    let bundle = SkillArtifactBundle {
        candidate_id: candidate_id.to_owned(),
        version: 1,
        artifacts: SkillArtifactKind::required()
            .iter()
            .map(|kind| SkillArtifact {
                kind: *kind,
                path: String::new(),
                content_sha256:
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                        .to_owned(),
                evidence_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
                status: "generated".to_owned(),
            })
            .collect(),
    };
    let report = SkillifyGateReport {
        candidate_id: candidate_id.to_owned(),
        gates: SkillArtifactKind::required()
            .iter()
            .map(|kind| SkillifyGate {
                name: kind.as_str().to_owned(),
                status: SkillifyGateStatus::Passed,
                message: None,
            })
            .collect(),
    };
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&bundle).expect("manifest json"),
    )
    .expect("manifest");
    std::fs::write(
        root.join("gate-report.json"),
        serde_json::to_vec_pretty(&report).expect("report json"),
    )
    .expect("report");

    let err = candidate_ready(temp.path(), candidate_id).expect_err("empty path not ready");
    assert!(err.to_string().contains("invalid path"));
}

#[test]
fn existing_candidate_rejects_directory_artifact_path() {
    let temp = TempDir::new().expect("temp");
    materialize_bundle(
        temp.path(),
        "skc_directory_artifact",
        &authored_bundle("deploy-hotfix"),
        &["01HQZX9F5N0000000000000001".to_owned()],
    )
    .expect("materialize");
    let script = temp
        .path()
        .join(".cairn/evolution/skillify/skc_directory_artifact/bundle/scripts/deploy-hotfix.sh");
    std::fs::remove_file(&script).expect("remove script");
    std::fs::create_dir(&script).expect("directory at script path");

    let err = candidate_ready(temp.path(), "skc_directory_artifact")
        .expect_err("directory artifact not ready");
    assert!(err.to_string().contains("incomplete"));
}

#[test]
fn existing_candidate_rejects_artifact_content_hash_mismatch() {
    let temp = TempDir::new().expect("temp");
    materialize_bundle(
        temp.path(),
        "skc_hash_mismatch",
        &authored_bundle("deploy-hotfix"),
        &["01HQZX9F5N0000000000000001".to_owned()],
    )
    .expect("materialize");
    std::fs::write(
        temp.path()
            .join(".cairn/evolution/skillify/skc_hash_mismatch/bundle/scripts/deploy-hotfix.sh"),
        "#!/usr/bin/env bash\necho changed\n",
    )
    .expect("tamper script");

    let err =
        candidate_ready(temp.path(), "skc_hash_mismatch").expect_err("tampered artifact not ready");
    assert!(err.to_string().contains("incomplete"));
}

#[test]
fn promotion_plan_records_candidate_and_gate_count() {
    let plan = SkillifyPlanSource::plan_promotion(SkillifyPromotionInput {
        candidate_id: "skc_fixture".to_owned(),
        skill_target_id: "01HQZX9F5N0000000000000003".to_owned(),
        evidence_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
        gate_count: 10,
    })
    .expect("plan");

    assert!(matches!(
        plan.reason,
        PlanReason::Skillify {
            ref candidate_id,
            gate_count: 10
        } if candidate_id == "skc_fixture"
    ));
    assert!(plan.placeholder);
    assert_eq!(
        PersistedPlan::pending(plan).schema_version,
        PersistedPlan::SKILLIFY_SCHEMA_VERSION
    );
}

#[test]
fn promotion_plan_rejects_unsafe_candidate_and_invalid_evidence() {
    let unsafe_candidate = SkillifyPlanSource::plan_promotion(SkillifyPromotionInput {
        candidate_id: "../escape".to_owned(),
        skill_target_id: "01HQZX9F5N0000000000000003".to_owned(),
        evidence_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
        gate_count: 10,
    })
    .expect_err("unsafe candidate");
    assert!(unsafe_candidate.contains("invalid candidate id"));

    let invalid_evidence = SkillifyPlanSource::plan_promotion(SkillifyPromotionInput {
        candidate_id: "skc_fixture".to_owned(),
        skill_target_id: "01HQZX9F5N0000000000000003".to_owned(),
        evidence_refs: vec!["not-a-ulid".to_owned()],
        gate_count: 10,
    })
    .expect_err("invalid evidence");
    assert!(invalid_evidence.contains("invalid evidence ref"));
}

#[test]
fn promotion_plan_uses_live_five_minute_ttl() {
    let plan = SkillifyPlanSource::plan_promotion(SkillifyPromotionInput {
        candidate_id: "skc_fixture".to_owned(),
        skill_target_id: "01HQZX9F5N0000000000000003".to_owned(),
        evidence_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
        gate_count: 10,
    })
    .expect("plan");

    assert_ne!(plan.issued_at, "2026-05-20T00:00:00Z");
    let issued = chrono::DateTime::parse_from_rfc3339(&plan.issued_at).expect("issued");
    let expires = chrono::DateTime::parse_from_rfc3339(&plan.expires_at).expect("expires");
    assert!(expires > issued);
}
