//! Stale-fence test for §5.6: a holder whose lease expired and was reclaimed
//! cannot commit through `with_fencing`.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use cairn_store_sqlite::locks::{LockError, LockMode, ResourceKey, acquire};
use cairn_store_sqlite::open_in_memory;

#[tokio::test]
async fn stale_writer_blocked_by_fencing_cas() {
    let store = open_in_memory().await.unwrap();
    let conn = Arc::clone(store.raw_conn_for_admin().unwrap());
    let inc = store.incarnation().cloned().unwrap();
    let resource = ResourceKey::entity("t1", "default", "rec1");

    // Holder A acquires with short TTL; do NOT release.
    let h_a = acquire(
        &conn,
        &resource,
        LockMode::Exclusive,
        "holder_a",
        Duration::from_millis(80),
        &inc,
        "writer_a",
    )
    .await
    .unwrap();
    assert_eq!(h_a.acquired_epoch(), 1);

    // Sleep past TTL.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Holder B acquires same resource — triggers GC + epoch bump.
    let h_b = acquire(
        &conn,
        &resource,
        LockMode::Exclusive,
        "holder_b",
        Duration::from_secs(5),
        &inc,
        "writer_b",
    )
    .await
    .unwrap();
    assert_eq!(h_b.acquired_epoch(), 2);

    // A's fencing CAS must fail closed.
    let err = h_a
        .with_fencing(|tx| tx.execute("CREATE TEMP TABLE forbidden (x INTEGER)", []))
        .await
        .unwrap_err();
    match err {
        LockError::Fenced {
            expected_epoch,
            observed_epoch,
            ..
        } => {
            assert_eq!(expected_epoch, 1);
            assert_eq!(observed_epoch, 2);
        }
        other => panic!("expected Fenced, got {other:?}"),
    }

    // B's fencing CAS succeeds.
    h_b.with_fencing(|tx| tx.execute("CREATE TEMP TABLE permitted (x INTEGER)", []))
        .await
        .unwrap();
}

#[tokio::test]
async fn incarnation_change_invalidates_prior_holders() {
    use cairn_store_sqlite::locks::init_incarnation;

    let store = open_in_memory().await.unwrap();
    let conn = Arc::clone(store.raw_conn_for_admin().unwrap());
    let inc1 = store.incarnation().cloned().unwrap();
    let resource = ResourceKey::vault("v_inc");

    let h_a = acquire(
        &conn,
        &resource,
        LockMode::Exclusive,
        "h_a",
        Duration::from_mins(1),
        &inc1,
        "writer_a",
    )
    .await
    .unwrap();
    assert_eq!(h_a.acquired_epoch(), 1);

    // Simulate daemon restart: mint a fresh incarnation directly.
    let _inc2 = init_incarnation(&conn).await.unwrap();

    // h_a's CAS now fails — its row was GC'd by init_incarnation, epoch bumped.
    let err = h_a
        .with_fencing(|tx| tx.execute("SELECT 1", []))
        .await
        .unwrap_err();
    assert!(matches!(err, LockError::Fenced { .. }));
}

#[tokio::test]
async fn assert_live_in_tx_blocks_stale_holder_inside_existing_transaction() {
    let store = open_in_memory().await.unwrap();
    let conn = Arc::clone(store.raw_conn_for_admin().unwrap());
    let inc = store.incarnation().cloned().unwrap();
    let resource = ResourceKey::entity("t1", "default", "rec-tx");

    let h_a = acquire(
        &conn,
        &resource,
        LockMode::Exclusive,
        "holder_a_tx",
        Duration::from_millis(80),
        &inc,
        "writer_a_tx",
    )
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;

    let _h_b = acquire(
        &conn,
        &resource,
        LockMode::Exclusive,
        "holder_b_tx",
        Duration::from_secs(5),
        &inc,
        "writer_b_tx",
    )
    .await
    .unwrap();

    let err = conn
        .call(move |c| {
            let tx = c.transaction()?;
            let result = h_a.assert_live_in_tx(&tx);
            drop(tx);
            Ok::<_, tokio_rusqlite::Error>(result)
        })
        .await
        .unwrap()
        .unwrap_err();

    assert!(matches!(err, LockError::Fenced { .. }));
}
