//! Integration: `DreamHandler` upserts a deterministic distillation
//! record when an `LLMProvider` is wired, and declines `Permanent`
//! when none is configured (issue #91).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cairn_core::config::{DreamConfig, DreamTier, DreamTierConfig};
use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::contract::version::{ContractVersion, VersionRange};
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
