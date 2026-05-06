//! Lock-table acquire/release for §5.6 (issue #254 + #56).

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use cairn_store_sqlite::locks::{LockError, acquire_exclusive};
use cairn_store_sqlite::open_in_memory;

async fn fresh_conn() -> Arc<tokio_rusqlite::Connection> {
    let store = open_in_memory().await.expect("open in-memory store");
    Arc::clone(
        store
            .raw_conn_for_admin()
            .expect("store has a live connection"),
    )
}

#[tokio::test]
async fn acquire_exclusive_succeeds_on_unheld_resource() {
    let conn = fresh_conn().await;
    let h = acquire_exclusive(&conn, "lint_repair", "v1", "A", Duration::from_mins(1))
        .await
        .expect("acquire");
    drop(h);
}

#[tokio::test]
async fn second_exclusive_returns_held() {
    let conn = fresh_conn().await;
    let _h1 = acquire_exclusive(&conn, "lint_repair", "v1", "A", Duration::from_mins(1))
        .await
        .expect("first");
    let r = acquire_exclusive(&conn, "lint_repair", "v1", "B", Duration::from_mins(1)).await;
    assert!(matches!(r, Err(LockError::Held { .. })), "got {r:?}");
}

#[tokio::test]
async fn release_via_handle_allows_reacquire() {
    let conn = fresh_conn().await;
    let h = acquire_exclusive(&conn, "lint_repair", "v", "A", Duration::from_mins(1))
        .await
        .expect("first");
    h.release().await.expect("release");
    let _h2 = acquire_exclusive(&conn, "lint_repair", "v", "B", Duration::from_mins(1))
        .await
        .expect("reacquire");
}

#[tokio::test]
async fn expired_lock_can_be_reclaimed() {
    let conn = fresh_conn().await;
    let h1 = acquire_exclusive(&conn, "lint_repair", "v", "A", Duration::from_millis(0))
        .await
        .expect("first");
    std::mem::forget(h1); // simulate crash — holder never released.
    tokio::time::sleep(Duration::from_millis(10)).await;
    let _h2 = acquire_exclusive(&conn, "lint_repair", "v", "B", Duration::from_mins(1))
        .await
        .expect("reclaim expired");
}

#[tokio::test]
async fn distinct_scope_keys_are_independent() {
    let conn = fresh_conn().await;
    let _h1 = acquire_exclusive(&conn, "lint_repair", "vault-1", "A", Duration::from_mins(1))
        .await
        .expect("v1");
    let _h2 = acquire_exclusive(&conn, "lint_repair", "vault-2", "B", Duration::from_mins(1))
        .await
        .expect("v2");
}

#[tokio::test]
async fn concurrent_acquires_serialize_one_winner() {
    let conn = Arc::new(fresh_conn().await);
    let c1 = Arc::clone(&conn);
    let c2 = Arc::clone(&conn);
    let (r1, r2) = tokio::join!(
        async move { acquire_exclusive(&c1, "lint_repair", "v", "A", Duration::from_mins(1)).await },
        async move { acquire_exclusive(&c2, "lint_repair", "v", "B", Duration::from_mins(1)).await },
    );
    let oks = [r1.is_ok(), r2.is_ok()].iter().filter(|b| **b).count();
    assert_eq!(oks, 1, "exactly one acquire must win");
}
