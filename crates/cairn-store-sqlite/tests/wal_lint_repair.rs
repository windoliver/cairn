//! Issue #254: WAL helper for `lint_repair` op kind (§5.6 FSM).

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use cairn_store_sqlite::open_in_memory;
use cairn_store_sqlite::wal::lint_repair::{LintRepairWalError, abort, begin, commit, prepare};

/// Opens a fresh in-memory store and returns its raw admin connection.
async fn fresh_conn() -> Arc<tokio_rusqlite::Connection> {
    let store = open_in_memory().await.expect("open in-memory store");
    Arc::clone(
        store
            .raw_conn_for_admin()
            .expect("store has a live connection"),
    )
}

#[tokio::test]
async fn lifecycle_issued_prepared_committed() {
    let conn = fresh_conn().await;
    let op = begin(&conn, "vault-1", "holder-A", Duration::from_mins(1))
        .await
        .expect("begin");
    prepare(&conn, &op).await.expect("prepare");
    commit(&conn, &op).await.expect("commit");
}

#[tokio::test]
async fn lifecycle_aborted_from_prepared_with_reason() {
    let conn = fresh_conn().await;
    let op = begin(&conn, "vault-1", "holder-A", Duration::from_mins(1))
        .await
        .expect("begin");
    prepare(&conn, &op).await.expect("prepare");
    abort(&conn, &op, "user cancelled").await.expect("abort");
}

#[tokio::test]
async fn cannot_skip_to_committed_from_issued() {
    let conn = fresh_conn().await;
    let op = begin(&conn, "v", "h", Duration::from_mins(1))
        .await
        .expect("begin");
    let r = commit(&conn, &op).await;
    assert!(
        matches!(r, Err(LintRepairWalError::IllegalTransition(_))),
        "got {r:?}"
    );
}

#[tokio::test]
async fn second_commit_is_illegal_terminal() {
    let conn = fresh_conn().await;
    let op = begin(&conn, "v", "h", Duration::from_mins(1))
        .await
        .expect("begin");
    prepare(&conn, &op).await.expect("prepare");
    commit(&conn, &op).await.expect("commit-1");
    let r = commit(&conn, &op).await;
    assert!(
        matches!(r, Err(LintRepairWalError::IllegalTransition(_))),
        "got {r:?}"
    );
}

#[tokio::test]
async fn issued_seq_strictly_advances_across_two_begins() {
    let conn = fresh_conn().await;
    let op1 = begin(&conn, "v", "h", Duration::from_mins(1))
        .await
        .expect("begin-1");
    let op2 = begin(&conn, "v", "h", Duration::from_mins(1))
        .await
        .expect("begin-2");
    assert_ne!(op1, op2);
    // Verify both rows exist in the DB.
    let count: i64 = conn
        .call(|c| {
            Ok::<_, tokio_rusqlite::Error>(c.query_row(
                "SELECT COUNT(*) FROM wal_ops WHERE kind='lint_repair'",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .expect("count");
    assert_eq!(count, 2);
}
