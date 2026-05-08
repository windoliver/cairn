//! Compatibility-matrix integration test for §5.6 lock acquisition.
//!
//! Verifies the (`incumbent_mode` × `requested_mode`) outcomes plus the
//! brief's "write with session" pattern: writer takes (Entity, Exclusive) AND
//! (Session, Shared) simultaneously; another writer holding (Session, Shared)
//! does not block; a concurrent `forget_session` attempt for that session is
//! Held.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use cairn_store_sqlite::locks::{LockError, LockMode, ResourceKey, acquire};
use cairn_store_sqlite::open_in_memory;
use rstest::rstest;

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Ok,
    Held,
}

async fn setup() -> (Arc<tokio_rusqlite::Connection>, Arc<str>) {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let inc = store.incarnation().cloned().expect("incarnation set");
    drop(store);
    (conn, inc)
}

#[rstest]
#[case(None, LockMode::Shared, Outcome::Ok)]
#[case(None, LockMode::Exclusive, Outcome::Ok)]
#[case(Some(LockMode::Shared), LockMode::Shared, Outcome::Ok)]
#[case(Some(LockMode::Shared), LockMode::Exclusive, Outcome::Held)]
#[case(Some(LockMode::Exclusive), LockMode::Shared, Outcome::Held)]
#[case(Some(LockMode::Exclusive), LockMode::Exclusive, Outcome::Held)]
#[tokio::test]
async fn compatibility_matrix(
    #[case] incumbent: Option<LockMode>,
    #[case] requested: LockMode,
    #[case] expected: Outcome,
) {
    let (conn, inc) = setup().await;
    let r = ResourceKey::vault("matrix");

    let _incumbent_handle = if let Some(mode) = incumbent {
        Some(
            acquire(
                &conn,
                &r,
                mode,
                "incumbent",
                Duration::from_secs(5),
                &inc,
                "incumbent_op",
            )
            .await
            .expect("incumbent acquire"),
        )
    } else {
        None
    };

    let result = acquire(
        &conn,
        &r,
        requested,
        "challenger",
        Duration::from_secs(5),
        &inc,
        "challenger_op",
    )
    .await;

    // Two arms with empty bodies make the intent (each (result, expected)
    // pair is the matching success case) explicit; collapsing them would
    // require pattern guards that obscure the table.
    #[allow(clippy::match_same_arms)]
    match (result, expected) {
        (Ok(_), Outcome::Ok) => {}
        (Err(LockError::Held { .. }), Outcome::Held) => {}
        (got, expected) => panic!("unexpected: got={got:?} expected={expected:?}"),
    }
}

#[tokio::test]
async fn write_with_session_takes_entity_excl_and_session_shared() {
    let (conn, inc) = setup().await;
    let session = ResourceKey::session("t1", "default", "s1");
    let entity_a = ResourceKey::entity("t1", "default", "rec_a");

    // Writer A: Session (Shared)
    let _writer_a_session = acquire(
        &conn,
        &session,
        LockMode::Shared,
        "writer_a",
        Duration::from_secs(5),
        &inc,
        "ingest",
    )
    .await
    .unwrap();

    // Writer B: Entity (Exclusive) AND Session (Shared) — both succeed
    // (Shared+Shared on session).
    let _writer_b_entity = acquire(
        &conn,
        &entity_a,
        LockMode::Exclusive,
        "writer_b",
        Duration::from_secs(5),
        &inc,
        "ingest",
    )
    .await
    .unwrap();
    let _writer_b_session = acquire(
        &conn,
        &session,
        LockMode::Shared,
        "writer_b",
        Duration::from_secs(5),
        &inc,
        "ingest",
    )
    .await
    .unwrap();

    // forget_session attempt: Session (Exclusive) → Held.
    let err = acquire(
        &conn,
        &session,
        LockMode::Exclusive,
        "forgetter",
        Duration::from_secs(5),
        &inc,
        "forget --session",
    )
    .await
    .unwrap_err();
    assert!(matches!(err, LockError::Held { .. }));
}
