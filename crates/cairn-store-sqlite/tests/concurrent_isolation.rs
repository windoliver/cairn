// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Concurrent read/write isolation test for `with_apply_tx`.
//!
//! Proves that a concurrent `list` call issued while a `with_apply_tx` is in
//! progress (after `stage_version` but before `COMMIT`) does NOT observe the
//! writer's uncommitted row. This guards against the bug where the mutex was
//! released between `BEGIN IMMEDIATE` and `COMMIT`, allowing reads to race in
//! and see in-progress state.

use std::collections::BTreeMap;
use std::time::Duration;

use cairn_core::contract::memory_store::{
    MemoryStore,
    apply::MemoryStoreApply,
    types::{ListQuery, TargetId},
};
use cairn_core::domain::{
    ChainRole, EvidenceVector, MemoryClass, MemoryKind, MemoryVisibility, Provenance,
    Rfc3339Timestamp, ScopeTuple,
    actor_chain::ActorChainEntry,
    identity::Identity,
    principal::Principal,
    record::{Ed25519Signature, MemoryRecord, RecordId},
};
use cairn_core::wal::test_apply_token;
use cairn_store_sqlite::SqliteMemoryStore;
use tempfile::tempdir;

fn make_record(id_ulid: &str, body: &str) -> MemoryRecord {
    let user_id = Identity::parse("usr:tafeng").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:tafeng".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "a".repeat(64)),
            consent_ref: "consent:01HQZ".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-22T14:05:11Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "a".repeat(128))).expect("valid"),
        tags: vec!["isolation-test".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

/// A reader that runs while a writer is sleeping mid-transaction must see
/// pre-write state (zero rows), not the uncommitted staged row.
///
/// The writer:
///   1. Acquires the connection mutex (held for the full tx).
///   2. Issues `BEGIN IMMEDIATE`.
///   3. Calls `stage_version` (row inserted, active=0).
///   4. Sleeps 50 ms (simulating CPU work mid-tx) — uses `std::thread::sleep`
///      because the closure is sync.
///   5. Calls `activate_version` (row becomes active=1).
///   6. Returns `Ok(())` → `COMMIT`.
///
/// The reader runs during that 50 ms window (step 4). Because the mutex is
/// held by the writer the reader blocks on `blocking_lock()` inside
/// `spawn_blocking` and only proceeds after `COMMIT`. It therefore observes
/// exactly 1 row (the now-committed row), not 0 (before write) or a dirty
/// intermediate state.
///
/// This test uses `flavor = "multi_thread"` so both tasks actually run in
/// parallel on separate OS threads.
#[tokio::test(flavor = "multi_thread")]
async fn reader_does_not_see_uncommitted_staged_row() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    // Use Arc so both tasks can hold a reference to the same store.
    let store = std::sync::Arc::new(store);

    let target = TargetId::new("isolation-target-1");
    let record = make_record("01HQZX9F5N0000000000000010", "concurrent isolation body");

    // Channel: writer signals reader that BEGIN + stage_version are done, so
    // the reader fires mid-transaction rather than after commit.
    let (tx_ready, rx_ready) = tokio::sync::oneshot::channel::<()>();

    // --- Writer task ---
    let store_w = store.clone();
    let target_w = target.clone();
    let record_w = record.clone();
    let writer = tokio::spawn(async move {
        store_w
            .with_apply_tx(test_apply_token(), move |tx| {
                tx.stage_version(&target_w, &record_w, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&target_w, 1, None)?;

                // Signal the reader that stage+activate are done; then sleep
                // so the reader has time to attempt its `list` while the mutex
                // is (should be) still held by us.
                let _ = tx_ready.send(());
                std::thread::sleep(Duration::from_millis(50));

                Ok(())
            })
            .await
            .expect("writer must succeed");
    });

    // --- Reader task ---
    // Wait until the writer has staged + activated, then fire a `list` that
    // should block on the held mutex and only return after the writer commits.
    let store_r = store.clone();
    let reader = tokio::spawn(async move {
        // Wait for the writer to reach mid-transaction.
        rx_ready.await.expect("writer must signal");

        // This call will block on `blocking_lock()` inside `spawn_blocking`
        // until the writer releases the guard (after COMMIT).
        let principal = Principal::system(&test_apply_token());
        store_r
            .list(&ListQuery::new(principal))
            .await
            .expect("list must not error")
    });

    let (writer_result, list_result) = tokio::join!(writer, reader);
    writer_result.expect("writer task must not panic");
    let list_result = list_result.expect("reader task must not panic");

    // The reader unblocked after COMMIT, so it must see exactly 1 row.
    assert_eq!(
        list_result.rows.len(),
        1,
        "reader must see exactly 1 committed row (not 0 pre-write or dirty intermediate)"
    );
    assert_eq!(
        list_result.rows[0].body, record.body,
        "committed record body must match"
    );
}
