//! Issue #92 — `fail()` × `max_attempts` must persist `failure_class`
//! and `dead_letter_at_ms` on the terminal branch (spec §4.5).
//!
//! Walks a job through enqueue → lease → heartbeat → fail twice with
//! `FailureClass::Transient`, where the second fail trips the retry
//! ceiling and forces a terminal `state = 'failed'` transition. Then
//! reads the row through `SqliteJobStore::raw_inspect` and asserts the
//! new columns are populated.

use cairn_core::contract::job_store::{
    EnqueueRequest, FailDisposition, FailureClass, JobId, JobKind, JobStore, RetryPolicy,
};
use cairn_workflows::SqliteJobStore;
use rusqlite::Connection;

fn open_store() -> SqliteJobStore {
    let conn = Connection::open_in_memory().expect("open in-memory");
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    SqliteJobStore::new(conn).expect("init store")
}

#[tokio::test]
async fn fail_to_exhaustion_writes_dead_letter_columns() {
    let store = open_store();

    // max_attempts = 2 so the second fail() exhausts the budget and
    // forces the terminal branch even though the handler returned
    // FailDisposition::Retry.
    let policy = RetryPolicy {
        max_attempts: 2,
        base_backoff_ms: 1,
        backoff_multiplier: 1,
        max_backoff_ms: 1,
    };
    (&store as &dyn JobStore)
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-dead"),
            kind: JobKind::new("test.dl"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            retry: policy,
        })
        .await
        .expect("enqueue");

    for attempt in 1..=2_i64 {
        let now_ms = attempt * 100;
        let leased = (&store as &dyn JobStore)
            .lease("w-0", now_ms, 1_000)
            .await
            .expect("lease")
            .expect("leased Some");
        // Heartbeat so the run consumes an execution attempt (lease_started=1).
        (&store as &dyn JobStore)
            .heartbeat(&leased.job_id, &leased.lease, now_ms + 1, now_ms + 1_000)
            .await
            .expect("heartbeat");
        (&store as &dyn JobStore)
            .fail(
                &leased.job_id,
                &leased.lease,
                FailDisposition::Retry,
                FailureClass::Transient,
                &format!("attempt {attempt} bombed"),
                now_ms + 2,
            )
            .await
            .expect("fail");
    }

    let (state, class, dl_at) = store
        .raw_inspect(
            "SELECT state, failure_class, dead_letter_at_ms \
               FROM workflow_jobs WHERE job_id = 'j-dead'",
        )
        .expect("inspect");
    assert_eq!(state, "failed");
    assert_eq!(class.as_deref(), Some("transient"));
    assert!(
        dl_at.is_some() && dl_at.unwrap_or(0) > 0,
        "dead_letter_at_ms must be populated on the terminal branch: {dl_at:?}"
    );
}

#[tokio::test]
async fn retry_branch_stamps_failure_class_but_not_dead_letter() {
    // Single retryable fail before exhaustion: failure_class is set
    // (so observers can read it back through lease()), but the row
    // remains alive (state = 'queued') and `dead_letter_at_ms` stays
    // NULL — the row is not dead-lettered yet.
    let store = open_store();
    let policy = RetryPolicy {
        max_attempts: 5,
        base_backoff_ms: 1,
        backoff_multiplier: 1,
        max_backoff_ms: 1,
    };
    (&store as &dyn JobStore)
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-retry"),
            kind: JobKind::new("test.dl"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            retry: policy,
        })
        .await
        .expect("enqueue");
    let leased = (&store as &dyn JobStore)
        .lease("w-0", 100, 1_000)
        .await
        .expect("lease")
        .expect("leased");
    (&store as &dyn JobStore)
        .heartbeat(&leased.job_id, &leased.lease, 101, 1_100)
        .await
        .expect("heartbeat");
    (&store as &dyn JobStore)
        .fail(
            &leased.job_id,
            &leased.lease,
            FailDisposition::Retry,
            FailureClass::Transient,
            "transient blip",
            102,
        )
        .await
        .expect("fail");
    let (state, class, dl_at) = store
        .raw_inspect(
            "SELECT state, failure_class, dead_letter_at_ms \
               FROM workflow_jobs WHERE job_id = 'j-retry'",
        )
        .expect("inspect");
    assert_eq!(state, "queued");
    assert_eq!(class.as_deref(), Some("transient"));
    assert_eq!(dl_at, None, "retry branch must not stamp dead_letter_at_ms");
}

#[tokio::test]
async fn complete_writes_completed_at_ms() {
    // `complete()` must stamp the new `completed_at_ms` column (spec
    // §4.9) so the lint last-success lookup has something to read.
    let store = open_store();
    (&store as &dyn JobStore)
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-done"),
            kind: JobKind::new("test.dl"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            retry: RetryPolicy::DEFAULT,
        })
        .await
        .expect("enqueue");
    let leased = (&store as &dyn JobStore)
        .lease("w-0", 200, 1_000)
        .await
        .expect("lease")
        .expect("leased");
    (&store as &dyn JobStore)
        .heartbeat(&leased.job_id, &leased.lease, 201, 1_200)
        .await
        .expect("heartbeat");
    (&store as &dyn JobStore)
        .complete(&leased.job_id, &leased.lease, 202)
        .await
        .expect("complete");

    // SELECT shape: (state, failure_class, completed_at_ms). The
    // dead-letter column stays NULL on a `done` row.
    let (state, class, completed_at) = store
        .raw_inspect(
            "SELECT state, failure_class, completed_at_ms \
               FROM workflow_jobs WHERE job_id = 'j-done'",
        )
        .expect("inspect");
    assert_eq!(state, "done");
    assert_eq!(class, None, "failure_class is null on a successful row");
    assert_eq!(completed_at, Some(202));
}

#[tokio::test]
async fn lease_propagates_persisted_failure_class() {
    // A row that failed once with Transient then got re-leased must
    // surface `LeasedJob.failure_class = Some(Transient)` so the
    // worker can branch on its prior failure history (spec §4.4).
    let store = open_store();
    (&store as &dyn JobStore)
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-prop"),
            kind: JobKind::new("test.dl"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            retry: RetryPolicy::DEFAULT,
        })
        .await
        .expect("enqueue");
    let first = (&store as &dyn JobStore)
        .lease("w-0", 300, 1_000)
        .await
        .expect("lease")
        .expect("leased");
    assert_eq!(first.failure_class, None, "first lease has no history");
    (&store as &dyn JobStore)
        .heartbeat(&first.job_id, &first.lease, 301, 1_300)
        .await
        .expect("heartbeat");
    (&store as &dyn JobStore)
        .fail(
            &first.job_id,
            &first.lease,
            FailDisposition::Retry,
            FailureClass::Transient,
            "transient blip",
            302,
        )
        .await
        .expect("fail");

    // Re-lease. The lease query must propagate the stored class.
    let second = (&store as &dyn JobStore)
        .lease("w-0", 1_500, 1_000)
        .await
        .expect("release")
        .expect("released Some");
    assert_eq!(second.failure_class, Some(FailureClass::Transient));
}
