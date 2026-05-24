//! Ordered gate runner execution with dependency blocking.

use cairn_core::pipeline::skillify::{SkillArtifactKind, SkillifyGateStatus};

use super::gate_runner::{
    CheckResolvableAndDryRunner, DeterministicScriptRunner, E2eSmokeRunner, FilingRulesRunner,
    GateRunContext, GateRunResult, GateRunner, IntegrationTestRunner, LlmEvalRunner,
    ResolverEvalRunner, ResolverTriggerRunner, SkillContractRunner, UnitTestRunner,
};

/// Dependency tier for gate execution ordering.
struct Tier {
    runners: Vec<Box<dyn GateRunner>>,
    depends_on: Vec<SkillArtifactKind>,
}

/// Registry of gate runners with dependency-ordered execution.
pub struct GateRunnerRegistry {
    tiers: Vec<Tier>,
}

impl GateRunnerRegistry {
    /// Create a registry with the default 10-runner suite in dependency order.
    #[must_use]
    pub fn default_suite() -> Self {
        Self {
            tiers: vec![
                // Tier 1: SkillContract (no deps)
                Tier {
                    runners: vec![Box::new(SkillContractRunner)],
                    depends_on: vec![],
                },
                // Tier 2: DeterministicScript (depends on SkillContract)
                Tier {
                    runners: vec![Box::new(DeterministicScriptRunner)],
                    depends_on: vec![SkillArtifactKind::SkillContract],
                },
                // Tier 3: FilingRules + ResolverTrigger (depend on SkillContract)
                Tier {
                    runners: vec![Box::new(FilingRulesRunner), Box::new(ResolverTriggerRunner)],
                    depends_on: vec![SkillArtifactKind::SkillContract],
                },
                // Tier 4: UnitTests + IntegrationTests (depend on DeterministicScript)
                Tier {
                    runners: vec![Box::new(UnitTestRunner), Box::new(IntegrationTestRunner)],
                    depends_on: vec![SkillArtifactKind::DeterministicScript],
                },
                // Tier 5: LlmEvals (depends on SkillContract + DeterministicScript)
                Tier {
                    runners: vec![Box::new(LlmEvalRunner)],
                    depends_on: vec![
                        SkillArtifactKind::SkillContract,
                        SkillArtifactKind::DeterministicScript,
                    ],
                },
                // Tier 6: ResolverEval (depends on ResolverTrigger)
                Tier {
                    runners: vec![Box::new(ResolverEvalRunner)],
                    depends_on: vec![SkillArtifactKind::ResolverTrigger],
                },
                // Tier 7: CheckResolvableAndDry (depends on ResolverTrigger + FilingRules)
                Tier {
                    runners: vec![Box::new(CheckResolvableAndDryRunner)],
                    depends_on: vec![
                        SkillArtifactKind::ResolverTrigger,
                        SkillArtifactKind::FilingRules,
                    ],
                },
                // Tier 8: E2eSmoke (depends on UnitTests + ResolverTrigger)
                Tier {
                    runners: vec![Box::new(E2eSmokeRunner)],
                    depends_on: vec![
                        SkillArtifactKind::UnitTests,
                        SkillArtifactKind::ResolverTrigger,
                    ],
                },
            ],
        }
    }

    /// Run all gates in dependency order.
    ///
    /// If any dependency gate did not pass, downstream gates in that tier are
    /// marked [`SkillifyGateStatus::Blocked`] instead of being executed.
    pub async fn run_all(&self, ctx: &GateRunContext<'_>) -> Vec<GateRunResult> {
        let mut results: Vec<GateRunResult> = Vec::new();

        for tier in &self.tiers {
            let dep_failed = tier.depends_on.iter().any(|dep| {
                results
                    .iter()
                    .any(|r| r.kind == *dep && r.status != SkillifyGateStatus::Passed)
            });

            for runner in &tier.runners {
                if dep_failed {
                    results.push(GateRunResult::blocked(
                        runner.artifact_kind(),
                        "dependency gate failed".to_owned(),
                    ));
                } else {
                    results.push(runner.run(ctx).await);
                }
            }
        }

        results
    }
}
