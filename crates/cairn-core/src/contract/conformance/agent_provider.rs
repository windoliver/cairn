//! Conformance cases for `AgentProvider` plugins.

use std::sync::Arc;

use crate::contract::agent_provider::{
    AgentCostBudget, AgentIdentity, AgentOutputSchema, AgentProvider, AgentProviderError,
    AgentRunStatus, AgentScope, AgentSpawnRequest, AgentToolAllowlist, AgentToolCall,
    AgentToolPolicyOutcome, AgentWallClockBudget, CONTRACT_VERSION, CairnVerb,
};
use crate::contract::conformance::{
    CaseOutcome, CaseStatus, Tier, tier1_manifest_features_match_capabilities,
    tier1_manifest_matches_host,
};
use crate::contract::registry::{PluginName, PluginRegistry};

/// Prompt marker for the allowlist conformance case.
pub const ALLOWLIST_REJECTS_UNLISTED_TOOL_PROMPT: &str =
    "agent_provider.conformance.allowlist_rejects_unlisted_tool";
/// Prompt marker for the mutating-scope conformance case.
pub const MUTATING_VERB_REQUIRES_SCOPE_PROMPT: &str =
    "agent_provider.conformance.mutating_verb_requires_scope";
/// Prompt marker for the budget exhaustion conformance case.
pub const BUDGET_EXHAUSTION_ABORTS_CLEANLY_PROMPT: &str =
    "agent_provider.conformance.budget_exhaustion_aborts_cleanly";
/// Prompt marker for the WAL-routed mutation conformance case.
pub const WRITES_ARE_WAL_ROUTED_PROMPT: &str = "agent_provider.conformance.writes_are_wal_routed";

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
        tier2_allowlist_rejects_unlisted_tool(plugin.clone()),
        tier2_mutating_verb_requires_scope(plugin.clone()),
        tier2_budget_exhaustion_aborts_cleanly(plugin.clone()),
        tier2_writes_are_wal_routed(plugin.clone()),
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

fn tier2_allowlist_rejects_unlisted_tool(provider: Arc<dyn AgentProvider>) -> CaseOutcome {
    let id = "allowlist_rejects_unlisted_tool";
    if !provider.capabilities().scope_enforced {
        return failed(
            id,
            Tier::Two,
            "provider must advertise scope_enforced=true to verify tool policy enforcement"
                .to_string(),
        );
    }
    let request = match conformance_request(ALLOWLIST_REJECTS_UNLISTED_TOOL_PROMPT) {
        Ok(request) => request,
        Err(message) => return failed(id, Tier::Two, message),
    };
    match spawn_conformance(provider, request) {
        Ok(run)
            if run.status == AgentRunStatus::Aborted
                && matches!(
                    run.abort_error,
                    Some(AgentProviderError::ToolNotAllowed {
                        verb: CairnVerb::Forget
                    })
                )
                && run.tool_calls.iter().any(|attempt| {
                    attempt.call.verb == CairnVerb::Forget
                        && attempt.outcome == AgentToolPolicyOutcome::Denied
                }) =>
        {
            ok(id)
        }
        Ok(run) => failed(
            id,
            Tier::Two,
            format!("expected denied forget tool attempt, got {run:?}"),
        ),
        Err(message) => failed(id, Tier::Two, message),
    }
}

fn tier2_mutating_verb_requires_scope(provider: Arc<dyn AgentProvider>) -> CaseOutcome {
    let id = "mutating_verb_requires_scope";
    if !provider.capabilities().scope_enforced {
        return failed(
            id,
            Tier::Two,
            "provider must advertise scope_enforced=true to verify tool policy enforcement"
                .to_string(),
        );
    }
    let mut request = match conformance_request(MUTATING_VERB_REQUIRES_SCOPE_PROMPT) {
        Ok(request) => request,
        Err(message) => return failed(id, Tier::Two, message),
    };
    request
        .tool_allowlist
        .tools
        .push(AgentToolCall::new(CairnVerb::Ingest));
    match spawn_conformance(provider, request) {
        Ok(run)
            if run.status == AgentRunStatus::Aborted
                && matches!(
                    run.abort_error,
                    Some(AgentProviderError::MutatingVerbNotScoped {
                        verb: CairnVerb::Ingest
                    })
                )
                && run.tool_calls.iter().any(|attempt| {
                    attempt.call.verb == CairnVerb::Ingest
                        && attempt.outcome == AgentToolPolicyOutcome::Denied
                }) =>
        {
            ok(id)
        }
        Ok(run) => failed(
            id,
            Tier::Two,
            format!("expected unscoped ingest denial, got {run:?}"),
        ),
        Err(message) => failed(id, Tier::Two, message),
    }
}

fn tier2_budget_exhaustion_aborts_cleanly(provider: Arc<dyn AgentProvider>) -> CaseOutcome {
    let id = "budget_exhaustion_aborts_cleanly";
    if !provider.capabilities().honors_cost_budget {
        return failed(
            id,
            Tier::Two,
            "provider must advertise honors_cost_budget=true to verify budget enforcement"
                .to_string(),
        );
    }
    let mut request = match conformance_request(BUDGET_EXHAUSTION_ABORTS_CLEANLY_PROMPT) {
        Ok(request) => request,
        Err(message) => return failed(id, Tier::Two, message),
    };
    request.cost_budget.max_tool_calls = 1;
    match spawn_conformance(provider, request) {
        Ok(run)
            if run.status == AgentRunStatus::Aborted
                && matches!(
                    run.abort_error,
                    Some(AgentProviderError::BudgetExceeded { ref limit })
                        if limit == "tool_calls"
                )
                && run.budget_consumed.tool_calls == 1 =>
        {
            ok(id)
        }
        Ok(run) => failed(
            id,
            Tier::Two,
            format!("expected clean tool-call budget abort, got {run:?}"),
        ),
        Err(message) => failed(id, Tier::Two, message),
    }
}

fn tier2_writes_are_wal_routed(provider: Arc<dyn AgentProvider>) -> CaseOutcome {
    let id = "writes_are_wal_routed";
    if !provider.capabilities().scope_enforced {
        return failed(
            id,
            Tier::Two,
            "provider must advertise scope_enforced=true to verify tool policy enforcement"
                .to_string(),
        );
    }
    let mut request = match conformance_request(WRITES_ARE_WAL_ROUTED_PROMPT) {
        Ok(request) => request,
        Err(message) => return failed(id, Tier::Two, message),
    };
    request.scope = AgentScope::with_mutations(vec![CairnVerb::Ingest]);
    request
        .tool_allowlist
        .tools
        .push(AgentToolCall::new(CairnVerb::Ingest));
    match spawn_conformance(provider, request) {
        Ok(run)
            if run.tool_calls.iter().any(|attempt| {
                attempt.call.verb == CairnVerb::Ingest
                    && attempt.outcome == AgentToolPolicyOutcome::AllowedWalRoutedMutation
            }) =>
        {
            ok(id)
        }
        Ok(run) => failed(
            id,
            Tier::Two,
            format!("expected WAL-routed ingest tool attempt, got {run:?}"),
        ),
        Err(message) => failed(id, Tier::Two, message),
    }
}

fn conformance_request(prompt: &'static str) -> Result<AgentSpawnRequest, String> {
    Ok(AgentSpawnRequest {
        identity: AgentIdentity::new("agt:conformance:v1")
            .map_err(|err| format!("conformance identity is invalid: {err}"))?,
        scope: AgentScope::read_only(),
        tool_allowlist: AgentToolAllowlist::read_only_cairn(),
        cost_budget: AgentCostBudget {
            max_turns: 3,
            max_tool_calls: 2,
            max_cost_units: 32,
        },
        wall_clock_budget: AgentWallClockBudget { max_millis: 1_000 },
        output_schema: AgentOutputSchema::Text,
        prompt: prompt.to_string(),
    })
}

fn spawn_conformance(
    provider: Arc<dyn AgentProvider>,
    request: AgentSpawnRequest,
) -> Result<crate::contract::agent_provider::AgentRun, String> {
    if let Err(err) = request.validate() {
        return Err(format!("conformance request is invalid: {err}"));
    }
    let output_schema = request.output_schema.clone();
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|err| format!("failed to build conformance runtime: {err}"))?;
        runtime
            .block_on(provider.spawn(request))
            .map_err(|err| format!("provider returned pre-run error: {err}"))
    });
    let run = handle
        .join()
        .map_err(|_| "provider conformance runtime panicked".to_string())??;
    run.validate(&output_schema)
        .map_err(|err| format!("provider returned invalid run: {err}"))?;
    Ok(run)
}

fn ok(id: &'static str) -> CaseOutcome {
    CaseOutcome {
        id,
        tier: Tier::Two,
        status: CaseStatus::Ok,
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
    use crate::contract::agent_provider::{
        AgentBudgetConsumed, AgentOutput, AgentProviderCapabilities, AgentRun, AgentToolAttempt,
        evaluate_tool_policy,
    };
    use crate::contract::manifest::PluginManifest;
    use crate::contract::version::{ContractVersion, VersionRange};

    struct StubAgent {
        caps: AgentProviderCapabilities,
    }

    impl StubAgent {
        fn new(caps: AgentProviderCapabilities) -> Self {
            Self { caps }
        }
    }

    #[async_trait::async_trait]
    impl AgentProvider for StubAgent {
        fn name(&self) -> &'static str {
            "stub-agent-provider"
        }

        fn capabilities(&self) -> &AgentProviderCapabilities {
            &self.caps
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

    struct ConformingAgent {
        caps: AgentProviderCapabilities,
    }

    #[async_trait::async_trait]
    impl AgentProvider for ConformingAgent {
        fn name(&self) -> &'static str {
            "stub-agent-provider"
        }

        fn capabilities(&self) -> &AgentProviderCapabilities {
            &self.caps
        }

        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }

        async fn spawn(&self, request: AgentSpawnRequest) -> Result<AgentRun, AgentProviderError> {
            request.validate()?;
            match request.prompt.as_str() {
                ALLOWLIST_REJECTS_UNLISTED_TOOL_PROMPT => {
                    let call = AgentToolCall::new(CairnVerb::Forget);
                    let err = evaluate_tool_policy(&request, &call).expect_err("forget denied");
                    Ok(aborted_for_policy(call, &err))
                }
                MUTATING_VERB_REQUIRES_SCOPE_PROMPT => {
                    let call = AgentToolCall::new(CairnVerb::Ingest);
                    let err = evaluate_tool_policy(&request, &call).expect_err("ingest denied");
                    Ok(aborted_for_policy(call, &err))
                }
                BUDGET_EXHAUSTION_ABORTS_CLEANLY_PROMPT => Ok(AgentRun {
                    status: AgentRunStatus::Aborted,
                    abort_error: Some(AgentProviderError::BudgetExceeded {
                        limit: "tool_calls".to_string(),
                    }),
                    output: AgentOutput::Empty,
                    budget_consumed: AgentBudgetConsumed {
                        turns: 2,
                        tool_calls: 1,
                        cost_units: 1,
                    },
                    tool_calls: Vec::new(),
                    policy_trace: vec!["budget:tool_calls".to_string()],
                }),
                WRITES_ARE_WAL_ROUTED_PROMPT => {
                    let call = AgentToolCall::new(CairnVerb::Ingest);
                    let outcome = evaluate_tool_policy(&request, &call).expect("ingest is scoped");
                    Ok(AgentRun {
                        status: AgentRunStatus::Completed,
                        abort_error: None,
                        output: AgentOutput::Text("ok".to_string()),
                        budget_consumed: AgentBudgetConsumed {
                            turns: 1,
                            tool_calls: 1,
                            cost_units: 1,
                        },
                        tool_calls: vec![AgentToolAttempt {
                            call,
                            outcome,
                            reason: format!("{outcome:?}"),
                        }],
                        policy_trace: vec!["Ingest:AllowedWalRoutedMutation".to_string()],
                    })
                }
                other => Err(AgentProviderError::InvalidRequest {
                    message: format!("unknown conformance prompt {other}"),
                }),
            }
        }
    }

    fn aborted_for_policy(call: AgentToolCall, err: &AgentProviderError) -> AgentRun {
        AgentRun {
            status: AgentRunStatus::Aborted,
            abort_error: Some(err.clone()),
            output: AgentOutput::Empty,
            budget_consumed: AgentBudgetConsumed {
                turns: 1,
                tool_calls: 0,
                cost_units: 0,
            },
            tool_calls: vec![AgentToolAttempt {
                call,
                outcome: AgentToolPolicyOutcome::Denied,
                reason: err.to_string(),
            }],
            policy_trace: vec!["denied".to_string()],
        }
    }

    fn truthful_caps() -> AgentProviderCapabilities {
        AgentProviderCapabilities {
            honors_cost_budget: true,
            scope_enforced: true,
            mcp_tools: false,
            cli_subprocess_tools: true,
        }
    }

    fn registry_with(caps: AgentProviderCapabilities) -> (PluginRegistry, PluginName) {
        registry_with_provider(std::sync::Arc::new(StubAgent::new(caps)), caps)
    }

    fn registry_with_provider(
        provider: std::sync::Arc<dyn AgentProvider>,
        caps: AgentProviderCapabilities,
    ) -> (PluginRegistry, PluginName) {
        let mut registry = PluginRegistry::new();
        let name = PluginName::new("stub-agent-provider").expect("valid plugin name");
        let manifest = PluginManifest::parse_toml(&format!(
            r#"
name = "stub-agent-provider"
contract = "AgentProvider"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
honors_cost_budget = {}
scope_enforced = {}
mcp_tools = {}
cli_subprocess_tools = {}
"#,
            caps.honors_cost_budget, caps.scope_enforced, caps.mcp_tools, caps.cli_subprocess_tools
        ))
        .expect("manifest parses");
        registry
            .register_agent_provider_with_manifest(name.clone(), manifest, provider)
            .expect("stub registers");
        (registry, name)
    }

    fn outcome<'a>(outcomes: &'a [CaseOutcome], id: &str) -> &'a CaseOutcome {
        outcomes
            .iter()
            .find(|outcome| outcome.id == id)
            .unwrap_or_else(|| panic!("missing outcome {id}"))
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

    fn host_policy_allowlist_rejects_unlisted_tool() -> CaseOutcome {
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

    fn host_policy_mutating_verb_requires_scope() -> CaseOutcome {
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

    fn host_policy_writes_are_wal_routed() -> CaseOutcome {
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

    #[test]
    fn host_policy_cases_pass() {
        let cases = [
            host_policy_allowlist_rejects_unlisted_tool(),
            host_policy_mutating_verb_requires_scope(),
            host_policy_writes_are_wal_routed(),
        ];

        for outcome in cases {
            assert_eq!(outcome.tier, Tier::Two, "case {}", outcome.id);
            assert_eq!(outcome.status, CaseStatus::Ok, "case {}", outcome.id);
        }
    }

    #[test]
    fn run_reports_provider_behavior_ok_for_conforming_agent() {
        let (registry, name) = registry_with_provider(
            std::sync::Arc::new(ConformingAgent {
                caps: truthful_caps(),
            }),
            truthful_caps(),
        );
        let outcomes = run(&registry, &name);

        for id in [
            "allowlist_rejects_unlisted_tool",
            "mutating_verb_requires_scope",
            "writes_are_wal_routed",
            "budget_exhaustion_aborts_cleanly",
        ] {
            assert_eq!(outcome(&outcomes, id).status, CaseStatus::Ok, "case {id}");
        }
    }

    #[test]
    fn run_fails_provider_behavior_cases_when_provider_unavailable() {
        let (registry, name) = registry_with(truthful_caps());
        let outcomes = run(&registry, &name);

        for id in [
            "allowlist_rejects_unlisted_tool",
            "mutating_verb_requires_scope",
            "writes_are_wal_routed",
            "budget_exhaustion_aborts_cleanly",
        ] {
            let CaseStatus::Failed { message } = &outcome(&outcomes, id).status else {
                panic!("case {id} should fail");
            };
            assert!(
                message.contains("provider returned pre-run error"),
                "case {id} message was {message}"
            );
        }
    }

    #[test]
    fn run_fails_provider_behavior_cases_when_required_capabilities_are_false() {
        let (registry, name) = registry_with(AgentProviderCapabilities {
            honors_cost_budget: false,
            scope_enforced: false,
            mcp_tools: false,
            cli_subprocess_tools: true,
        });
        let outcomes = run(&registry, &name);

        for id in [
            "allowlist_rejects_unlisted_tool",
            "mutating_verb_requires_scope",
            "writes_are_wal_routed",
        ] {
            let CaseStatus::Failed { message } = &outcome(&outcomes, id).status else {
                panic!("case {id} should fail");
            };
            assert!(
                message.contains("scope_enforced=true"),
                "case {id} message was {message}"
            );
        }

        let CaseStatus::Failed { message } =
            &outcome(&outcomes, "budget_exhaustion_aborts_cleanly").status
        else {
            panic!("budget case should fail");
        };
        assert!(
            message.contains("honors_cost_budget=true"),
            "message was {message}"
        );
    }

    #[test]
    fn run_conformance_routes_agent_provider_to_runner() {
        let (registry, name) = registry_with(truthful_caps());
        let outcomes = crate::contract::conformance::run_conformance_for_plugin(&registry, &name);

        assert!(
            outcomes
                .iter()
                .all(|outcome| outcome.id != "no_conformance_runner"),
            "AgentProvider should route to its conformance runner"
        );
        assert!(
            outcomes
                .iter()
                .any(|outcome| outcome.id == "allowlist_rejects_unlisted_tool"),
            "AgentProvider conformance output should include the stable tier-2 case"
        );
    }
}
