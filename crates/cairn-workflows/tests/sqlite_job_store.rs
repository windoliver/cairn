//! Integration tests for [`SqliteJobStore`] against a real `SQLite`
//! database with migration 0020 applied.

use std::sync::Arc;

use cairn_core::contract::{
    EnqueueRequest, FailDisposition, FailureClass, JobId, JobKind, JobStore, JobStoreError,
    RetryPolicy,
};
use cairn_store_sqlite::open_sync;
use cairn_workflows::{SqliteJobStore, SqliteJobStoreInitError};
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
    (SqliteJobStore::new(conn).expect("init store"), dir)
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
        .complete(&leased.job_id, &leased.lease, 200)
        .await
        .expect("complete");
    // Second call must fail — lease no longer matches.
    let again = store.complete(&leased.job_id, &leased.lease, 200).await;
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
async fn enqueue_leased_inserts_directly_into_leased_state() {
    // Issue #92 round-5 finding 5.2: SqliteJobStore::enqueue_leased
    // must produce a row in `state = 'leased'` from the moment of
    // insert — never visible as `queued` to any scheduler. Asserts:
    //  - The returned LeasedJob carries the supplied job_id/kind and
    //    a non-empty lease nonce.
    //  - The persisted row has state='leased', delivery_count=1,
    //    attempts=0, lease_owner matching the call, and the lease
    //    nonce/deadline that the LeasedJob reports.
    //  - The row is NOT lease-eligible via the generic lease() path
    //    (the partial-unique queue_key index would normally allow a
    //    re-lease, but enqueue_leased has already grabbed the lease).
    let (store, dir) = open_store();
    let req = req("j-enqueue-leased", "kind.eq");
    let owner = "atomic-owner-1";
    let now_ms = 1_700_000_000_000_i64;
    let lease_ms = 30_000_i64;

    let leased = store
        .enqueue_leased(req.clone(), owner, now_ms, lease_ms)
        .await
        .expect("enqueue_leased should succeed on fresh row");
    assert_eq!(leased.job_id.as_str(), "j-enqueue-leased");
    assert_eq!(leased.kind.as_str(), "kind.eq");
    assert_eq!(leased.attempts, 1, "worker-visible attempts = 1");
    assert_eq!(leased.lease.owner, owner);
    assert!(
        !leased.lease.nonce.is_empty(),
        "lease nonce must be non-empty"
    );
    assert_eq!(leased.lease.expires_at_ms, now_ms + lease_ms);
    assert!(
        leased.failure_class.is_none(),
        "no failure_class on fresh row"
    );
    assert_eq!(leased.dedupe_key, None);

    // Inspect the persisted row through a fresh connection — the row
    // must be exactly as the LeasedJob describes, with delivery_count=1
    // (the lease counts as one delivery) and attempts=0 (matches the
    // generic atomic_lease's "persisted=0, surfaced=1" contract).
    let db = dir.path().join("cairn.db");
    let probe = Connection::open(&db).expect("reopen for inspection");
    let row: (String, i64, i64, String, String, i64) = probe
        .query_row(
            "SELECT state, attempts, delivery_count, lease_owner, lease_nonce, lease_expires_at \
             FROM workflow_jobs WHERE job_id = ?1",
            rusqlite::params!["j-enqueue-leased"],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            },
        )
        .expect("row exists");
    assert_eq!(row.0, "leased", "row must be in state='leased'");
    assert_eq!(row.1, 0, "persisted attempts = 0");
    assert_eq!(row.2, 1, "delivery_count = 1 (the active lease)");
    assert_eq!(row.3, owner);
    assert_eq!(row.4, leased.lease.nonce);
    assert_eq!(row.5, now_ms + lease_ms);

    // A concurrent generic lease() must NOT find this row — it's
    // already in 'leased' state, so the queued-only WHERE filters it
    // out. This is the load-bearing property: no scheduler can claim
    // the row between enqueue and our heartbeat.
    let racing = store
        .lease("foreign-worker", now_ms, lease_ms)
        .await
        .expect("lease query");
    assert!(
        racing.is_none(),
        "enqueue_leased must hide the row from concurrent lease() calls"
    );

    // The row IS reachable via complete() with the returned lease,
    // proving the LeaseToken is genuine and matches the row.
    store
        .complete(&leased.job_id, &leased.lease, now_ms + 1)
        .await
        .expect("complete with returned lease");
}

#[tokio::test]
async fn enqueue_leased_rejects_duplicate_dedupe_key() {
    // Issue #92 round-5 finding 5.2: enqueue_leased must surface
    // duplicate (kind, dedupe_key) collisions the same way enqueue()
    // does — the partial-unique index covers `state IN
    // ('queued','leased','done')` so a leased row blocks a second
    // enqueue (whether leased OR queued) under the same key.
    let (store, _dir) = open_store();
    let mut first = req("j-dup-1", "kind.dup");
    first.dedupe_key = Some("op-dup".to_string());
    let leased = store
        .enqueue_leased(first, "owner-a", 1_000_000_000, 30_000)
        .await
        .expect("first enqueue_leased ok");
    drop(leased);

    let mut second = req("j-dup-2", "kind.dup");
    second.dedupe_key = Some("op-dup".to_string());
    let err = store
        .enqueue_leased(second, "owner-b", 1_000_000_001, 30_000)
        .await
        .expect_err("duplicate dedupe must be rejected");
    match err {
        JobStoreError::DuplicateDedupeKey { kind, dedupe_key } => {
            assert_eq!(kind.as_str(), "kind.dup");
            assert_eq!(dedupe_key, "op-dup");
        }
        other => panic!("expected DuplicateDedupeKey, got {other:?}"),
    }
}

#[tokio::test]
async fn enqueue_leased_rejects_queue_key() {
    // Issue #92 round-6 finding 6.1: `enqueue_leased` must refuse
    // `queue_key.is_some()` requests. Migration 0020's partial-unique
    // index only enforces "at most one leased row per queue_key" — it
    // does NOT enforce FIFO ordering between leased and queued
    // siblings sharing a key. FIFO is a runtime invariant of
    // `atomic_lease`. Accepting a leased-on-insert with `queue_key`
    // would let it overtake an older queued sibling. Defend the
    // invariant at the contract layer so future callers can't slip
    // past it silently.
    let (store, _dir) = open_store();
    let mut req = req("j-qkey", "kind.qk");
    req.queue_key = Some("k1".into());
    let err = store
        .enqueue_leased(req, "owner-q", 1_000_000, 30_000)
        .await
        .expect_err("queue_key on enqueue_leased must be rejected");
    assert!(
        matches!(err, JobStoreError::EnqueueLeasedQueueKey),
        "expected EnqueueLeasedQueueKey, got {err:?}"
    );
}

#[tokio::test]
async fn enqueue_leased_rejects_non_positive_lease_duration() {
    // Defensive guard: a zero/negative lease duration is a caller bug,
    // not a no-op. The store rejects with InvalidLeaseDeadline before
    // mutating anything.
    let (store, _dir) = open_store();
    let r = req("j-bad-dur", "kind.bad");
    let err = store
        .enqueue_leased(r.clone(), "owner-x", 0, 0)
        .await
        .expect_err("zero lease_duration must reject");
    assert!(matches!(err, JobStoreError::InvalidLeaseDeadline { .. }));
    let err = store
        .enqueue_leased(r, "owner-x", 0, -1)
        .await
        .expect_err("negative lease_duration must reject");
    assert!(matches!(err, JobStoreError::InvalidLeaseDeadline { .. }));
}

#[tokio::test]
async fn queue_key_serializes_writers() {
    // Brief §10 v0.1: jobs sharing a queue_key serialize through the
    // lease — they are durably queued, not rejected at enqueue. Both
    // enqueues succeed; lease hands them out one at a time in FIFO
    // order.
    let (store, _dir) = open_store();
    let mut r1 = req("j1", "kind.a");
    r1.queue_key = Some("q1".to_string());
    let mut r2 = req("j2", "kind.a");
    r2.queue_key = Some("q1".to_string());
    store.enqueue(r1).await.expect("first enqueued");
    store
        .enqueue(r2)
        .await
        .expect("second enqueued (queued behind)");

    // First lease drains j1.
    let leased = store.lease("w", 0, LEASE_MS).await.unwrap().unwrap();
    assert_eq!(leased.job_id.as_str(), "j1");
    // While j1 is leased, j2 must NOT be leasable — same queue_key.
    assert!(
        store.lease("w2", 0, LEASE_MS).await.unwrap().is_none(),
        "j2 must wait for j1 to finish before becoming leasable"
    );
    // Once j1 completes, j2 becomes leasable.
    store
        .complete(&leased.job_id, &leased.lease, 1)
        .await
        .expect("complete j1");
    let next = store.lease("w2", 2, LEASE_MS).await.unwrap().unwrap();
    assert_eq!(next.job_id.as_str(), "j2");
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
            FailureClass::Transient,
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
            FailureClass::Transient,
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
            FailureClass::Validation,
            "fatal",
            0,
        )
        .await
        .expect("permanent fail");
    assert!(store.lease("w", 999_999, LEASE_MS).await.unwrap().is_none());
}

#[tokio::test]
async fn heartbeat_keeps_token_valid_for_complete() {
    // Regression: heartbeat used to overwrite the CAS key, so callers
    // following the contract (reuse the same `LeaseToken`) would get
    // `LeaseLost` after the first successful heartbeat. The token's
    // nonce must stay stable across heartbeats.
    let (store, _dir) = open_store();
    store.enqueue(req("j1", "kind.a")).await.expect("enqueue");
    let leased = store.lease("w", 0, 1_000).await.unwrap().unwrap();
    store
        .heartbeat(&leased.job_id, &leased.lease, 500, 5_000)
        .await
        .expect("heartbeat");
    store
        .heartbeat(&leased.job_id, &leased.lease, 4_500, 9_000)
        .await
        .expect("second heartbeat with same token");
    store
        .complete(&leased.job_id, &leased.lease, 8_500)
        .await
        .expect("complete with original token after heartbeats");
}

#[tokio::test]
async fn stolen_lease_rejects_writes() {
    let (store, _dir) = open_store();
    store.enqueue(req("j1", "kind.a")).await.expect("enqueue");
    let leased = store.lease("w", 0, 1_000).await.unwrap().unwrap();
    // Forge a token with the right owner but a bogus nonce.
    let forged = cairn_core::contract::LeaseToken {
        owner: leased.lease.owner.clone(),
        nonce: "forged-nonce".to_string(),
        expires_at_ms: leased.lease.expires_at_ms,
    };
    let err = store
        .complete(&leased.job_id, &forged, 500)
        .await
        .unwrap_err();
    assert!(matches!(err, JobStoreError::LeaseLost { .. }));
}

#[tokio::test]
async fn expired_lease_cannot_complete_or_heartbeat_before_reap() {
    // Regression: writes used to match only nonce, so a worker that
    // woke up after its deadline could still extend or commit until
    // the reaper happened to run. The CAS path now enforces
    // `lease_expires_at > now_ms` synchronously.
    let (store, _dir) = open_store();
    store.enqueue(req("j1", "kind.a")).await.expect("enqueue");
    let leased = store.lease("w", 0, 1_000).await.unwrap().unwrap();
    // now_ms past lease deadline (1000), but reaper has not run.
    let hb = store
        .heartbeat(&leased.job_id, &leased.lease, 2_000, 9_000)
        .await;
    assert!(matches!(hb, Err(JobStoreError::LeaseLost { .. })));
    let cp = store.complete(&leased.job_id, &leased.lease, 2_000).await;
    assert!(matches!(cp, Err(JobStoreError::LeaseLost { .. })));
    let fl = store
        .fail(
            &leased.job_id,
            &leased.lease,
            FailDisposition::Retry,
            FailureClass::Transient,
            "late",
            2_000,
        )
        .await;
    assert!(matches!(fl, Err(JobStoreError::LeaseLost { .. })));
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
        let store = SqliteJobStore::new(conn).expect("init store");
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
        let store = SqliteJobStore::new(conn).expect("init store");
        // Time has advanced past lease expiry.
        let reclaimed = store.reap_expired(10_000).await.expect("reap");
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].job_id.as_str(), "j-orphan");
        // Reaper applies per-row backoff; jump past it before re-leasing.
        let after_backoff = 1_000_000_i64;
        let leased = store
            .lease("new-worker", after_backoff, LEASE_MS)
            .await
            .unwrap()
            .expect("reclaimed job is leasable after backoff");
        assert_eq!(leased.job_id.as_str(), "j-orphan");
        // First lease never heartbeated (worker crashed pre-start), so
        // the persisted attempt counter stayed at 0; the new lease now
        // shows `attempts = 1` (the in-flight try).
        assert_eq!(
            leased.attempts, 1,
            "never-started crash returns full retry budget"
        );
        store
            .complete(&leased.job_id, &leased.lease, after_backoff + 1)
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
async fn reap_terminates_exhausted_orphans_instead_of_requeueing() {
    // Regression: the reaper used to put exhausted orphans back into
    // `queued`, where the next lease would try `attempts = attempts +
    // 1` and trip the `attempts <= max_attempts` CHECK — poisoning the
    // queue. After fix, exhausted orphans should land in `failed` and
    // later jobs must lease normally.
    let (store, _dir) = open_store();
    let mut r = req("doomed", "kind.a");
    r.retry = RetryPolicy {
        max_attempts: 1,
        ..RetryPolicy::DEFAULT
    };
    store.enqueue(r).await.expect("enqueue doomed");
    store
        .enqueue(req("normal", "kind.a"))
        .await
        .expect("enqueue normal");

    let leased = store
        .lease("crash-worker", 0, 100)
        .await
        .unwrap()
        .expect("doomed leased");
    assert_eq!(leased.job_id.as_str(), "doomed");
    assert_eq!(leased.attempts, 1);
    // Worker actually starts (heartbeat marks lease_started=1) and
    // consumes its only attempt; this models a worker that began
    // executing on its last allowed attempt and then crashed.
    store
        .heartbeat(&leased.job_id, &leased.lease, 50, 100)
        .await
        .expect("heartbeat marks started");
    drop(leased); // simulate crash after starting

    let reclaimed = store.reap_expired(10_000).await.expect("reap");
    assert_eq!(reclaimed.len(), 1);

    // The exhausted job must NOT be leasable; the next lease must go to
    // `normal`.
    let next = store
        .lease("w", 10_000, LEASE_MS)
        .await
        .unwrap()
        .expect("subsequent lease");
    assert_eq!(next.job_id.as_str(), "normal");
}

#[tokio::test]
async fn custom_retry_policy_is_persisted_and_honored_after_restart() {
    // Regression: the store used to reconstruct LeasedJob.retry from
    // RetryPolicy::DEFAULT, dropping any non-default backoff that
    // callers passed at enqueue time. The retry policy must round-trip
    // through SQLite — including across a fresh Connection.
    let custom = RetryPolicy {
        max_attempts: 4,
        base_backoff_ms: 250,
        backoff_multiplier: 3,
        max_backoff_ms: 7_500,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _ = open_sync(&db).expect("migrate");
    }
    {
        let store = SqliteJobStore::new(Connection::open(&db).unwrap()).expect("init store");
        let mut r = req("j", "kind.a");
        r.retry = custom;
        store.enqueue(r).await.expect("enqueue");
    }
    {
        let store = SqliteJobStore::new(Connection::open(&db).unwrap()).expect("init store");
        let leased = store.lease("w", 0, LEASE_MS).await.unwrap().unwrap();
        assert_eq!(leased.retry, custom, "retry policy survives restart");

        // Fail once with retry; next_run_at must reflect the custom base
        // backoff (250ms), not the default 1000ms.
        store
            .fail(
                &leased.job_id,
                &leased.lease,
                FailDisposition::Retry,
                FailureClass::Transient,
                "boom",
                0,
            )
            .await
            .expect("retry-fail");
        // 100ms after fail: shouldn't be eligible yet (custom base = 250).
        assert!(store.lease("w", 100, LEASE_MS).await.unwrap().is_none());
        // 300ms after: eligible.
        let again = store
            .lease("w", 300, LEASE_MS)
            .await
            .unwrap()
            .expect("re-leasable after custom backoff");
        assert_eq!(again.job_id.as_str(), "j");
    }
}

#[tokio::test]
async fn unmigrated_db_rejected_at_construction() {
    // Regression: SqliteJobStore used to wrap any Connection without
    // checking migrations, so a misconfigured startup would only fail
    // on the first enqueue/lease at runtime. The constructor now
    // probes the schema and fails fast.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("empty.db");
    let conn = Connection::open(&db).expect("open empty");
    let Err(err) = SqliteJobStore::new(conn) else {
        panic!("must reject empty db");
    };
    assert!(matches!(
        err,
        SqliteJobStoreInitError::MigrationMissing { .. }
    ));
}

#[tokio::test]
async fn fifo_lease_order_independent_of_not_before_ms() {
    // Regression: enqueued_at used to be persisted from
    // not_before_ms, so the lease tie-breaker on
    // (next_run_at, enqueued_at) reflected scheduled time, not
    // insertion time. With the SQLite-clock fix, two jobs sharing a
    // next_run_at must lease in insertion order.
    let (store, _dir) = open_store();
    // Five back-to-back enqueues with the same next_run_at; rowid is
    // the deterministic tie-breaker, so the lease order must match the
    // insertion order regardless of clock resolution.
    let ids = ["j_a", "j_b", "j_c", "j_d", "j_e"];
    for id in &ids {
        store.enqueue(req(id, "kind.a")).await.expect("enqueue");
    }
    for expected in &ids {
        let leased = store
            .lease("w", 5_000, LEASE_MS)
            .await
            .unwrap()
            .expect("lease");
        assert_eq!(
            leased.job_id.as_str(),
            *expected,
            "FIFO order must hold across back-to-back enqueues"
        );
        store
            .complete(&leased.job_id, &leased.lease, 5_500)
            .await
            .expect("complete");
    }
}

#[tokio::test]
async fn lease_then_crash_before_heartbeat_does_not_consume_attempt() {
    // Regression: lease used to bump attempts immediately, so a
    // crash/timeout between lease and first heartbeat would burn retry
    // budget for work that never executed. The store now defers the
    // attempt bump to the first heartbeat / fail / complete; the
    // reaper requeues never-started leases without advancing
    // `attempts` (it does still bump `delivery_count` so the loop is
    // bounded — see pre_heartbeat_crash_loop_is_bounded).
    let (store, _dir) = open_store();
    let mut r = req("flaky", "kind.a");
    r.retry = RetryPolicy {
        max_attempts: 5,
        ..RetryPolicy::DEFAULT
    };
    store.enqueue(r).await.expect("enqueue");

    // Two consecutive infrastructure crashes — lease, expire, reap,
    // lease, expire, reap — must not exhaust the 5-attempt budget.
    let mut now = 0_i64;
    for _ in 0..2 {
        let _ = store.lease("w", now, 100).await.unwrap();
        // Reap well past lease deadline AND past whatever backoff the
        // policy schedules.
        now += 200_000;
        store.reap_expired(now).await.expect("reap");
    }

    // Job must still be eligible — no heartbeat ever happened, so no
    // attempt was consumed.
    let leased = store
        .lease("w", now + 100_000, LEASE_MS)
        .await
        .unwrap()
        .expect("still leasable after crashes");
    assert_eq!(leased.job_id.as_str(), "flaky");
    // Persisted attempts are still 0 because no heartbeat fired; the
    // caller-visible value is attempts+1 = 1.
    assert_eq!(leased.attempts, 1);

    // Now actually start (heartbeat) and fail with retry — that
    // consumes the first real attempt. Heartbeat must extend, never
    // shrink, the lease deadline, so we add to the token's existing
    // expires_at rather than to `now`.
    let after_lease = now + 100_001;
    store
        .heartbeat(
            &leased.job_id,
            &leased.lease,
            after_lease,
            leased.lease.expires_at_ms + 10_000,
        )
        .await
        .expect("heartbeat");
    store
        .fail(
            &leased.job_id,
            &leased.lease,
            FailDisposition::Retry,
            FailureClass::Transient,
            "real-fail",
            after_lease + 100,
        )
        .await
        .expect("retry-fail");

    let again = store
        .lease("w", after_lease + 1_000_000, LEASE_MS)
        .await
        .unwrap()
        .expect("re-leasable for second real try");
    assert_eq!(again.attempts, 2);
}

#[tokio::test]
async fn pre_heartbeat_crash_loop_is_bounded() {
    // Regression: never-started reaps used to requeue with
    // next_run_at=now and no delivery cap, so a worker that kept
    // dying before heartbeat would loop forever, blocking keyed
    // queues. delivery_count is now bumped on every lease and capped
    // at max_attempts * 5 (a poison guard separate from the public
    // execution-count contract); the row dead-letters once that
    // ceiling is hit. Pre-start crashes do NOT consume `attempts`.
    let (store, _dir) = open_store();
    let mut r = req("doomed-pre-start", "kind.a");
    // Zero backoff so each lease is immediately eligible — we want to
    // count deliveries, not exercise the backoff schedule.
    r.retry = RetryPolicy {
        max_attempts: 3,
        base_backoff_ms: 0,
        backoff_multiplier: 1,
        max_backoff_ms: 0,
    };
    store.enqueue(r).await.expect("enqueue");

    // Drive enough lease/reap cycles to exhaust the delivery cap
    // (max_attempts * 5 = 15; one extra to confirm rejection).
    let mut now = 0_i64;
    for _ in 0..16 {
        let _ = store.lease("w", now, 100).await.unwrap();
        now += 1_000_000; // skip past any backoff
        store.reap_expired(now).await.expect("reap");
    }

    // Row must now be terminal `failed`; no further leases possible.
    assert!(
        store
            .lease("w", now + 10_000_000, LEASE_MS)
            .await
            .unwrap()
            .is_none(),
        "delivery_count cap must dead-letter pre-heartbeat crash loops"
    );
}

#[tokio::test]
async fn started_orphan_reap_honors_backoff() {
    // Regression: the reaper used to put started-but-crashed rows
    // back to queued with next_run_at=now, bypassing the per-row
    // exponential backoff. Crash recovery should respect the policy.
    let (store, _dir) = open_store();
    let mut r = req("crashed-mid-run", "kind.a");
    r.retry = RetryPolicy {
        max_attempts: 5,
        base_backoff_ms: 5_000,
        backoff_multiplier: 2,
        max_backoff_ms: 60_000,
    };
    store.enqueue(r).await.expect("enqueue");

    let leased = store.lease("w", 0, 100).await.unwrap().unwrap();
    store
        .heartbeat(&leased.job_id, &leased.lease, 50, 100)
        .await
        .expect("heartbeat marks started");
    drop(leased); // crash

    // Reap at t=200; effective_attempts is now 1 (bumped on reap),
    // so backoff should be base_backoff_ms = 5000 — meaning
    // next_run_at = 200 + 5000 = 5200.
    let reclaimed = store.reap_expired(200).await.expect("reap");
    assert_eq!(reclaimed.len(), 1);

    // Before backoff elapsed, must NOT be eligible.
    assert!(
        store.lease("w", 4_000, LEASE_MS).await.unwrap().is_none(),
        "reaped started orphan must respect backoff"
    );
    // After backoff: leasable.
    let again = store
        .lease("w", 6_000, LEASE_MS)
        .await
        .unwrap()
        .expect("leasable after backoff");
    assert_eq!(again.job_id.as_str(), "crashed-mid-run");
    // Persisted attempts is now 1 (bumped on reap because lease_started=1);
    // caller-visible includes the in-flight try = 2.
    assert_eq!(again.attempts, 2);
}

#[tokio::test]
async fn schema_drift_rejected_at_construction() {
    // Regression: the constructor used to only check that
    // `workflow_jobs` existed. A partially-migrated DB (table present
    // but missing indexes/triggers) should fail fast rather than run
    // with weakened guarantees.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("partial.db");
    let setup = Connection::open(&db).expect("open");
    // Create only the table — none of the indexes or triggers from
    // migration 0020.
    setup
        .execute_batch(
            "CREATE TABLE workflow_jobs ( \
              job_id TEXT PRIMARY KEY, kind TEXT, payload BLOB, state TEXT, \
              attempts INTEGER, delivery_count INTEGER, max_attempts INTEGER, \
              base_backoff_ms INTEGER, backoff_multiplier INTEGER, max_backoff_ms INTEGER, \
              queue_key TEXT, dedupe_key TEXT, next_run_at INTEGER, \
              lease_owner TEXT, lease_nonce TEXT, lease_started INTEGER, \
              lease_expires_at INTEGER, last_error TEXT, \
              enqueued_at INTEGER, updated_at INTEGER \
            );",
        )
        .expect("partial schema");
    drop(setup);

    let probe = Connection::open(&db).expect("reopen");
    let Err(err) = SqliteJobStore::new(probe) else {
        panic!("must reject drifted schema");
    };
    assert!(matches!(
        err,
        SqliteJobStoreInitError::MigrationMissing { .. }
    ));
}

#[tokio::test]
async fn partial_migration_to_0020_only_rejected_at_construction() {
    // Regression (issue #92 round-5, finding 5.1): SqliteJobStore::new
    // probed for migration-0020 schema objects by name but did NOT
    // verify the columns added by migration 0062 (failure_class,
    // dead_letter_at_ms, completed_at_ms). A DB stuck at 0020 used to
    // pass `new()` then fail at runtime with `no such column:
    // failure_class` on the first `fail()` / `complete()` call —
    // *after* `enqueue` had already mutated the DB. The constructor
    // now probes both for the new indexes (existence-by-name) AND
    // for the new columns (PRAGMA table_info) so a partially-migrated
    // DB is rejected at construction.
    //
    // Apply ONLY migration 0020 inline (bypassing the canonical opener
    // which would apply every migration up through head). The
    // migration constant is re-exported off `cairn_store_sqlite`.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("only-0020.db");
    let setup = Connection::open(&db).expect("open");
    setup
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (\
                migration_id INTEGER NOT NULL PRIMARY KEY, \
                name TEXT NOT NULL, \
                sql_hash TEXT NOT NULL DEFAULT '', \
                applied_at INTEGER NOT NULL \
             );",
        )
        .expect("create schema_migrations");
    setup
        .execute_batch(cairn_store_sqlite::migrations::WORKFLOW_JOBS_MIGRATION_SQL)
        .expect("apply 0020");
    drop(setup);

    let probe = Connection::open(&db).expect("reopen");
    let Err(err) = SqliteJobStore::new(probe) else {
        panic!("must reject 0020-only DB");
    };
    // The schema-by-name probe walks `REQUIRED_SCHEMA` in order and
    // hits the 0062 dead-letter index first, so we surface as
    // MigrationMissing { kind="index", name="workflow_jobs_dead_letter_idx" }.
    // The intent of finding 5.1 is "reject 0020-only DB at construction" —
    // the exact discriminator (index vs column) is a probe-order detail;
    // assert the wider invariant (either MigrationMissing for a 0062
    // object or ColumnMissing for a 0062 column).
    match &err {
        SqliteJobStoreInitError::MigrationMissing { name, .. } => {
            assert!(
                name.contains("dead_letter") || name.contains("kind_completed"),
                "MigrationMissing must name a 0062-introduced object, got `{name}`",
            );
        }
        SqliteJobStoreInitError::ColumnMissing { name } => {
            assert!(
                matches!(
                    name,
                    &"failure_class" | &"dead_letter_at_ms" | &"completed_at_ms"
                ),
                "ColumnMissing must name a 0062 column, got `{name}`",
            );
        }
        other => panic!("expected MigrationMissing or ColumnMissing, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_0062_columns_alone_rejected_at_construction() {
    // Tighter regression for finding 5.1: confirm the *column probe*
    // independently catches a DB whose schema_objects (table + every
    // 0020 *and* 0062 index/trigger) pass the existence-by-name probe
    // but whose `workflow_jobs` table is missing the 0062 columns.
    // This isolates the column probe from the schema-by-name probe so
    // a future schema evolution that reshuffles probe order cannot
    // silently regress the column check.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("no-0062-columns.db");
    let conn = Connection::open(&db).expect("open");
    // Build a workflow_jobs table that omits the 0062 columns, plus
    // every required schema object by name (including the 0062 index
    // names — created against an unused expression so they pass the
    // existence-by-name probe). Triggers are stubs because the
    // column probe runs BEFORE the runtime-invariant probes; we only
    // need to get the early checks past their existence test.
    conn.execute_batch(
        "CREATE TABLE workflow_jobs ( \
            job_id TEXT PRIMARY KEY, kind TEXT, payload BLOB, state TEXT, \
            attempts INTEGER, delivery_count INTEGER, max_attempts INTEGER, \
            base_backoff_ms INTEGER, backoff_multiplier INTEGER, max_backoff_ms INTEGER, \
            queue_key TEXT, dedupe_key TEXT, next_run_at INTEGER, \
            lease_owner TEXT, lease_nonce TEXT, lease_started INTEGER, \
            lease_expires_at INTEGER, last_error TEXT, \
            enqueued_at INTEGER, updated_at INTEGER \
         ); \
         CREATE INDEX workflow_jobs_ready_idx ON workflow_jobs(next_run_at); \
         CREATE INDEX workflow_jobs_queued_queue_key_idx \
            ON workflow_jobs(queue_key, enqueued_at); \
         CREATE INDEX workflow_jobs_lease_expiry_idx ON workflow_jobs(lease_expires_at); \
         CREATE UNIQUE INDEX workflow_jobs_queue_key_leased_uniq \
            ON workflow_jobs(queue_key) WHERE queue_key IS NOT NULL; \
         CREATE UNIQUE INDEX workflow_jobs_dedupe_uniq \
            ON workflow_jobs(kind, dedupe_key) WHERE dedupe_key IS NOT NULL; \
         CREATE INDEX workflow_jobs_dead_letter_idx \
            ON workflow_jobs(next_run_at) WHERE next_run_at IS NOT NULL; \
         CREATE INDEX workflow_jobs_kind_completed_idx ON workflow_jobs(kind, next_run_at); \
         CREATE TRIGGER workflow_jobs_identity_immutable \
            BEFORE UPDATE ON workflow_jobs FOR EACH ROW WHEN 0 \
            BEGIN SELECT 1; END; \
         CREATE TRIGGER workflow_jobs_terminal_absorbing \
            BEFORE UPDATE OF state ON workflow_jobs FOR EACH ROW WHEN 0 \
            BEGIN SELECT 1; END; \
         CREATE TRIGGER workflow_jobs_state_transition \
            BEFORE UPDATE OF state ON workflow_jobs FOR EACH ROW WHEN 0 \
            BEGIN SELECT 1; END; \
         CREATE TRIGGER workflow_jobs_no_delete \
            BEFORE DELETE ON workflow_jobs FOR EACH ROW WHEN 0 \
            BEGIN SELECT 1; END; \
         CREATE TABLE schema_migrations ( \
            migration_id INTEGER PRIMARY KEY, name TEXT, sql_hash TEXT, applied_at INTEGER \
         ); \
         INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at) \
            VALUES (20, '0020_workflow_jobs', '', 0);",
    )
    .expect("setup schema without 0062 columns");
    drop(conn);

    let probe = Connection::open(&db).expect("reopen");
    let Err(err) = SqliteJobStore::new(probe) else {
        panic!("must reject DB missing 0062 columns");
    };
    match err {
        SqliteJobStoreInitError::ColumnMissing { name } => {
            assert!(
                matches!(
                    name,
                    "failure_class" | "dead_letter_at_ms" | "completed_at_ms"
                ),
                "ColumnMissing must name a 0062 column, got `{name}`",
            );
        }
        other => panic!("expected ColumnMissing, got {other:?}"),
    }
}

#[tokio::test]
async fn relaxed_check_rejected_at_construction() {
    // Regression: same-name DDL drift (CHECK relaxed in a hand-edited
    // table) used to start cleanly. The constructor now runs a
    // runtime-invariant probe — it tries to insert a row that the
    // queued-state CHECK must reject (state='queued' with a
    // lease_owner). If the probe succeeds, the CHECK has been
    // weakened and SqliteJobStore::new returns SchemaDrift.
    //
    // We simulate the relaxed-CHECK condition by recreating
    // `workflow_jobs` from scratch without the state-shape CHECK,
    // then re-create the indexes and triggers by name so the earlier
    // existence probe still passes.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("relaxed.db");
    let conn = Connection::open(&db).expect("open");
    conn.execute_batch(
        "CREATE TABLE workflow_jobs ( \
            job_id TEXT PRIMARY KEY, kind TEXT, payload BLOB, state TEXT, \
            attempts INTEGER, delivery_count INTEGER, max_attempts INTEGER, \
            base_backoff_ms INTEGER, backoff_multiplier INTEGER, max_backoff_ms INTEGER, \
            queue_key TEXT, dedupe_key TEXT, next_run_at INTEGER, \
            lease_owner TEXT, lease_nonce TEXT, lease_started INTEGER, \
            lease_expires_at INTEGER, last_error TEXT, \
            enqueued_at INTEGER, updated_at INTEGER, \
            failure_class TEXT, dead_letter_at_ms INTEGER, completed_at_ms INTEGER \
         ); \
         CREATE INDEX workflow_jobs_ready_idx ON workflow_jobs(next_run_at); \
         CREATE INDEX workflow_jobs_queued_queue_key_idx \
            ON workflow_jobs(queue_key, enqueued_at); \
         CREATE INDEX workflow_jobs_lease_expiry_idx ON workflow_jobs(lease_expires_at); \
         CREATE UNIQUE INDEX workflow_jobs_queue_key_leased_uniq \
            ON workflow_jobs(queue_key) WHERE queue_key IS NOT NULL; \
         CREATE UNIQUE INDEX workflow_jobs_dedupe_uniq \
            ON workflow_jobs(kind, dedupe_key) WHERE dedupe_key IS NOT NULL; \
         CREATE INDEX workflow_jobs_dead_letter_idx \
            ON workflow_jobs(dead_letter_at_ms) WHERE dead_letter_at_ms IS NOT NULL; \
         CREATE INDEX workflow_jobs_kind_completed_idx \
            ON workflow_jobs(kind, completed_at_ms); \
         CREATE TRIGGER workflow_jobs_identity_immutable \
            BEFORE UPDATE ON workflow_jobs FOR EACH ROW WHEN 0 \
            BEGIN SELECT 1; END; \
         CREATE TRIGGER workflow_jobs_terminal_absorbing \
            BEFORE UPDATE OF state ON workflow_jobs FOR EACH ROW WHEN 0 \
            BEGIN SELECT 1; END; \
         CREATE TRIGGER workflow_jobs_state_transition \
            BEFORE UPDATE OF state ON workflow_jobs FOR EACH ROW WHEN 0 \
            BEGIN SELECT 1; END; \
         CREATE TRIGGER workflow_jobs_no_delete \
            BEFORE DELETE ON workflow_jobs FOR EACH ROW WHEN 0 \
            BEGIN SELECT 1; END; \
         CREATE TABLE schema_migrations ( \
            migration_id INTEGER PRIMARY KEY, name TEXT, sql_hash TEXT, applied_at INTEGER \
         ); \
         INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at) \
            VALUES (20, '0020_workflow_jobs', '', 0);",
    )
    .expect("relaxed schema");
    drop(conn);

    let probe = Connection::open(&db).expect("reopen");
    let Err(err) = SqliteJobStore::new(probe) else {
        panic!("must reject relaxed schema");
    };
    assert!(
        matches!(err, SqliteJobStoreInitError::SchemaDrift { .. }),
        "expected SchemaDrift, got {err:?}"
    );
}

#[tokio::test]
async fn future_migration_extending_workflow_jobs_still_accepted() {
    // Regression: an earlier round of the constructor pinned an exact
    // DDL snapshot of migration 0020 and would have rejected any
    // future migration that legitimately altered the table or its
    // triggers. The current probe is invariant-based instead, so a
    // subsequent ALTER (modeled here as ADD COLUMN + DROP/RECREATE
    // triggers with stricter bodies) must still let
    // SqliteJobStore::new succeed as long as the runtime-invariant
    // contract still holds.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("evolved.db");
    {
        let _ = open_sync(&db).expect("migrate");
    }
    let conn = Connection::open(&db).expect("open");
    // Simulate a legitimate follow-up migration: add a non-null column
    // with a default. Triggers and CHECKs are untouched, so the
    // runtime invariants still hold. This is exactly the kind of
    // change a future schema evolution might make.
    conn.execute_batch("ALTER TABLE workflow_jobs ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;")
        .expect("future ALTER");
    drop(conn);

    let probe = Connection::open(&db).expect("reopen");
    SqliteJobStore::new(probe).expect("evolved schema must still construct");
}

#[tokio::test]
async fn construction_tolerates_caller_chosen_probe_id_lookalikes() {
    // Regression: the constructor used to insert probe rows with
    // hard-coded primary keys (e.g. `__cairn_probe_q__`). A caller
    // that legitimately picked the same job_id would brick startup
    // forever — every restart would crash on a SAVEPOINT-internal
    // UNIQUE conflict. Probe IDs are now generated per-call from a
    // ULID, so even a job_id matching the historical fixed strings
    // must not interfere.
    let (store, dir) = open_store();
    for id in [
        "__cairn_probe_q__",
        "__cairn_probe_qk1__",
        "__cairn_probe_qk2__",
        "__cairn_probe_dk1__",
        "__cairn_probe_dk2__",
        "__cairn_probe_st__",
    ] {
        store
            .enqueue(req(id, "user.kind"))
            .await
            .expect("caller-chosen lookalike id must enqueue");
    }
    // Reopen — startup must succeed despite these rows being on disk.
    let path = dir.path().join("cairn.db");
    drop(store);
    let conn = Connection::open(&path).expect("reopen");
    SqliteJobStore::new(conn).expect("startup must tolerate persisted lookalike IDs");
}

#[tokio::test]
async fn canonical_opener_stamps_hash_so_construction_succeeds() {
    // Regression: we now require schema_migrations.sql_hash for
    // migration_id=11 to be stamped (non-empty) before
    // SqliteJobStore::new will accept the connection. The canonical
    // cairn-store-sqlite::open_sync stamps it during
    // verify_migration_history, so the standard happy path must
    // still succeed end-to-end. (Tested transitively by every other
    // test that uses open_store(); this test pins the contract.)
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _ = open_sync(&db).expect("migrate via canonical opener");
    }
    let stamped: String = Connection::open(&db)
        .expect("reopen")
        .query_row(
            "SELECT sql_hash FROM schema_migrations WHERE migration_id = 20",
            [],
            |r| r.get(0),
        )
        .expect("query");
    assert!(
        !stamped.is_empty(),
        "canonical opener must stamp migration 0020 sql_hash"
    );
    let conn = Connection::open(&db).expect("reopen for store");
    SqliteJobStore::new(conn).expect("stamped hash must construct cleanly");
}

#[tokio::test]
async fn queue_key_fifo_holds_under_retry() {
    // Regression: lease only excluded queue_keys with a *currently
    // leased* row. After a keyed job retried with backoff, its
    // next_run_at jumped into the future and a younger sibling for
    // the same queue_key — with next_run_at = 0 — could be leased
    // first. That violates per-key FIFO exactly when ordering
    // matters most (partial failure / retry / crash recovery). The
    // lease query now also rejects rows that have an *older queued*
    // sibling for the same key.
    let (store, _dir) = open_store();
    let mut r1 = req("j1", "kind.a");
    r1.queue_key = Some("q".to_string());
    let mut r2 = req("j2", "kind.a");
    r2.queue_key = Some("q".to_string());
    store.enqueue(r1).await.expect("enqueue j1");
    store.enqueue(r2).await.expect("enqueue j2");

    // Lease j1 then fail-with-retry — j1 goes back to queued with
    // next_run_at = now + backoff, far in the future relative to
    // j2's next_run_at = 0.
    let leased = store.lease("w", 0, LEASE_MS).await.unwrap().unwrap();
    assert_eq!(leased.job_id.as_str(), "j1");
    store
        .fail(
            &leased.job_id,
            &leased.lease,
            FailDisposition::Retry,
            FailureClass::Transient,
            "transient",
            0,
        )
        .await
        .expect("retry-fail j1");

    // Even though j2 is technically eligible by next_run_at, it must
    // wait for j1 (the older queued sibling for queue_key=q).
    assert!(
        store.lease("w2", 0, LEASE_MS).await.unwrap().is_none(),
        "j2 must NOT overtake j1 — head-of-line FIFO per queue_key"
    );

    // After the backoff, j1 is the one leased.
    let next = store
        .lease("w", 60_000, LEASE_MS)
        .await
        .unwrap()
        .expect("j1 retried");
    assert_eq!(next.job_id.as_str(), "j1");
}

#[tokio::test]
async fn dedupe_key_blocks_replay_after_done() {
    // Regression: a completed (done) row must still occupy the
    // dedupe slot — otherwise a timed-out caller that retries the
    // enqueue after the first worker succeeded would run the same
    // externally-visible side effect twice. Only `failed` rows
    // release the slot.
    let (store, _dir) = open_store();
    let mut r = req("op-first", "kind.a");
    r.dedupe_key = Some("op-1".to_string());
    store.enqueue(r).await.expect("enqueue first attempt");
    let leased = store.lease("w", 0, LEASE_MS).await.unwrap().unwrap();
    store
        .complete(&leased.job_id, &leased.lease, 1)
        .await
        .expect("complete");
    // Second enqueue with the same operation_id must short-circuit.
    let mut dup = req("op-replay-after-done", "kind.a");
    dup.dedupe_key = Some("op-1".to_string());
    let err = store.enqueue(dup).await.unwrap_err();
    assert!(
        matches!(err, JobStoreError::DuplicateDedupeKey { .. }),
        "completed dedupe slot must still block replay, got {err:?}"
    );
}

#[tokio::test]
async fn dedupe_key_replayable_after_terminal_failure() {
    // Regression: dedupe_key uniqueness used to span all states, so
    // a stable workflow operation_id was permanently burnt by its
    // first failed delivery. The contract promises step-level
    // idempotency via operation_id; that requires the slot to free
    // up once the row reaches a terminal state.
    let (store, _dir) = open_store();
    let mut r = req("op-first", "kind.a");
    r.dedupe_key = Some("op-1".to_string());
    store.enqueue(r).await.expect("enqueue first attempt");
    let leased = store.lease("w", 0, LEASE_MS).await.unwrap().unwrap();
    store
        .fail(
            &leased.job_id,
            &leased.lease,
            FailDisposition::Permanent,
            FailureClass::Validation,
            "fatal",
            0,
        )
        .await
        .expect("permanent fail");

    // After terminal failure, the same operation_id must be
    // re-enqueueable so an operator can replay it safely.
    let mut replay = req("op-replay", "kind.a");
    replay.dedupe_key = Some("op-1".to_string());
    store
        .enqueue(replay)
        .await
        .expect("dedupe slot must free up after terminal failure");
}

#[tokio::test]
async fn construction_self_stamps_unstamped_migration_hash() {
    // Regression: migration 0020 inserts schema_migrations.sql_hash =
    // ''. Prior code rejected '' as SchemaDrift, bricking any caller
    // that opens a freshly-migrated DB without going through the
    // canonical opener's verify_migration_history. The constructor
    // now self-stamps in place (the schema_migrations trigger
    // explicitly permits a one-shot '' -> hash transition).
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("freshly-migrated.db");
    {
        // Apply migrations *without* the canonical opener's verify
        // pass — emulate "migrations ran but nothing else has touched
        // the DB yet". `open_in_memory_sync` would lose the file; we
        // instead use the canonical opener and then nuke the stamped
        // hash to simulate the unstamped state.
        let _ = open_sync(&db).expect("migrate");
    }
    let setup = Connection::open(&db).expect("reopen");
    setup
        .execute_batch(
            "DROP TRIGGER schema_migrations_immutable; \
             UPDATE schema_migrations SET sql_hash = '' WHERE migration_id = 20; \
             CREATE TRIGGER schema_migrations_immutable \
                BEFORE UPDATE ON schema_migrations \
                FOR EACH ROW \
                WHEN OLD.migration_id IS NOT NEW.migration_id \
                  OR OLD.name IS NOT NEW.name \
                  OR OLD.applied_at IS NOT NEW.applied_at \
                  OR NOT (OLD.sql_hash = '' AND length(NEW.sql_hash) > 0) \
                BEGIN \
                  SELECT RAISE(ABORT, 'schema_migrations rows are immutable (only `` -> hash on sql_hash allowed)'); \
                END;",
        )
        .expect("reset hash to ''");
    drop(setup);

    let conn = Connection::open(&db).expect("reopen for store");
    SqliteJobStore::new(conn).expect("must self-stamp '' hash and construct");

    // Verify the row is now stamped and a second construction works.
    let stamped: String = Connection::open(&db)
        .expect("reopen")
        .query_row(
            "SELECT sql_hash FROM schema_migrations WHERE migration_id = 20",
            [],
            |r| r.get(0),
        )
        .expect("query");
    assert!(!stamped.is_empty(), "construction must stamp the hash");
    SqliteJobStore::new(Connection::open(&db).unwrap())
        .expect("second construction must succeed against stamped hash");
}

#[tokio::test]
async fn heartbeat_updated_at_is_now_not_future_deadline() {
    // Regression: heartbeat used to write updated_at = new_expires_at_ms,
    // which was a future timestamp. That hid stale-worker activity in
    // telemetry and made any future "rows last touched before T" query
    // wrong. updated_at must be the actual mutation time (now_ms).
    let (store, dir) = open_store();
    store.enqueue(req("j", "kind.a")).await.expect("enqueue");
    let leased = store.lease("w", 1_000, 5_000).await.unwrap().unwrap();
    let now = 1_500_i64;
    let new_deadline = 30_000_i64;
    store
        .heartbeat(&leased.job_id, &leased.lease, now, new_deadline)
        .await
        .expect("heartbeat");

    let conn = Connection::open(dir.path().join("cairn.db")).expect("reopen");
    let (updated_at, expires_at): (i64, i64) = conn
        .query_row(
            "SELECT updated_at, lease_expires_at FROM workflow_jobs WHERE job_id = ?",
            ["j"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("query");
    assert_eq!(updated_at, now, "updated_at must be the heartbeat's now_ms");
    assert_eq!(
        expires_at, new_deadline,
        "lease_expires_at gets the deadline"
    );
    assert_ne!(
        updated_at, expires_at,
        "telemetry must not collapse updated_at into the future deadline"
    );
}

#[tokio::test]
async fn lease_rejects_non_positive_duration() {
    // Regression: lease used to persist `lease_expires_at = now +
    // duration` for any duration value, including 0 or negative —
    // producing rows that were already expired the moment they were
    // leased, which made the next heartbeat/complete fail with
    // LeaseLost and could dead-letter healthy work via the poison cap.
    let (store, _dir) = open_store();
    store.enqueue(req("j", "kind.a")).await.expect("enqueue");
    let err = store.lease("w", 1_000, 0).await.unwrap_err();
    assert!(
        matches!(err, JobStoreError::InvalidLeaseDeadline { .. }),
        "expected InvalidLeaseDeadline, got {err:?}"
    );
    let err2 = store.lease("w", 1_000, -100).await.unwrap_err();
    assert!(matches!(err2, JobStoreError::InvalidLeaseDeadline { .. }));
}

#[tokio::test]
async fn heartbeat_rejects_non_future_or_shrinking_deadline() {
    // Regression: heartbeat used to accept any new_expires_at_ms as
    // long as the *current* lease was still live, so a buggy caller
    // could move a live deadline into the past and the reaper would
    // requeue the job out from under the still-running worker.
    let (store, _dir) = open_store();
    store.enqueue(req("j", "kind.a")).await.expect("enqueue");
    let leased = store.lease("w", 1_000, 5_000).await.unwrap().unwrap();

    // Past deadline.
    let err = store
        .heartbeat(&leased.job_id, &leased.lease, 1_000, 500)
        .await
        .unwrap_err();
    assert!(matches!(err, JobStoreError::InvalidLeaseDeadline { .. }));

    // Equal-to-now deadline.
    let err2 = store
        .heartbeat(&leased.job_id, &leased.lease, 1_000, 1_000)
        .await
        .unwrap_err();
    assert!(matches!(err2, JobStoreError::InvalidLeaseDeadline { .. }));

    // Shrinking — strictly less than the existing lease deadline.
    // Even though new (3_000) is in the future relative to now
    // (1_000), it would move the lease closer in than its current
    // 6_000 expiry and let the reaper preempt the live worker.
    let err3 = store
        .heartbeat(
            &leased.job_id,
            &leased.lease,
            1_000,
            leased.lease.expires_at_ms - 1,
        )
        .await
        .unwrap_err();
    assert!(matches!(err3, JobStoreError::InvalidLeaseDeadline { .. }));
}

#[tokio::test]
async fn second_heartbeat_cannot_shrink_already_extended_deadline() {
    // Regression: the Rust-side shrink check compares against the
    // caller's LeaseToken, which is stale across heartbeats (the
    // contract reuses the same token). After heartbeat #1 extends
    // the persisted deadline well into the future, a buggy
    // heartbeat #2 with a smaller new deadline would slip through
    // the wrapper check (still >= token's stale expiry) but must
    // still be rejected by the SQL layer's compare-to-persisted
    // predicate. Otherwise the reaper could steal a still-running
    // worker.
    let (store, _dir) = open_store();
    store.enqueue(req("j", "kind.a")).await.expect("enqueue");
    let leased = store.lease("w", 0, 1_000).await.unwrap().unwrap();
    // First heartbeat: extend from 1_000 to 100_000.
    store
        .heartbeat(&leased.job_id, &leased.lease, 500, 100_000)
        .await
        .expect("heartbeat #1 extends to 100_000");
    // Second heartbeat using the SAME (stale) token, requesting a
    // deadline that is still > now and > the token's expires_at
    // (1_000) but < the persisted deadline (100_000). Wrapper
    // accepts; SQL must reject as InvalidLeaseDeadline.
    let err = store
        .heartbeat(&leased.job_id, &leased.lease, 1_500, 50_000)
        .await
        .unwrap_err();
    assert!(
        matches!(err, JobStoreError::InvalidLeaseDeadline { .. }),
        "expected InvalidLeaseDeadline from SQL-layer shrink check, got {err:?}"
    );
    // Original token must still be valid for complete (not stolen).
    store
        .complete(&leased.job_id, &leased.lease, 2_000)
        .await
        .expect("token still valid after rejected shrink heartbeat");
}

#[tokio::test]
async fn lease_is_atomic_across_independent_connections() {
    // Regression: lease/reap used deferred transactions, which could
    // raise stale-snapshot errors under multi-connection contention.
    // BEGIN IMMEDIATE plus busy_timeout makes contention resolve as a
    // clean lost race instead of a backend error. This test runs many
    // racers across separately-opened SqliteJobStore instances on the
    // same DB file.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("multi.db");
    {
        let _ = open_sync(&db).expect("migrate");
    }

    // Seed 8 jobs.
    {
        let store = SqliteJobStore::new(Connection::open(&db).unwrap()).expect("init seed");
        for i in 0..8 {
            store
                .enqueue(req(&format!("j{i}"), "kind.a"))
                .await
                .expect("seed");
        }
    }

    // 16 concurrent workers, each with its own Connection (and thus
    // its own snapshot view).
    let mut handles = Vec::new();
    for id in 0..16 {
        let path = db.clone();
        handles.push(tokio::spawn(async move {
            let store = SqliteJobStore::new(Connection::open(&path).unwrap()).expect("init worker");
            let mut leased = Vec::new();
            while let Ok(Some(job)) = store.lease(&format!("w{id}"), 0, LEASE_MS).await {
                store
                    .complete(&job.job_id, &job.lease, 1)
                    .await
                    .expect("complete from independent conn");
                leased.push(job.job_id.0);
            }
            leased
        }));
    }

    let mut all = Vec::new();
    for h in handles {
        all.extend(h.await.expect("join"));
    }
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 8, "every job leased exactly once across racers");
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
        store
            .complete(&leased.job_id, &leased.lease, 1_500)
            .await
            .unwrap();
        drained += 1;
    }
    assert_eq!(drained, n);
}

/// Round-4 finding regression: `lease_specific` must atomically claim
/// ONLY the row matching `(job_id, expected_kind)`. Any other queued
/// row — in particular a production-kind row enqueued concurrently —
/// must remain `state = 'queued'` with `delivery_count = 0` so the
/// rightful worker still picks it up untouched. The bug we're proving
/// fixed: the original `drive_synthetic_job` used the generic `lease()`,
/// which would happily lease a production row in the precheck→lease
/// race window, bump its `delivery_count`, and park it in `'leased'`
/// until expiry.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "linear regression test: enqueue prod + synth, lease synth, \
              assert prod untouched, then re-probe with wrong kind and \
              missing id. Splitting hides the on-disk invariant chain."
)]
async fn lease_specific_only_touches_matching_row() {
    let (store, dir) = open_store();
    let db_path = dir.path().join("cairn.db");

    // Production row first — a real workflow kind. `not_before_ms = 0`
    // makes it immediately lease-eligible, which is the worst case for
    // the diagnostic to misroute.
    store
        .enqueue(req("prod-row", "dream.light"))
        .await
        .expect("enqueue prod");
    // Synthetic row second.
    store
        .enqueue(req("synth-row", "test.e2e.always_done"))
        .await
        .expect("enqueue synthetic");

    let synth_id = JobId::new("synth-row");
    let synth_kind = JobKind::new("test.e2e.always_done");

    // 1. Successful lease of the synthetic row only.
    let leased = store
        .lease_specific(&synth_id, &synth_kind, "diagnostic-owner", 1_000, 30_000)
        .await
        .expect("lease_specific ok")
        .expect("synthetic row should be leasable");
    assert_eq!(leased.job_id.as_str(), "synth-row");
    assert_eq!(leased.kind.as_str(), "test.e2e.always_done");
    assert_eq!(leased.lease.owner, "diagnostic-owner");

    // 2. Inspect on-disk state directly. The production row must be
    // BIT-IDENTICAL to its post-enqueue state — same state, same
    // delivery_count, same lease columns (NULL).
    let prod = Connection::open(&db_path).expect("reopen for assertion");
    let (prod_state, prod_dc, prod_owner, prod_nonce, prod_expires): (
        String,
        i64,
        Option<String>,
        Option<String>,
        Option<i64>,
    ) = prod
        .query_row(
            "SELECT state, delivery_count, lease_owner, lease_nonce, lease_expires_at \
               FROM workflow_jobs WHERE job_id = ?1",
            rusqlite::params!["prod-row"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("prod row exists");
    assert_eq!(prod_state, "queued", "production row must remain queued");
    assert_eq!(
        prod_dc, 0,
        "production row delivery_count must be untouched (was 0 after enqueue)"
    );
    assert!(
        prod_owner.is_none() && prod_nonce.is_none() && prod_expires.is_none(),
        "production row lease columns must remain NULL"
    );

    // Synthetic row should be in 'leased' state with delivery_count=1.
    let (synth_state, synth_dc): (String, i64) = prod
        .query_row(
            "SELECT state, delivery_count \
               FROM workflow_jobs WHERE job_id = ?1",
            rusqlite::params!["synth-row"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("synth row exists");
    assert_eq!(synth_state, "leased", "synthetic row should be leased");
    assert_eq!(
        synth_dc, 1,
        "synthetic row delivery_count should have advanced exactly once"
    );

    // 3. Kind mismatch refuses with Ok(None). Re-enqueue a fresh
    // synthetic so we have a queued row to (not) match against — the
    // first one is now leased and ineligible.
    store
        .enqueue(req("synth-row-2", "test.e2e.always_done"))
        .await
        .expect("enqueue synthetic-2");
    let none = store
        .lease_specific(
            &JobId::new("synth-row-2"),
            &JobKind::new("wrong_kind"),
            "diagnostic-owner",
            1_500,
            30_000,
        )
        .await
        .expect("kind-mismatch returns Ok(None), not Err");
    assert!(
        none.is_none(),
        "kind mismatch must refuse to lease (got {none:?})"
    );

    // After the kind-mismatch attempt, synth-row-2 must still be queued
    // with delivery_count=0 — proves the UPDATE matched nothing.
    let (sr2_state, sr2_dc): (String, i64) = prod
        .query_row(
            "SELECT state, delivery_count \
               FROM workflow_jobs WHERE job_id = ?1",
            rusqlite::params!["synth-row-2"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("synth-row-2 exists");
    assert_eq!(
        sr2_state, "queued",
        "kind-mismatched row must remain queued"
    );
    assert_eq!(
        sr2_dc, 0,
        "kind-mismatched row delivery_count must be untouched"
    );

    // 4. Nonexistent job id refuses with Ok(None).
    let none2 = store
        .lease_specific(
            &JobId::new("nonexistent"),
            &synth_kind,
            "diagnostic-owner",
            2_000,
            30_000,
        )
        .await
        .expect("nonexistent returns Ok(None), not Err");
    assert!(
        none2.is_none(),
        "nonexistent id must refuse to lease (got {none2:?})"
    );

    // Production row, third and final check after every other call —
    // still queued, still untouched.
    let (prod_state_final, prod_dc_final): (String, i64) = prod
        .query_row(
            "SELECT state, delivery_count \
               FROM workflow_jobs WHERE job_id = ?1",
            rusqlite::params!["prod-row"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("prod row exists");
    assert_eq!(prod_state_final, "queued");
    assert_eq!(
        prod_dc_final, 0,
        "production row delivery_count remains 0 across the entire test"
    );
}
