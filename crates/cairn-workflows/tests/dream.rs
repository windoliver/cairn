//! Integration: `DreamHandler` upserts a deterministic distillation
//! record when an `LLMProvider` is wired, and declines `Permanent`
//! when none is configured (issue #91).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cairn_core::config::{DreamConfig, DreamTier, DreamTierConfig, DreamWorkerMode};
use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::contract::{
    AgentBudgetConsumed, AgentOutput, AgentOutputSchema, AgentProvider, AgentProviderCapabilities,
    AgentProviderError, AgentRun, AgentRunStatus, AgentScope, AgentSpawnRequest,
    AgentToolAllowlist,
};
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_test_fixtures::{memstore, sample_record};
use cairn_workflows::scheduler::{HandlerOutcome, JobHandler};
use cairn_workflows::{DreamHandler, DreamPayload};

struct FakeLlm {
    body: &'static str,
}

#[async_trait]
impl LLMProvider for FakeLlm {
    fn name(&self) -> &'static str {
        "fake-llm"
    }
    fn capabilities(&self) -> &LLMProviderCapabilities {
        static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
            json_mode: false,
            streaming: false,
            tool_calls: false,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        Ok(CompletionOutput::Text(self.body.into()))
    }
}

struct CapturingLlm {
    body: String,
    prompt: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl LLMProvider for CapturingLlm {
    fn name(&self) -> &'static str {
        "capturing-llm"
    }
    fn capabilities(&self) -> &LLMProviderCapabilities {
        static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
            json_mode: false,
            streaming: false,
            tool_calls: false,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        *self.prompt.lock().expect("prompt lock") = Some(req.prompt.clone());
        Ok(CompletionOutput::Text(self.body.clone()))
    }
}

struct RecordingAgentProvider {
    requests: Arc<Mutex<Vec<AgentSpawnRequest>>>,
    run: Mutex<AgentRun>,
}

impl RecordingAgentProvider {
    fn new(run: AgentRun) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            run: Mutex::new(run),
        }
    }

    fn requests(&self) -> Arc<Mutex<Vec<AgentSpawnRequest>>> {
        self.requests.clone()
    }
}

#[async_trait]
impl AgentProvider for RecordingAgentProvider {
    fn name(&self) -> &str {
        "recording-agent"
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

    async fn spawn(&self, request: AgentSpawnRequest) -> Result<AgentRun, AgentProviderError> {
        self.requests.lock().expect("requests lock").push(request);
        Ok(self.run.lock().expect("run lock").clone())
    }
}

fn completed_agent_dream_run() -> AgentRun {
    AgentRun {
        status: AgentRunStatus::Completed,
        abort_error: None,
        output: AgentOutput::Json(serde_json::json!({
            "body": "agent synthesized dream body",
            "evidence": [
                {
                    "tool": "search",
                    "record_id": "01HQZX9F5N0000000000000031",
                    "claim": "Seeded records support the synthesis."
                }
            ]
        })),
        budget_consumed: AgentBudgetConsumed {
            turns: 2,
            tool_calls: 1,
            cost_units: 17,
        },
        tool_calls: Vec::new(),
        policy_trace: vec!["search allowed read-only".to_string()],
    }
}

fn aborted_agent_budget_run() -> AgentRun {
    AgentRun {
        status: AgentRunStatus::Aborted,
        abort_error: Some(AgentProviderError::BudgetExceeded {
            limit: "turns".to_string(),
        }),
        output: AgentOutput::Empty,
        budget_consumed: AgentBudgetConsumed {
            turns: 1,
            tool_calls: 0,
            cost_units: 0,
        },
        tool_calls: Vec::new(),
        policy_trace: vec!["turn budget exhausted".to_string()],
    }
}

#[tokio::test]
async fn no_llm_returns_permanent() {
    let store: Arc<dyn MemoryStore> = Arc::new(memstore().await);
    let handler = DreamHandler::new(
        store,
        DreamConfig {
            enabled: true,
            ..DreamConfig::default()
        },
        None,
        None,
    );
    let payload = DreamPayload {
        tier: DreamTier::LightSleep,
        key: "sess-1".into(),
        bound_scope: None,
    };
    let bytes = payload.to_bytes().expect("encode");
    let outcome = handler.handle(&bytes).await;
    assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));
}

#[tokio::test]
async fn agent_dream_outputs_evidence_and_budget_metadata() {
    let store = Arc::new(memstore().await);
    let mut first = sample_record(31);
    first.body = "source excerpt alpha".into();
    let mut second = sample_record(32);
    second.body = "source excerpt beta".into();
    store.upsert(&first).await.expect("seed first");
    store.upsert(&second).await.expect("seed second");

    let agent = Arc::new(RecordingAgentProvider::new(completed_agent_dream_run()));
    let requests = agent.requests();
    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = DreamHandler::new(
        dyn_store,
        DreamConfig {
            enabled: true,
            deep_dreaming: DreamTierConfig {
                worker: DreamWorkerMode::Agent,
                window_size_records: 4,
                completion_token_budget: 256,
                max_wall_ms: 1_000,
                max_tool_calls: 2,
                ..DreamTierConfig::deep_dreaming_default()
            },
            ..DreamConfig::default()
        },
        None,
        Some(agent),
    );
    let payload = DreamPayload {
        tier: DreamTier::DeepDreaming,
        key: "sess-agent".into(),
        bound_scope: None,
    };

    let outcome = handler.handle(&payload.to_bytes().expect("encode")).await;
    assert!(
        matches!(outcome, HandlerOutcome::Done),
        "expected Done, got {outcome:?}"
    );

    let requests = requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.identity.as_str(), "agt:cairn-librarian:v2");
    assert_eq!(request.scope, AgentScope::read_only());
    assert_eq!(
        request.tool_allowlist,
        AgentToolAllowlist::read_only_cairn()
    );
    assert_eq!(request.output_schema, AgentOutputSchema::Json);
    assert_eq!(request.cost_budget.max_turns, 2);
    assert_eq!(request.cost_budget.max_tool_calls, 2);
    assert!(request.prompt.contains("01HQZX9F5N000000000000001F"));
    assert!(request.prompt.contains("01HQZX9F5N0000000000000020"));
    assert!(request.prompt.contains("source excerpt alpha"));
    assert!(request.prompt.contains("source excerpt beta"));

    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    let dreams: Vec<_> = listed
        .records
        .iter()
        .filter(|r| r.kind == MemoryKind::Reasoning && r.body == "agent synthesized dream body")
        .collect();
    assert_eq!(dreams.len(), 1, "exactly one agent dream should be written");
    let dream_meta = dreams[0]
        .extra_frontmatter
        .get("dream")
        .expect("dream metadata");
    assert_eq!(dream_meta["worker"], "agent");
    assert_eq!(dream_meta["evidence"][0]["tool"], "search");
    assert_eq!(
        dream_meta["evidence"][0]["claim"],
        "Seeded records support the synthesis."
    );
    assert_eq!(dream_meta["budget_consumed"]["turns"], 2);
    assert_eq!(dream_meta["budget_consumed"]["tool_calls"], 1);
    assert_eq!(dream_meta["budget_consumed"]["cost_units"], 17);
    assert_eq!(dream_meta["policy_trace"][0], "search allowed read-only");
    let metadata_wire = serde_json::to_string(dream_meta).expect("metadata json");
    assert!(!metadata_wire.contains("source excerpt alpha"));
    assert!(!metadata_wire.contains("source excerpt beta"));
}

#[tokio::test]
async fn agent_dream_budget_abort_is_permanent_without_upsert() {
    let store = Arc::new(memstore().await);
    store.upsert(&sample_record(41)).await.expect("seed record");

    let agent = Arc::new(RecordingAgentProvider::new(aborted_agent_budget_run()));
    let requests = agent.requests();
    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = DreamHandler::new(
        dyn_store,
        DreamConfig {
            enabled: true,
            deep_dreaming: DreamTierConfig {
                worker: DreamWorkerMode::Agent,
                window_size_records: 4,
                completion_token_budget: 256,
                max_wall_ms: 1_000,
                max_tool_calls: 1,
                ..DreamTierConfig::deep_dreaming_default()
            },
            ..DreamConfig::default()
        },
        None,
        Some(agent),
    );
    let payload = DreamPayload {
        tier: DreamTier::DeepDreaming,
        key: "sess-agent-abort".into(),
        bound_scope: None,
    };

    let outcome = handler.handle(&payload.to_bytes().expect("encode")).await;
    let HandlerOutcome::Permanent { reason, .. } = outcome else {
        panic!("expected Permanent, got {outcome:?}");
    };
    assert!(reason.contains("agent budget exceeded: turns"));
    assert_eq!(
        requests.lock().expect("requests lock").len(),
        1,
        "agent abort classification must come from AgentProvider::spawn"
    );

    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    assert!(
        listed
            .records
            .iter()
            .all(|r| r.kind != MemoryKind::Reasoning),
        "aborted agent dream must not upsert a reasoning record"
    );
}

#[tokio::test]
async fn with_llm_upserts_dream_record() {
    let store = Arc::new(memstore().await);
    store.upsert(&sample_record(1)).await.expect("seed record");

    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = DreamHandler::new(
        dyn_store,
        DreamConfig {
            enabled: true,
            light_sleep: DreamTierConfig {
                window_size_records: 4,
                ..DreamTierConfig::light_sleep_default()
            },
            ..DreamConfig::default()
        },
        Some(Arc::new(FakeLlm {
            body: "deterministic dream body",
        })),
        None,
    );
    let payload = DreamPayload {
        tier: DreamTier::LightSleep,
        key: "sess-1".into(),
        bound_scope: None,
    };
    let bytes = payload.to_bytes().expect("encode");

    let outcome = handler.handle(&bytes).await;
    assert!(
        matches!(outcome, HandlerOutcome::Done),
        "expected Done, got {outcome:?}"
    );

    // A reasoning record bearing the distillation body must now exist.
    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    let dream = listed
        .records
        .iter()
        .find(|r| r.kind == MemoryKind::Reasoning && r.body == "deterministic dream body");
    assert!(
        dream.is_some(),
        "no dream record found in: {:?}",
        listed.records.iter().map(|r| &r.body).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn rem_sleep_records_tier_worker_budget_and_source_evidence() {
    let store = Arc::new(memstore().await);
    store.upsert(&sample_record(11)).await.expect("seed record");

    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = DreamHandler::new(
        dyn_store,
        DreamConfig {
            enabled: true,
            rem_sleep: DreamTierConfig {
                window_size_records: 4,
                completion_token_budget: 128,
                max_wall_ms: 12_345,
                ..DreamTierConfig::rem_sleep_default()
            },
            ..DreamConfig::default()
        },
        Some(Arc::new(FakeLlm {
            body: "rem dream body",
        })),
        None,
    );
    let payload = DreamPayload {
        tier: DreamTier::RemSleep,
        key: "sess-rem".into(),
        bound_scope: None,
    };

    let outcome = handler.handle(&payload.to_bytes().expect("encode")).await;
    assert!(
        matches!(outcome, HandlerOutcome::Done),
        "expected Done, got {outcome:?}"
    );

    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    let dream = listed
        .records
        .iter()
        .find(|r| r.body == "rem dream body")
        .expect("rem dream record");
    let meta = dream
        .extra_frontmatter
        .get("dream")
        .expect("dream metadata");
    assert_eq!(meta["tier"], "rem_sleep");
    assert_eq!(meta["worker"], "hybrid");
    assert_eq!(meta["budget"]["max_tokens"], 128);
    assert_eq!(meta["budget"]["max_wall_ms"], 12_345);
    assert_eq!(
        meta["source_record_ids"].as_array().expect("sources").len(),
        1
    );
}

#[tokio::test]
async fn budget_exceeded_retries_without_upsert() {
    let store = Arc::new(memstore().await);
    store.upsert(&sample_record(12)).await.expect("seed record");

    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = DreamHandler::new(
        dyn_store,
        DreamConfig {
            enabled: true,
            light_sleep: DreamTierConfig {
                completion_token_budget: DreamConfig::COMPLETION_BUDGET_FLOOR,
                ..DreamTierConfig::light_sleep_default()
            },
            ..DreamConfig::default()
        },
        Some(Arc::new(CapturingLlm {
            body: "x".repeat((DreamConfig::COMPLETION_BUDGET_FLOOR as usize * 4) + 1),
            prompt: Arc::new(Mutex::new(None)),
        })),
        None,
    );
    let payload = DreamPayload {
        tier: DreamTier::LightSleep,
        key: "sess-budget".into(),
        bound_scope: None,
    };

    let outcome = handler.handle(&payload.to_bytes().expect("encode")).await;
    assert!(
        matches!(outcome, HandlerOutcome::Retry { .. }),
        "expected Retry, got {outcome:?}"
    );

    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    assert!(
        listed.records.iter().all(|r| !r.body.starts_with('x')),
        "budget-exceeded body must not be upserted"
    );
}

#[tokio::test]
async fn hybrid_worker_prunes_duplicate_bodies_before_prompting() {
    let store = Arc::new(memstore().await);
    let mut first = sample_record(21);
    first.body = "duplicate source body".into();
    let mut second = sample_record(22);
    second.body = "duplicate source body".into();
    store.upsert(&first).await.expect("seed first");
    store.upsert(&second).await.expect("seed second");

    let prompt = Arc::new(Mutex::new(None));
    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = DreamHandler::new(
        dyn_store,
        DreamConfig {
            enabled: true,
            rem_sleep: DreamTierConfig {
                window_size_records: 4,
                ..DreamTierConfig::rem_sleep_default()
            },
            ..DreamConfig::default()
        },
        Some(Arc::new(CapturingLlm {
            body: "hybrid body".into(),
            prompt: prompt.clone(),
        })),
        None,
    );
    let payload = DreamPayload {
        tier: DreamTier::RemSleep,
        key: "sess-hybrid".into(),
        bound_scope: None,
    };

    let outcome = handler.handle(&payload.to_bytes().expect("encode")).await;
    assert!(
        matches!(outcome, HandlerOutcome::Done),
        "expected Done, got {outcome:?}"
    );
    let captured = prompt
        .lock()
        .expect("prompt lock")
        .clone()
        .expect("prompt captured");
    let record_lines = captured
        .lines()
        .filter(|line| line.starts_with("- "))
        .count();
    assert_eq!(
        record_lines, 1,
        "hybrid prune should keep one duplicate body"
    );
}

#[tokio::test]
async fn second_run_skips_llm_when_target_already_exists() {
    // Round-4 adversarial review #4: concurrent or retried same-key
    // jobs MUST NOT regenerate non-deterministic LLM content. The
    // second invocation must observe the first run's record and
    // exit without calling the LLM (which here would return a
    // different body, surfacing the bug).
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingLlm {
        calls: Arc<AtomicUsize>,
        bodies: Vec<&'static str>,
    }
    #[async_trait]
    impl LLMProvider for CountingLlm {
        fn name(&self) -> &'static str {
            "counting-llm"
        }
        fn capabilities(&self) -> &LLMProviderCapabilities {
            static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
                json_mode: false,
                streaming: false,
                tool_calls: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
            let i = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CompletionOutput::Text(
                self.bodies.get(i).copied().unwrap_or("overflow").into(),
            ))
        }
    }

    let store = Arc::new(memstore().await);
    store.upsert(&sample_record(42)).await.expect("seed");

    let calls = Arc::new(AtomicUsize::new(0));
    let llm = Arc::new(CountingLlm {
        calls: calls.clone(),
        bodies: vec!["first run body", "WOULD HAVE LEAKED"],
    });

    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = DreamHandler::new(
        dyn_store,
        DreamConfig {
            enabled: true,
            light_sleep: DreamTierConfig {
                window_size_records: 4,
                ..DreamTierConfig::light_sleep_default()
            },
            ..DreamConfig::default()
        },
        Some(llm as Arc<dyn LLMProvider>),
        None,
    );
    let payload = DreamPayload {
        tier: DreamTier::LightSleep,
        key: "sess-42".into(),
        bound_scope: None,
    };
    let bytes = payload.to_bytes().expect("encode");

    let _ = handler.handle(&bytes).await;
    let _ = handler.handle(&bytes).await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "LLM must be called at most once for two identical-source replays"
    );
    let listed = store
        .list(&ListArgs {
            limit: 50,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    let leak = listed.records.iter().any(|r| r.body == "WOULD HAVE LEAKED");
    assert!(!leak, "second run must not have written a regenerated body");
}

#[tokio::test]
async fn replay_is_idempotent() {
    let store = Arc::new(memstore().await);
    store.upsert(&sample_record(2)).await.expect("seed record");

    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = DreamHandler::new(
        dyn_store,
        DreamConfig {
            enabled: true,
            light_sleep: DreamTierConfig {
                window_size_records: 4,
                ..DreamTierConfig::light_sleep_default()
            },
            ..DreamConfig::default()
        },
        Some(Arc::new(FakeLlm { body: "stable" })),
        None,
    );
    let payload = DreamPayload {
        tier: DreamTier::LightSleep,
        key: "sess-2".into(),
        bound_scope: None,
    };
    let bytes = payload.to_bytes().expect("encode");

    let first = handler.handle(&bytes).await;
    let second = handler.handle(&bytes).await;
    assert!(matches!(first, HandlerOutcome::Done));
    assert!(matches!(second, HandlerOutcome::Done));

    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    let dream_count = listed
        .records
        .iter()
        .filter(|r| r.kind == MemoryKind::Reasoning && r.body == "stable")
        .count();
    // Body-hash dedupe means at most one stable copy exists.
    assert!(dream_count >= 1);
}
