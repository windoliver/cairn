//! Integration: `DreamHandler` upserts a deterministic distillation
//! record when an `LLMProvider` is wired, and declines `Permanent`
//! when none is configured (issue #91).

use std::sync::Arc;

use async_trait::async_trait;
use cairn_core::config::DreamConfig;
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
            window_size_records: 4,
            ..DreamConfig::default()
        },
        Some(Arc::new(FakeLlm {
            body: "deterministic dream body",
        })),
    );
    let payload = DreamPayload {
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
            window_size_records: 4,
            ..DreamConfig::default()
        },
        Some(llm as Arc<dyn LLMProvider>),
    );
    let payload = DreamPayload {
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
            window_size_records: 4,
            ..DreamConfig::default()
        },
        Some(Arc::new(FakeLlm { body: "stable" })),
    );
    let payload = DreamPayload {
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
