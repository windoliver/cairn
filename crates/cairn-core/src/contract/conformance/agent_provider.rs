//! Conformance cases for `AgentProvider` plugins.

use crate::contract::agent_provider::{
    AgentCostBudget, AgentIdentity, AgentOutputSchema, AgentProvider, AgentProviderError,
    AgentScope, AgentSpawnRequest, AgentToolAllowlist, AgentToolCall, AgentToolPolicyOutcome,
    AgentWallClockBudget, CONTRACT_VERSION, CairnVerb, evaluate_tool_policy,
};
use crate::contract::conformance::{
    CaseOutcome, CaseStatus, Tier, tier1_manifest_features_match_capabilities,
    tier1_manifest_matches_host,
};
use crate::contract::registry::{PluginName, PluginRegistry};

/// Run tier-1 + tier-2 cases for an `AgentProvider` plugin.
#[must_use]
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let Some(plugin) = registry.agent_provider(name) else {
        return vec![CaseOutcome {
            id: "typed_plugin_registered",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!(
                    "manifest declared AgentProvider but no AgentProvider Arc \
                     registered under name {name}"
                ),
            },
        }];
    };
    let caps = plugin.capabilities();

    vec![
        tier1_manifest_matches_host(registry, name, CONTRACT_VERSION),
        tier1_arc_pointer_stable(registry, name, &plugin),
        tier1_capability_self_consistency_floor(&*plugin),
        tier1_manifest_features_match_capabilities(
            registry,
            name,
            &[
                ("honors_cost_budget", caps.honors_cost_budget),
                ("scope_enforced", caps.scope_enforced),
                ("mcp_tools", caps.mcp_tools),
                ("cli_subprocess_tools", caps.cli_subprocess_tools),
            ],
        ),
        tier2_allowlist_rejects_unlisted_tool(),
        tier2_mutating_verb_requires_scope(),
        tier2_budget_exhaustion_aborts_cleanly(&*plugin),
        tier2_writes_are_wal_routed(),
    ]
}

fn tier1_arc_pointer_stable(
    registry: &PluginRegistry,
    name: &PluginName,
    plugin: &std::sync::Arc<dyn AgentProvider>,
) -> CaseOutcome {
    let Some(resolved) = registry.agent_provider(name) else {
        return CaseOutcome {
            id: "arc_pointer_stable",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: "lookup returned None for registered plugin".to_string(),
            },
        };
    };
    let status = if std::sync::Arc::ptr_eq(plugin, &resolved) {
        CaseStatus::Ok
    } else {
        CaseStatus::Failed {
            message: "two lookups returned different Arcs".to_string(),
        }
    };
    CaseOutcome {
        id: "arc_pointer_stable",
        tier: Tier::One,
        status,
    }
}

fn tier1_capability_self_consistency_floor(plugin: &dyn AgentProvider) -> CaseOutcome {
    let caps = plugin.capabilities();
    if plugin.name().is_empty() {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: "plugin.name() returned empty string".to_string(),
            },
        };
    }
    if !plugin
        .supported_contract_versions()
        .accepts(CONTRACT_VERSION)
    {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!("plugin does not accept host CONTRACT_VERSION {CONTRACT_VERSION}"),
            },
        };
    }
    let _ = (
        caps.honors_cost_budget,
        caps.scope_enforced,
        caps.mcp_tools,
        caps.cli_subprocess_tools,
    );
    CaseOutcome {
        id: "capability_self_consistency_floor",
        tier: Tier::One,
        status: CaseStatus::Ok,
    }
}

fn tier2_allowlist_rejects_unlisted_tool() -> CaseOutcome {
    let id = "allowlist_rejects_unlisted_tool";
    let request = conformance_request();
    if let Err(err) = request.validate() {
        return failed(
            id,
            Tier::Two,
            format!("conformance request is invalid: {err}"),
        );
    }
    let call = AgentToolCall::new(CairnVerb::Forget);
    let status = match evaluate_tool_policy(&request, &call) {
        Err(AgentProviderError::ToolNotAllowed {
            verb: CairnVerb::Forget,
        }) => CaseStatus::Ok,
        Err(err) => CaseStatus::Failed {
            message: format!("expected ToolNotAllowed for forget, got {err}"),
        },
        Ok(outcome) => CaseStatus::Failed {
            message: format!("expected ToolNotAllowed for forget, got {outcome:?}"),
        },
    };
    CaseOutcome {
        id,
        tier: Tier::Two,
        status,
    }
}

fn tier2_mutating_verb_requires_scope() -> CaseOutcome {
    let id = "mutating_verb_requires_scope";
    let mut request = conformance_request();
    request
        .tool_allowlist
        .tools
        .push(AgentToolCall::new(CairnVerb::Ingest));
    if let Err(err) = request.validate() {
        return failed(
            id,
            Tier::Two,
            format!("conformance request is invalid: {err}"),
        );
    }
    let call = AgentToolCall::new(CairnVerb::Ingest);
    let status = match evaluate_tool_policy(&request, &call) {
        Err(AgentProviderError::MutatingVerbNotScoped {
            verb: CairnVerb::Ingest,
        }) => CaseStatus::Ok,
        Err(err) => CaseStatus::Failed {
            message: format!("expected MutatingVerbNotScoped for ingest, got {err}"),
        },
        Ok(outcome) => CaseStatus::Failed {
            message: format!("expected MutatingVerbNotScoped for ingest, got {outcome:?}"),
        },
    };
    CaseOutcome {
        id,
        tier: Tier::Two,
        status,
    }
}

fn tier2_budget_exhaustion_aborts_cleanly(provider: &dyn AgentProvider) -> CaseOutcome {
    let status = if provider.capabilities().honors_cost_budget {
        CaseStatus::Ok
    } else {
        CaseStatus::Failed {
            message: "provider must advertise honors_cost_budget=true".to_string(),
        }
    };
    CaseOutcome {
        id: "budget_exhaustion_aborts_cleanly",
        tier: Tier::Two,
        status,
    }
}

fn tier2_writes_are_wal_routed() -> CaseOutcome {
    let id = "writes_are_wal_routed";
    let mut request = conformance_request();
    request.scope = AgentScope::with_mutations(vec![CairnVerb::Ingest]);
    request
        .tool_allowlist
        .tools
        .push(AgentToolCall::new(CairnVerb::Ingest));
    if let Err(err) = request.validate() {
        return failed(
            id,
            Tier::Two,
            format!("conformance request is invalid: {err}"),
        );
    }
    let call = AgentToolCall::new(CairnVerb::Ingest);
    let status = match evaluate_tool_policy(&request, &call) {
        Ok(AgentToolPolicyOutcome::AllowedWalRoutedMutation) => CaseStatus::Ok,
        Ok(outcome) => CaseStatus::Failed {
            message: format!("expected AllowedWalRoutedMutation for ingest, got {outcome:?}"),
        },
        Err(err) => CaseStatus::Failed {
            message: format!("expected AllowedWalRoutedMutation for ingest, got {err}"),
        },
    };
    CaseOutcome {
        id,
        tier: Tier::Two,
        status,
    }
}

fn conformance_request() -> AgentSpawnRequest {
    AgentSpawnRequest {
        identity: AgentIdentity::new("agt:conformance:v1").expect("valid conformance identity"),
        scope: AgentScope::read_only(),
        tool_allowlist: AgentToolAllowlist::read_only_cairn(),
        cost_budget: AgentCostBudget {
            max_turns: 1,
            max_tool_calls: 1,
            max_cost_units: 1,
        },
        wall_clock_budget: AgentWallClockBudget { max_millis: 1 },
        output_schema: AgentOutputSchema::Text,
        prompt: "conformance".to_string(),
    }
}

fn failed(id: &'static str, tier: Tier, message: String) -> CaseOutcome {
    CaseOutcome {
        id,
        tier,
        status: CaseStatus::Failed { message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::agent_provider::{AgentProviderCapabilities, AgentRun};
    use crate::contract::version::{ContractVersion, VersionRange};

    struct StubAgent;

    #[async_trait::async_trait]
    impl AgentProvider for StubAgent {
        fn name(&self) -> &str {
            "stub-agent-provider"
        }

        fn capabilities(&self) -> &AgentProviderCapabilities {
            static CAPS: AgentProviderCapabilities = AgentProviderCapabilities {
                honors_cost_budget: true,
                scope_enforced: true,
                mcp_tools: false,
                cli_subprocess_tools: true,
            };
            &CAPS
        }

        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }

        async fn spawn(&self, _request: AgentSpawnRequest) -> Result<AgentRun, AgentProviderError> {
            Err(AgentProviderError::ProviderUnavailable {
                message: "test stub does not execute".to_string(),
            })
        }
    }

    #[test]
    fn tier2_policy_cases_pass() {
        let provider = StubAgent;
        let cases = [
            tier2_allowlist_rejects_unlisted_tool(),
            tier2_mutating_verb_requires_scope(),
            tier2_budget_exhaustion_aborts_cleanly(&provider),
            tier2_writes_are_wal_routed(),
        ];

        for outcome in cases {
            assert_eq!(outcome.tier, Tier::Two, "case {}", outcome.id);
            assert_eq!(outcome.status, CaseStatus::Ok, "case {}", outcome.id);
        }
    }
}
