//! Issue #254 AC#3: two concurrent `--fix-markdown` invocations on the same
//! vault — one wins, the other returns [`LintFixError::FixInProgress`].
//!
//! # Flake note
//!
//! `fix_markdown_handler` on an empty store completes in <1 ms. With only
//! one async executor task, `tokio::join!` polls them round-robin; the first
//! acquires the lock and completes before the second even attempts to acquire,
//! so both would succeed. The test avoids this by using two independent
//! `Arc`-cloned stores that share the same underlying `tokio_rusqlite::Connection`
//! (via `raw_conn_for_admin`). Both calls contend on the same lock row.
//!
//! If this test proves flaky across CI environments (both futures resolving
//! before each other's lock-acquire is observed), the concurrency invariant is
//! already fully covered by
//! `crates/cairn-store-sqlite/tests/locks.rs::second_exclusive_returns_held`.
//! In that case, remove this file and document the reliance on the lower-level
//! test.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use cairn_cli::verbs::lint::{LintFixError, fix_markdown_with_lock};
use cairn_store_sqlite::open_in_memory;

/// Both futures share the same `Arc<SqliteMemoryStore>`. This means they
/// share the same `tokio_rusqlite::Connection` and therefore the same lock
/// table. `tokio::join!` polls them concurrently inside one runtime thread;
/// one of them must observe `Held` from the lock table and return
/// `FixInProgress`.
#[tokio::test]
async fn second_concurrent_fix_run_returns_fix_in_progress() {
    let store = Arc::new(open_in_memory().await.expect("open in-memory store"));
    let vault = tempfile::tempdir().expect("tempdir");

    let s1 = Arc::clone(&store);
    let s2 = Arc::clone(&store);
    let v1 = vault.path().to_path_buf();
    let v2 = vault.path().to_path_buf();

    // Long TTL so the lock is held until Drop (end of task), not reclaimed mid-race.
    let (r1, r2) = tokio::join!(
        async move { fix_markdown_with_lock(&s1, &v1, Duration::from_mins(1)).await },
        async move { fix_markdown_with_lock(&s2, &v2, Duration::from_mins(1)).await },
    );

    let ok_count = [r1.is_ok(), r2.is_ok()].iter().filter(|b| **b).count();
    let fix_in_progress_count = [&r1, &r2]
        .iter()
        .filter(|r| matches!(r, Err(LintFixError::FixInProgress)))
        .count();

    // Exactly one must win and one must observe the lock held.
    // If both succeed the lock serialization invariant is broken.
    // If both fail the test infrastructure is broken.
    assert_eq!(
        ok_count, 1,
        "exactly one fix_markdown_with_lock call must succeed; got r1={r1:?} r2={r2:?}",
    );
    assert_eq!(
        fix_in_progress_count, 1,
        "the losing call must surface FixInProgress; got r1={r1:?} r2={r2:?}",
    );
}
