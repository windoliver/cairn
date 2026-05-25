//! Daily health check for promoted skills.

use std::path::PathBuf;
use std::sync::Arc;

use cairn_core::contract::llm_provider::LLMProvider;
use cairn_core::pipeline::skillify::{SkillArtifactBundle, SkillifyGateReport, SkillifyGateStatus};

use super::gate_registry::GateRunnerRegistry;
use super::gate_runner::GateRunContext;
use super::materialize::AuthoredSkillBundle;

/// Result of a health check on one promoted skill.
#[derive(Debug)]
pub struct HealthCheckResult {
    /// Candidate id.
    pub candidate_id: String,
    /// Whether all gates still pass.
    pub healthy: bool,
    /// Updated gate report.
    pub gate_report: SkillifyGateReport,
    /// Newly failed gate names.
    pub regressions: Vec<String>,
}

/// Runs health checks against promoted skills.
pub struct HealthCheckRunner {
    vault_root: PathBuf,
    llm: Option<Arc<dyn LLMProvider>>,
    gate_registry: GateRunnerRegistry,
}

impl HealthCheckRunner {
    /// Create a new health check runner.
    #[must_use]
    pub fn new(vault_root: PathBuf, llm: Option<Arc<dyn LLMProvider>>) -> Self {
        Self {
            vault_root,
            llm,
            gate_registry: GateRunnerRegistry::default_suite(),
        }
    }

    /// Run health check for one candidate.
    ///
    /// # Errors
    /// Returns on I/O or JSON failures.
    pub async fn check(
        &self,
        candidate_id: &str,
    ) -> Result<HealthCheckResult, Box<dyn std::error::Error + Send + Sync>> {
        let candidate_dir = self
            .vault_root
            .join(".cairn/evolution/skillify")
            .join(candidate_id);

        let bundle: SkillArtifactBundle =
            serde_json::from_slice(&std::fs::read(candidate_dir.join("manifest.json"))?)?;

        let authored = reconstruct_authored(&candidate_dir, &bundle)?;

        // Round-13 hardening: use the REAL vault snapshot so health checks
        // catch collisions with skills added since the last gate run.
        // Previously the empty snapshot let a candidate report "healthy"
        // even after another skill installed a colliding trigger.
        let snapshot = super::snapshot::build_vault_snapshot(&self.vault_root, Some(candidate_id))?;

        let ctx = GateRunContext {
            vault_root: &self.vault_root,
            candidate_id,
            candidate_dir: candidate_dir.clone(),
            bundle: &bundle,
            authored: &authored,
            llm: self.llm.as_deref(),
            snapshot: &snapshot,
        };

        let results = self.gate_registry.run_all(&ctx).await;

        let mut report = SkillifyGateReport {
            candidate_id: candidate_id.to_owned(),
            gates: Vec::new(),
        };
        let mut regressions = Vec::new();

        for result in &results {
            if result.status != SkillifyGateStatus::Passed {
                regressions.push(result.kind.as_str().to_owned());
            }
            report.gates.push(result.clone().into_gate());
        }

        // Round-15 hardening: after gate execution, re-verify every
        // declared artifact's bytes against its content_sha256. The main
        // pipeline does this before Promote (a self-mutating script
        // appending to $0 would otherwise drift the manifest); health
        // check needs the SAME guarantee or installs/health-checks can
        // record a passing report for bytes that no longer match the
        // manifest, making `candidate_ready` trust stale evidence.
        for artifact in &bundle.artifacts {
            let artifact_path = candidate_dir.join(&artifact.path);
            let bytes = match std::fs::read(&artifact_path) {
                Ok(b) => b,
                Err(e) => {
                    let msg = format!("post-gate verify: missing artifact {}: {e}", artifact.path);
                    regressions.push(format!("post_gate_verify[{}]", artifact.path));
                    report
                        .gates
                        .push(cairn_core::pipeline::skillify::SkillifyGate {
                            name: format!("post_gate_verify[{}]", artifact.path),
                            status: SkillifyGateStatus::Failed,
                            message: Some(msg),
                        });
                    continue;
                }
            };
            let actual = {
                use sha2::Digest as _;
                let mut h = sha2::Sha256::new();
                h.update(&bytes);
                format!("sha256:{:x}", h.finalize())
            };
            if actual != artifact.content_sha256 {
                let msg = format!(
                    "post-gate verify: {} hash drifted (expected {}, got {actual})",
                    artifact.path, artifact.content_sha256
                );
                regressions.push(format!("post_gate_verify[{}]", artifact.path));
                report
                    .gates
                    .push(cairn_core::pipeline::skillify::SkillifyGate {
                        name: format!("post_gate_verify[{}]", artifact.path),
                        status: SkillifyGateStatus::Failed,
                        message: Some(msg),
                    });
            }
        }

        std::fs::write(
            candidate_dir.join("gate-report.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;

        Ok(HealthCheckResult {
            candidate_id: candidate_id.to_owned(),
            healthy: regressions.is_empty(),
            gate_report: report,
            regressions,
        })
    }
}

fn reconstruct_authored(
    candidate_dir: &std::path::Path,
    bundle: &SkillArtifactBundle,
) -> Result<AuthoredSkillBundle, Box<dyn std::error::Error + Send + Sync>> {
    use cairn_core::pipeline::skillify::SkillArtifactKind;

    let read_artifact = |kind: SkillArtifactKind| -> Result<String, std::io::Error> {
        let artifact = bundle
            .artifacts
            .iter()
            .find(|a| a.kind == kind)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, format!("missing {kind}"))
            })?;
        std::fs::read_to_string(candidate_dir.join(&artifact.path))
    };

    let read_json = |kind: SkillArtifactKind| -> Result<
        serde_json::Value,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let content = read_artifact(kind)?;
        Ok(serde_json::from_str(&content)?)
    };

    let slug = bundle
        .artifacts
        .iter()
        .find(|a| a.kind == SkillArtifactKind::SkillContract)
        .map_or_else(
            || "unknown".to_owned(),
            |a| {
                a.path
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .strip_prefix("skill_")
                    .and_then(|s| s.strip_suffix(".md"))
                    .unwrap_or("unknown")
                    .to_owned()
            },
        );

    let skill_markdown = read_artifact(SkillArtifactKind::SkillContract)?;
    let lane = skill_markdown
        .lines()
        .find_map(|l| l.strip_prefix("lane:").map(|v| v.trim().to_owned()))
        .unwrap_or_else(|| "unknown".to_owned());

    Ok(AuthoredSkillBundle {
        lane,
        slug,
        skill_markdown,
        script: read_artifact(SkillArtifactKind::DeterministicScript)?,
        unit_tests: read_json(SkillArtifactKind::UnitTests)?,
        integration_tests: read_json(SkillArtifactKind::IntegrationTests)?,
        llm_evals: read_json(SkillArtifactKind::LlmEvals)?,
        resolver_triggers: read_json(SkillArtifactKind::ResolverTrigger)?,
        resolver_eval: read_json(SkillArtifactKind::ResolverEval)?,
        smoke: read_json(SkillArtifactKind::E2eSmoke)?,
        filing_rules: read_json(SkillArtifactKind::FilingRules)?,
    })
}
