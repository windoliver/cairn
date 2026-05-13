//! Forget a source turn → `ConsolidationForgetCleanupHandler` tombstones
//! any rolling summary that referenced it.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::config::ConsolidationConfig;
use cairn_core::contract::job_store::{EnqueueRequest, JobId, JobKind, JobStore, RetryPolicy};
use cairn_core::contract::memory_store::{ListArgs, MemoryStore, TombstoneReason};
use cairn_core::domain::record::RecordId;
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_workflows::{
    SqliteJobStore,
    consolidation::{
        ConsolidationForgetCleanupHandler, ConsolidationHandler, ConsolidationPayload,
        ForgetCleanupPayload, CONSOLIDATION_KIND, FORGET_CLEANUP_KIND,
    },
    scheduler::{Clock, HandlerRegistryBuilder, Scheduler, SchedulerConfig, SystemClock},
};
use cairn_test_fixtures::trace::sample_turn_summary;
use tempfile::tempdir;

/// A valid 26-char Crockford base32 ULID for use as session ID.
const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::expect_used,
    reason = "integration test: panics surface broken invariants immediately"
)]
async fn forgetting_source_turn_tombstones_summary() {
    let dir = tempdir().expect("tempdir");
    let mem_path = dir.path().join("cairn.db");

    // The memory store opens + applies all migrations.
    let mem = Arc::new(cairn_store_sqlite::open(&mem_path).await.expect("open memory"));

    // Reuse the same DB file for the jobs store.
    // open_sync runs the full migration chain so SqliteJobStore::new finds the schema.
    let jobs_conn = cairn_store_sqlite::open_sync(&mem_path).expect("open_sync for jobs");
    let jobs: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(jobs_conn).expect("job store"));

    let cfg = ConsolidationConfig::default();
    let registry = HandlerRegistryBuilder::default()
        .with(Arc::new(ConsolidationHandler::new(mem.clone(), cfg)))
        .with(Arc::new(ConsolidationForgetCleanupHandler::new(mem.clone())))
        .build();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let s = Scheduler::start(
        "inc",
        jobs.clone(),
        &registry,
        clock.clone(),
        SchedulerConfig::p0(),
    );

    // Seed: 6 turns then enqueue a consolidation job directly.
    for seq in 1..=6_u32 {
        mem.upsert(&sample_turn_summary(SESSION_ID, seq))
            .await
            .expect("upsert turn");
    }
    jobs.enqueue(EnqueueRequest {
        job_id: JobId::new("c-1"),
        kind: JobKind::new(CONSOLIDATION_KIND),
        payload: ConsolidationPayload {
            session_id: SESSION_ID.to_owned(),
            since_sequence: 0,
        }
        .to_bytes()
        .expect("encode consolidation payload"),
        queue_key: None,
        dedupe_key: None,
        not_before_ms: clock.now_ms(),
        retry: RetryPolicy::DEFAULT,
    })
    .await
    .expect("enqueue consolidation job");

    // Poll until the summary `reasoning` record materialises.
    let mut summary_id: Option<RecordId> = None;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        let page = mem.list(&ListArgs::default()).await.expect("list");
        if let Some(rec) = page
            .records
            .iter()
            .find(|r| r.kind == MemoryKind::Reasoning)
        {
            summary_id = Some(rec.id.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let summary_id = summary_id.expect("consolidation summary should be emitted within 10s");

    // Fetch the first source turn record_id (a String ULID from TurnHeader).
    let first_source_str = mem
        .list_trace_turns(SESSION_ID, 0, 1)
        .await
        .expect("list_trace_turns")
        .into_iter()
        .next()
        .expect("at least one turn")
        .record_id;

    // Parse the source record_id string into a typed RecordId.
    let src_record_id =
        RecordId::parse(first_source_str.clone()).expect("source record_id is a valid ULID");

    // Tombstone the source turn directly (simulating `cairn forget --record`).
    mem.tombstone(&src_record_id, TombstoneReason::Forget)
        .await
        .expect("tombstone source turn");

    // Enqueue the forget-cleanup job so the handler can propagate the tombstone.
    jobs.enqueue(EnqueueRequest {
        job_id: JobId::new("fc-1"),
        kind: JobKind::new(FORGET_CLEANUP_KIND),
        payload: ForgetCleanupPayload {
            forgotten_record_id: first_source_str,
        }
        .to_bytes()
        .expect("encode forget-cleanup payload"),
        queue_key: None,
        dedupe_key: None,
        not_before_ms: clock.now_ms(),
        retry: RetryPolicy::DEFAULT,
    })
    .await
    .expect("enqueue forget-cleanup job");

    // Poll until the summary record is gone (tombstoned → get returns None).
    let mut gone = false;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if mem.get(&summary_id).await.expect("get").is_none() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    s.shutdown().await;
    assert!(
        gone,
        "forget-cleanup handler never tombstoned the orphan summary"
    );
}
