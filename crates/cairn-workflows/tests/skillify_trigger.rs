#![allow(missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use cairn_core::config::{DreamConfig, DreamTier, DreamTierConfig};
use cairn_core::contract::job_store::{JobKind, JobStore};
use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_test_fixtures::{memstore, sample_record};
use cairn_workflows::scheduler::{HandlerOutcome, HandlerRegistryBuilder, JobHandler};
use cairn_workflows::{
    DreamHandler, DreamPayload, SKILLIFY_KIND, SkillifyEnqueueDecision, SkillifyHandler,
    SkillifyPayload, SkillifyTrigger, SqliteJobStore, enqueue_skillify,
};
use rusqlite::Connection;

struct FakeLlm;

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
        Ok(CompletionOutput::Text("deep dream body".to_owned()))
    }
}

fn job_store() -> Arc<dyn JobStore> {
    let conn = Connection::open_in_memory().expect("conn");
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    Arc::new(SqliteJobStore::new(conn).expect("store"))
}

#[test]
fn registry_accepts_skillify_handler_kind() {
    let handler = Arc::new(SkillifyHandler::new(PathBuf::from("."), None));
    let registry = HandlerRegistryBuilder::default().with(handler).build();

    let found = registry
        .lookup(&cairn_core::contract::job_store::JobKind::new(
            SKILLIFY_KIND,
        ))
        .expect("handler");

    assert_eq!(found.kind().as_str(), SKILLIFY_KIND);
}

#[tokio::test]
async fn enqueue_is_idempotent_for_same_key_and_token() {
    let s = job_store();
    let first: SkillifyEnqueueDecision = enqueue_skillify(
        &*s,
        SkillifyTrigger::Explicit,
        "session-1",
        "turn-7",
        1_000,
        None,
        vec!["01HQZX9F5N0000000000000001".to_owned()],
    )
    .await
    .expect("first");
    let second = enqueue_skillify(
        &*s,
        SkillifyTrigger::Explicit,
        "session-1",
        "turn-7",
        1_000,
        None,
        vec!["01HQZX9F5N0000000000000001".to_owned()],
    )
    .await
    .expect("second");

    assert_eq!(first, second);
}

#[tokio::test]
async fn enqueue_derives_candidate_id_from_source_set() {
    let s = job_store();
    let first = enqueue_skillify(
        &*s,
        SkillifyTrigger::Explicit,
        "session-1",
        "turn-7",
        1_000,
        None,
        vec!["01HQZX9F5N0000000000000001".to_owned()],
    )
    .await
    .expect("first");
    let second = enqueue_skillify(
        &*s,
        SkillifyTrigger::Explicit,
        "session-1",
        "turn-8",
        1_000,
        None,
        vec!["01HQZX9F5N0000000000000002".to_owned()],
    )
    .await
    .expect("second");

    let SkillifyEnqueueDecision::Enqueued { job_id: first_id } = first;
    let SkillifyEnqueueDecision::Enqueued { job_id: second_id } = second;
    let first = s
        .lease_specific(
            &first_id,
            &JobKind::new(SKILLIFY_KIND),
            "test-worker",
            1_000,
            30_000,
        )
        .await
        .expect("lease first")
        .expect("first queued");
    s.complete(&first.job_id, &first.lease, 1_001)
        .await
        .expect("complete first");
    let second = s
        .lease_specific(
            &second_id,
            &JobKind::new(SKILLIFY_KIND),
            "test-worker",
            1_002,
            30_000,
        )
        .await
        .expect("lease second")
        .expect("second queued");

    let first_payload = SkillifyPayload::from_bytes(&first.payload).expect("first payload");
    let second_payload = SkillifyPayload::from_bytes(&second.payload).expect("second payload");

    assert!(first_payload.candidate_id.is_some());
    assert!(second_payload.candidate_id.is_some());
    assert_ne!(first_payload.candidate_id, second_payload.candidate_id);
}

#[test]
fn payload_round_trips_json() {
    let payload = SkillifyPayload {
        trigger: SkillifyTrigger::DeepDream,
        key: "vault".to_owned(),
        candidate_id: Some("skc_fixture".to_owned()),
        bound_scope: None,
        source_record_ids: vec!["01HQZX9F5N0000000000000001".to_owned()],
    };

    let bytes = payload.to_bytes().expect("encode");
    let back = SkillifyPayload::from_bytes(&bytes).expect("decode");
    assert_eq!(payload, back);
}

#[tokio::test]
async fn deep_dream_strategy_success_enqueues_skillify() {
    let store = Arc::new(memstore().await);
    let mut strategy = sample_record(701);
    strategy.kind = MemoryKind::StrategySuccess;
    strategy.body = "successful hotfix deployment procedure".to_owned();
    store
        .upsert(&strategy)
        .await
        .expect("seed strategy success");

    let jobs = job_store();
    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = DreamHandler::new(
        dyn_store,
        DreamConfig {
            enabled: true,
            deep_dreaming: DreamTierConfig {
                window_size_records: 4,
                ..DreamTierConfig::deep_dreaming_default()
            },
            ..DreamConfig::default()
        },
        Some(Arc::new(FakeLlm)),
    )
    .with_skillify_jobs(jobs.clone());
    let payload = DreamPayload {
        tier: DreamTier::DeepDreaming,
        key: "vault".to_owned(),
        bound_scope: None,
    };

    let outcome = handler.handle(&payload.to_bytes().expect("encode")).await;

    assert!(
        matches!(outcome, HandlerOutcome::Done),
        "expected Done, got {outcome:?}"
    );
    let leased = jobs
        .lease("test-worker", i64::MAX - 60_000, 30_000)
        .await
        .expect("lease")
        .expect("skillify job queued");
    assert_eq!(leased.kind.as_str(), SKILLIFY_KIND);
    let payload = SkillifyPayload::from_bytes(&leased.payload).expect("skillify payload");
    assert_eq!(payload.trigger, SkillifyTrigger::DeepDream);
    assert_eq!(payload.key, "vault");
    assert_eq!(
        payload.source_record_ids,
        vec![strategy.id.as_str().to_owned()]
    );
}
