//! Lock-table acquire/release for §5.6 (issue #254 + #56).

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use cairn_store_sqlite::locks::{LockError, LockMode, ResourceKey, acquire, init_incarnation};
use cairn_store_sqlite::open_in_memory;

async fn fresh_env() -> (Arc<tokio_rusqlite::Connection>, Arc<str>) {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = Arc::clone(
        store
            .raw_conn_for_admin()
            .expect("store has a live connection"),
    );
    // `open_in_memory` already mints an incarnation via `Store::open`, but
    // these tests use the raw conn directly without going through the
    // store's lock context. Re-init returns the same Arc for the duration
    // of this connection.
    let inc = init_incarnation(&conn)
        .await
        .expect("init_incarnation on in-memory store");
    (conn, inc)
}

#[tokio::test]
async fn acquire_exclusive_succeeds_on_unheld_resource() {
    let (conn, inc) = fresh_env().await;
    let r = ResourceKey::vault("v1");
    let h = acquire(
        &conn,
        &r,
        LockMode::Exclusive,
        "A",
        Duration::from_mins(1),
        &inc,
        "test",
    )
    .await
    .expect("acquire");
    drop(h);
}

#[tokio::test]
async fn second_exclusive_returns_held() {
    let (conn, inc) = fresh_env().await;
    let r = ResourceKey::vault("v1");
    let _h1 = acquire(
        &conn,
        &r,
        LockMode::Exclusive,
        "A",
        Duration::from_mins(1),
        &inc,
        "first",
    )
    .await
    .expect("first");
    let res = acquire(
        &conn,
        &r,
        LockMode::Exclusive,
        "B",
        Duration::from_mins(1),
        &inc,
        "second",
    )
    .await;
    assert!(matches!(res, Err(LockError::Held { .. })), "got {res:?}");
}

#[tokio::test]
async fn release_via_handle_allows_reacquire() {
    let (conn, inc) = fresh_env().await;
    let r = ResourceKey::vault("v");
    let h = acquire(
        &conn,
        &r,
        LockMode::Exclusive,
        "A",
        Duration::from_mins(1),
        &inc,
        "first",
    )
    .await
    .expect("first");
    h.release().await.expect("release");
    let _h2 = acquire(
        &conn,
        &r,
        LockMode::Exclusive,
        "B",
        Duration::from_mins(1),
        &inc,
        "reacquire",
    )
    .await
    .expect("reacquire");
}

#[tokio::test]
async fn expired_lock_can_be_reclaimed() {
    let (conn, inc) = fresh_env().await;
    let r = ResourceKey::vault("v");
    let h1 = acquire(
        &conn,
        &r,
        LockMode::Exclusive,
        "A",
        Duration::from_millis(0),
        &inc,
        "first",
    )
    .await
    .expect("first");
    std::mem::forget(h1); // simulate crash — holder never released.
    tokio::time::sleep(Duration::from_millis(10)).await;
    let _h2 = acquire(
        &conn,
        &r,
        LockMode::Exclusive,
        "B",
        Duration::from_mins(1),
        &inc,
        "reclaim",
    )
    .await
    .expect("reclaim expired");
}

#[tokio::test]
async fn distinct_scope_keys_are_independent() {
    let (conn, inc) = fresh_env().await;
    let r1 = ResourceKey::vault("vault-1");
    let r2 = ResourceKey::vault("vault-2");
    let _h1 = acquire(
        &conn,
        &r1,
        LockMode::Exclusive,
        "A",
        Duration::from_mins(1),
        &inc,
        "v1",
    )
    .await
    .expect("v1");
    let _h2 = acquire(
        &conn,
        &r2,
        LockMode::Exclusive,
        "B",
        Duration::from_mins(1),
        &inc,
        "v2",
    )
    .await
    .expect("v2");
}

#[tokio::test]
async fn concurrent_acquires_serialize_one_winner() {
    let (conn, inc) = fresh_env().await;
    let r = ResourceKey::vault("v");
    let c1 = Arc::clone(&conn);
    let c2 = Arc::clone(&conn);
    let r1 = r.clone();
    let r2 = r.clone();
    let inc1 = Arc::clone(&inc);
    let inc2 = Arc::clone(&inc);
    let (a, b) = tokio::join!(
        async move {
            acquire(
                &c1,
                &r1,
                LockMode::Exclusive,
                "A",
                Duration::from_mins(1),
                &inc1,
                "race-a",
            )
            .await
        },
        async move {
            acquire(
                &c2,
                &r2,
                LockMode::Exclusive,
                "B",
                Duration::from_mins(1),
                &inc2,
                "race-b",
            )
            .await
        },
    );
    let oks = [a.is_ok(), b.is_ok()].iter().filter(|b| **b).count();
    assert_eq!(oks, 1, "exactly one acquire must win");
}
