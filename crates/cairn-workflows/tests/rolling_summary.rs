//! End-to-end: many `turn_summary` writes → `ConsolidationHandler` runs
//! via the live scheduler → reasoning record materializes for the
//! session, linking back to its source turns.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::config::ConsolidationConfig;
use cairn_core::contract::job_store::JobStore;
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_workflows::{
    SqliteJobStore,
    consolidation::{enqueue_if_due, ConsolidationHandler},
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
async fn long_session_emits_rolling_summary_without_blocking_turns() {
    let dir = tempdir().expect("tempdir");
    let mem_path = dir.path().join("cairn.db");

    // The memory store opens + applies all migrations (including 0020).
    let mem = Arc::new(cairn_store_sqlite::open(&mem_path).await.expect("open memory"));

    // Reuse the same DB file for the jobs store.
    // open_sync runs the full migration runner (same schema), so
    // SqliteJobStore::new can verify the 0020 schema objects.
    let jobs_conn =
        cairn_store_sqlite::open_sync(&mem_path).expect("open_sync for jobs connection");
    let jobs: Arc<dyn JobStore> =
        Arc::new(SqliteJobStore::new(jobs_conn).expect("job store"));

    let cfg = ConsolidationConfig::default();
    let h = Arc::new(ConsolidationHandler::new(mem.clone(), cfg));
    let registry = HandlerRegistryBuilder::default().with(h).build();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let s = Scheduler::start(
        "inc",
        jobs.clone(),
        &registry,
        clock.clone(),
        SchedulerConfig::p0(),
    );

    // Simulate 12 turns landing.
    for seq in 1..=12_u32 {
        mem.upsert(&sample_turn_summary(SESSION_ID, seq))
            .await
            .expect("upsert turn");
        let _ = enqueue_if_due(jobs.as_ref(), &cfg, SESSION_ID, seq, 0, clock.now_ms()).await;
    }

    // Poll for the summary to appear (generous 10 s deadline).
    let start = std::time::Instant::now();
    let mut found = false;
    while start.elapsed() < Duration::from_secs(10) {
        let listed = mem
            .list(&ListArgs::default())
            .await
            .expect("list records");
        if listed.records.iter().any(|r| {
            r.kind == MemoryKind::Reasoning
                && r.scope.session_id.as_deref() == Some(SESSION_ID)
        }) {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    s.shutdown().await;
    assert!(found, "rolling summary was never emitted within 10 s");
}
