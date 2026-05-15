//! 200 turns in one session → summary body must respect `token_budget`
//! and the summary must list `source_record_ids` for the window count.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::config::ConsolidationConfig;
use cairn_core::contract::job_store::{EnqueueRequest, JobId, JobKind, JobStore, RetryPolicy};
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_test_fixtures::trace::sample_turn_summary;
use cairn_workflows::{
    SqliteJobStore,
    consolidation::{CONSOLIDATION_KIND, ConsolidationHandler, ConsolidationPayload},
    scheduler::{Clock, HandlerRegistryBuilder, Scheduler, SchedulerConfig, SystemClock},
};
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::expect_used,
    reason = "integration test: panics surface broken invariants immediately"
)]
async fn long_session_summary_fits_within_token_budget() {
    let dir = tempdir().expect("tempdir");
    let mem_path = dir.path().join("cairn.db");

    // The memory store opens + applies all migrations (including 0020).
    let mem = Arc::new(
        cairn_store_sqlite::open(&mem_path)
            .await
            .expect("open memory"),
    );

    // Reuse the same DB file for the jobs store.
    // open_sync runs the full migration runner (same schema).
    let jobs_conn =
        cairn_store_sqlite::open_sync(&mem_path).expect("open_sync for jobs connection");
    let jobs: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(jobs_conn).expect("job store"));

    let cfg = ConsolidationConfig {
        window_size_turns: 32,
        token_budget: 200,
        ..ConsolidationConfig::default()
    };
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

    // A valid 26-char Crockford base32 ULID for use as session ID.
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    // Insert 200 turns. The fixture hardcodes salience = 0.5 which clears
    // the default salience_floor of 0.4 so all 200 are eligible.
    for seq in 1..=200_u32 {
        mem.upsert(&sample_turn_summary(session, seq))
            .await
            .expect("upsert");
    }

    // Enqueue the job directly (bypassing enqueue_if_due) to guarantee a
    // single job fires against all 200 turns with since_sequence = 0.
    let payload_bytes = ConsolidationPayload {
        session_id: session.to_owned(),
        since_sequence: 0,
        bound_scope: None,
    }
    .to_bytes()
    .expect("encode payload");

    jobs.enqueue(EnqueueRequest {
        job_id: JobId::new("c-long"),
        kind: JobKind::new(CONSOLIDATION_KIND),
        payload: payload_bytes,
        queue_key: None,
        dedupe_key: None,
        not_before_ms: clock.now_ms(),
        retry: RetryPolicy::DEFAULT,
    })
    .await
    .expect("enqueue");

    // Poll until a `Reasoning` record appears (generous 10 s deadline).
    let start = std::time::Instant::now();
    let mut found = None;
    while start.elapsed() < Duration::from_secs(10) {
        let listed = mem.list(&ListArgs::default()).await.expect("list");
        if let Some(r) = listed
            .records
            .iter()
            .find(|r| r.kind == MemoryKind::Reasoning)
        {
            found = Some(r.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    s.shutdown().await;
    let r = found.expect("summary must be emitted within 10 s");

    // Body length must respect token_budget * 4 chars (per the char-per-token
    // approximation in draft.rs) plus a small truncation-marker allowance.
    assert!(
        r.body.len() <= 200 * 4 + 8,
        "body length {} exceeds budget ({})",
        r.body.len(),
        200 * 4 + 8
    );

    // The source_record_ids list must match the window cap (32), not 200,
    // because pick_window takes at most window_size_turns entries.
    let src = r
        .extra_frontmatter
        .get("consolidation")
        .and_then(|c| c.get("source_record_ids"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        src.len(),
        32,
        "summary should link exactly window_size_turns sources, got {}",
        src.len()
    );
}
