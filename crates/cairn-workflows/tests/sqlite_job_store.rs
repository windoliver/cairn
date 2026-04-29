//! Integration tests for [`SqliteJobStore`] against a real `SQLite`
//! database with migration 0011 applied.

use std::sync::Arc;

use cairn_core::contract::{
    EnqueueRequest, FailDisposition, JobId, JobKind, JobStore, JobStoreError, RetryPolicy,
};
use cairn_store_sqlite::open_sync;
use cairn_workflows::SqliteJobStore;
use rusqlite::Connection;
use tempfile::TempDir;

const LEASE_MS: i64 = 30_000;

fn open_store() -> (SqliteJobStore, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    // Run migrations through the canonical opener.
    {
        let _conn = open_sync(&db).expect("migrate");
    }
    let conn = Connection::open(&db).expect("reopen for jobs");
    (SqliteJobStore::new(conn), dir)
}

fn req(id: &str, kind: &str) -> EnqueueRequest {
    EnqueueRequest {
        job_id: JobId::new(id),
        kind: JobKind::new(kind),
        payload: b"payload".to_vec(),
        queue_key: None,
        dedupe_key: None,
        not_before_ms: 0,
        retry: RetryPolicy::DEFAULT,
    }
}

#[tokio::test]
async fn enqueue_lease_complete_round_trip() {
    let (store, _dir) = open_store();
    store.enqueue(req("j1", "kind.a")).await.expect("enqueue");
    let leased = store
        .lease("worker-1", 100, LEASE_MS)
        .await
        .expect("lease ok")
        .expect("had work");
    assert_eq!(leased.job_id.as_str(), "j1");
    assert_eq!(leased.attempts, 1);
    assert_eq!(leased.payload, b"payload");
    store
        .complete(&leased.job_id, &leased.lease)
        .await
        .expect("complete");
    // Second call must fail — lease no longer matches.
    let again = store.complete(&leased.job_id, &leased.lease).await;
    assert!(matches!(again, Err(JobStoreError::LeaseLost { .. })));
}

#[tokio::test]
async fn lease_returns_none_when_empty() {
    let (store, _dir) = open_store();
    let none = store
        .lease("worker", 100, LEASE_MS)
        .await
        .expect("empty queue ok");
    assert!(none.is_none());
}

#[tokio::test]
async fn lease_skips_not_yet_eligible() {
    let (store, _dir) = open_store();
    let mut r = req("future", "kind.a");
    r.not_before_ms = 10_000;
    store.enqueue(r).await.expect("enqueue");
    assert!(store.lease("w", 5_000, LEASE_MS).await.unwrap().is_none());
    let leased = store.lease("w", 10_000, LEASE_MS).await.unwrap();
    assert!(leased.is_some());
}

#[tokio::test]
async fn dedupe_key_blocks_duplicate_enqueue() {
    let (store, _dir) = open_store();
    let mut r = req("j1", "kind.a");
    r.dedupe_key = Some("op-1".to_string());
    store.enqueue(r.clone()).await.expect("first");
    let mut dup = req("j2", "kind.a");
    dup.dedupe_key = Some("op-1".to_string());
    let err = store.enqueue(dup).await.unwrap_err();
    match err {
        JobStoreError::DuplicateDedupeKey { kind, dedupe_key } => {
            assert_eq!(kind.as_str(), "kind.a");
            assert_eq!(dedupe_key, "op-1");
        }
        other => panic!("expected DuplicateDedupeKey, got {other:?}"),
    }
}

#[tokio::test]
async fn queue_key_serializes_writers() {
    let (store, _dir) = open_store();
    let mut r1 = req("j1", "kind.a");
    r1.queue_key = Some("q1".to_string());
    let mut r2 = req("j2", "kind.a");
    r2.queue_key = Some("q1".to_string());
    store.enqueue(r1).await.expect("first");
    let err = store.enqueue(r2).await.unwrap_err();
    assert!(matches!(err, JobStoreError::QueueKeyBusy { .. }));
}

#[tokio::test]
async fn fail_with_retry_requeues_until_max_attempts() {
    let (store, _dir) = open_store();
    let mut r = req("j1", "kind.a");
    r.retry = RetryPolicy {
        max_attempts: 2,
        ..RetryPolicy::DEFAULT
    };
    store.enqueue(r).await.expect("enqueue");

    let leased = store.lease("w", 0, LEASE_MS).await.unwrap().unwrap();
    store
        .fail(
            &leased.job_id,
            &leased.lease,
            FailDisposition::Retry,
            "boom",
            0,
        )
        .await
        .expect("retry-fail #1");

    // After backoff, should be re-leasable.
    let leased2 = store
        .lease("w", 60_000, LEASE_MS)
        .await
        .unwrap()
        .expect("requeued");
    assert_eq!(leased2.attempts, 2);
    store
        .fail(
            &leased2.job_id,
            &leased2.lease,
            FailDisposition::Retry,
            "boom2",
            60_000,
        )
        .await
        .expect("retry-fail #2 -> terminal");

    // No more eligible work; row is now `failed`.
    assert!(store.lease("w", 999_999, LEASE_MS).await.unwrap().is_none());
}

#[tokio::test]
async fn fail_permanent_skips_retry() {
    let (store, _dir) = open_store();
    let mut r = req("j1", "kind.a");
    r.retry = RetryPolicy {
        max_attempts: 5,
        ..RetryPolicy::DEFAULT
    };
    store.enqueue(r).await.expect("enqueue");
    let leased = store.lease("w", 0, LEASE_MS).await.unwrap().unwrap();
    store
        .fail(
            &leased.job_id,
            &leased.lease,
            FailDisposition::Permanent,
            "fatal",
            0,
        )
        .await
        .expect("permanent fail");
    assert!(store.lease("w", 999_999, LEASE_MS).await.unwrap().is_none());
}

#[tokio::test]
async fn heartbeat_extends_lease_and_invalidates_old_token() {
    let (store, _dir) = open_store();
    store.enqueue(req("j1", "kind.a")).await.expect("enqueue");
    let leased = store.lease("w", 0, 1_000).await.unwrap().unwrap();
    let original = leased.lease.clone();
    store
        .heartbeat(&leased.job_id, &original, 5_000)
        .await
        .expect("heartbeat");
    // The stale lease token must no longer match.
    let stale_complete = store.complete(&leased.job_id, &original).await;
    assert!(matches!(
        stale_complete,
        Err(JobStoreError::LeaseLost { .. })
    ));
    // But a refreshed token works.
    let refreshed = cairn_core::contract::LeaseToken {
        owner: original.owner.clone(),
        expires_at_ms: 5_000,
    };
    store
        .complete(&leased.job_id, &refreshed)
        .await
        .expect("complete with refreshed token");
}

#[tokio::test]
async fn reap_expired_recovers_orphans_after_restart() {
    // Simulates: process A leased a job and crashed before completing.
    // A reaper sweep on next startup must move it back to queued.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _ = open_sync(&db).expect("migrate");
    }

    {
        let conn = Connection::open(&db).expect("conn1");
        let store = SqliteJobStore::new(conn);
        store
            .enqueue(req("j-orphan", "kind.a"))
            .await
            .expect("enqueue");
        let leased = store
            .lease("crashed-worker", 0, 100)
            .await
            .unwrap()
            .unwrap();
        // Drop without complete — simulates crash. Lease will expire at 100.
        drop(leased);
    }

    {
        let conn = Connection::open(&db).expect("conn2");
        let store = SqliteJobStore::new(conn);
        // Time has advanced past lease expiry.
        let reclaimed = store.reap_expired(10_000).await.expect("reap");
        assert_eq!(reclaimed, 1);
        let leased = store
            .lease("new-worker", 10_000, LEASE_MS)
            .await
            .unwrap()
            .expect("reclaimed job is leasable");
        assert_eq!(leased.job_id.as_str(), "j-orphan");
        assert_eq!(leased.attempts, 2, "attempt count survives restart");
        store
            .complete(&leased.job_id, &leased.lease)
            .await
            .expect("complete after recovery");
    }
}

#[tokio::test]
async fn concurrent_leasers_yield_exactly_one_winner() {
    let (store, _dir) = open_store();
    let store = Arc::new(store);
    store
        .enqueue(req("only-one", "kind.a"))
        .await
        .expect("enqueue");

    let mut handles = Vec::new();
    for i in 0..16 {
        let s = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            s.lease(&format!("worker-{i}"), 0, LEASE_MS).await
        }));
    }

    let mut winners = 0usize;
    for h in handles {
        let res = h.await.expect("join").expect("lease ok");
        if res.is_some() {
            winners += 1;
        }
    }
    assert_eq!(winners, 1, "exactly one worker must lease the row");
}

#[tokio::test]
async fn enqueue_and_lease_under_load() {
    // Smoke: many enqueues then drain.
    let (store, _dir) = open_store();
    let store = Arc::new(store);

    let n = 32;
    let mut h = Vec::new();
    for i in 0..n {
        let s = Arc::clone(&store);
        h.push(tokio::spawn(async move {
            s.enqueue(req(&format!("j{i}"), "kind.a")).await
        }));
    }
    for j in h {
        j.await.expect("join").expect("enqueue");
    }

    let mut drained = 0usize;
    while let Some(leased) = store.lease("w", 1_000, LEASE_MS).await.unwrap() {
        store.complete(&leased.job_id, &leased.lease).await.unwrap();
        drained += 1;
    }
    assert_eq!(drained, n);
}
