//! Tests for `SqliteMemoryStore::list_trace_turns` (issue #90).

use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_store_sqlite::open_in_memory;
use cairn_test_fixtures::trace::sample_turn_summary;

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn returns_turns_in_sequence_order() {
    let store = open_in_memory().await.expect("open in-memory store");
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    for seq in 1..=5_u32 {
        store
            .upsert(&sample_turn_summary(session, seq))
            .await
            .expect("upsert");
    }

    let turns = store
        .list_trace_turns(session, 0, 10)
        .await
        .expect("list_trace_turns");

    assert_eq!(
        turns.iter().map(|h| h.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5],
        "turns must be returned in ascending sequence order"
    );
    for h in &turns {
        assert_eq!(
            h.session_id, session,
            "every header must carry the queried session_id"
        );
    }
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn skips_already_covered() {
    let store = open_in_memory().await.expect("open in-memory store");
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    for seq in 1..=5_u32 {
        store
            .upsert(&sample_turn_summary(session, seq))
            .await
            .expect("upsert");
    }

    let turns = store
        .list_trace_turns(session, 3, 10)
        .await
        .expect("list_trace_turns");

    assert_eq!(
        turns.iter().map(|h| h.sequence).collect::<Vec<_>>(),
        vec![4, 5],
        "only turns with sequence > 3 must be returned"
    );
}
