//! Issue #254 AC#4: crash-recovery for lint --fix-markdown.
//!
//! Two scenarios: TTL-based lock reclaim after crash, and partial-write repair.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::memory_store::{ListArgs, MemoryStore as _};
use cairn_core::domain::projection::{MarkdownProjector, body_hash};
use cairn_store_sqlite::locks::acquire_exclusive;

#[tokio::test]
async fn ttl_expired_lock_is_reclaimed_by_next_acquire() {
    let store = cairn_store_sqlite::open_in_memory()
        .await
        .expect("open in-memory store");
    let conn = Arc::clone(
        store
            .raw_conn_for_admin()
            .expect("store has a live connection"),
    );

    // "Crashed" holder: short TTL, then forget the handle (no Drop release fires).
    let h = acquire_exclusive(
        &conn,
        "lint_repair",
        "v",
        "crashed",
        Duration::from_millis(50),
    )
    .await
    .expect("first acquire");
    std::mem::forget(h);

    // Wait past TTL so the next acquire's reclaim sweep observes expiry.
    // Bumped to 500ms for test reliability; TTL reclaim on busy systems
    // may take longer than a bare milliseconds sleep due to scheduler jitter.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let _h2 = acquire_exclusive(&conn, "lint_repair", "v", "next", Duration::from_mins(1))
        .await
        .expect("reclaim after TTL expiry");
}

#[tokio::test]
async fn partial_write_is_repaired_by_fix_markdown_with_lock() {
    use cairn_test_fixtures::store::sample_record;

    let store = Arc::new(
        cairn_store_sqlite::open_in_memory()
            .await
            .expect("open in-memory store"),
    );
    let r = sample_record();
    store.upsert(&r).await.expect("upsert");

    let stored = store
        .list_active_stored(&ListArgs::default())
        .await
        .expect("list")
        .into_iter()
        .next()
        .expect("at least one active record");
    let projected = MarkdownProjector.project(&stored);

    let vault = tempfile::tempdir().expect("tempdir");
    let abs = vault.path().join(&projected.path);
    std::fs::create_dir_all(abs.parent().expect("parent")).expect("mkdir");
    std::fs::write(&abs, "PARTIAL WRITE\n").expect("write garbage");

    let result = cairn_cli::verbs::lint::fix_markdown_with_lock(
        &store,
        vault.path(),
        Duration::from_mins(1),
    )
    .await
    .expect("fix");
    assert_eq!(result.written.len(), 1, "exactly one file rewritten");

    let on_disk = std::fs::read_to_string(&abs).expect("read repaired");
    assert_eq!(
        body_hash(&on_disk),
        body_hash(&projected.content),
        "repaired file must match canonical projection by body_hash",
    );
}
