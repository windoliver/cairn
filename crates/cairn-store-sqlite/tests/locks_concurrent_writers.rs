//! Concurrent-writers test for §5.6: N writers serializing without deadlock.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use cairn_store_sqlite::locks::{LockError, LockMode, ResourceKey, acquire};
use cairn_store_sqlite::open_in_memory;
use tokio::task::JoinSet;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_exclusive_writers_serialize_without_deadlock() {
    const N_WRITERS: usize = 8;
    const ITERATIONS: usize = 10;

    let store = open_in_memory().await.unwrap();
    let conn = Arc::clone(store.raw_conn_for_admin().unwrap());
    let inc = store.incarnation().cloned().unwrap();

    let resource = ResourceKey::entity("t1", "default", "shared_record");
    let inflight = Arc::new(AtomicI32::new(0));
    let max_inflight = Arc::new(AtomicI32::new(0));

    let started = Instant::now();
    let mut set = JoinSet::new();

    for w in 0..N_WRITERS {
        let conn = Arc::clone(&conn);
        let inc = Arc::clone(&inc);
        let resource = resource.clone();
        let inflight = Arc::clone(&inflight);
        let max_inflight = Arc::clone(&max_inflight);
        set.spawn(async move {
            for i in 0..ITERATIONS {
                let holder_id = format!("w{w}_i{i}");
                loop {
                    match acquire(
                        &conn,
                        &resource,
                        LockMode::Exclusive,
                        &holder_id,
                        Duration::from_secs(5),
                        &inc,
                        "concurrent_test",
                    )
                    .await
                    {
                        Ok(handle) => {
                            let n = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                            max_inflight.fetch_max(n, Ordering::SeqCst);
                            // Simulate brief work.
                            tokio::time::sleep(Duration::from_millis(5)).await;
                            inflight.fetch_sub(1, Ordering::SeqCst);
                            handle.release().await.unwrap();
                            break;
                        }
                        Err(LockError::Held { .. }) => {
                            tokio::time::sleep(Duration::from_millis(15)).await;
                        }
                        Err(e) => panic!("unexpected acquire error: {e:?}"),
                    }
                }
            }
        });
    }

    while let Some(r) = set.join_next().await {
        r.unwrap();
    }

    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_mins(1),
        "8 writers × 10 iterations should finish in < 60s; took {elapsed:?}"
    );
    assert_eq!(
        max_inflight.load(Ordering::SeqCst),
        1,
        "exclusive lock must allow only one writer in critical section at a time"
    );
}
