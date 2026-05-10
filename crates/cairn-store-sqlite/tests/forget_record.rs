//! Issue #58: record-level forget through body-bearing WAL.
//!
//! Engine-level integration coverage. The CLI/MCP/SDK dispatch wiring
//! and the `wiring::FORGET_RECORD_WIRED` constant flip are deferred
//! to issue #9; these tests call `SqliteMemoryStore::forget_record`
//! directly to exercise the WAL apply path end-to-end.

#![allow(missing_docs)]

use cairn_core::contract::memory_store::{ForgetReceipt, KeywordSearchArgs, ListArgs, MemoryStore};
use cairn_core::domain::{Identity, MemoryRecord};
use cairn_store_sqlite::open_in_memory;

fn sample() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

fn alice() -> Identity {
    Identity::parse("hmn:alice:v1").expect("identity")
}

fn make_keyword_args(query: &str, record: &MemoryRecord) -> KeywordSearchArgs<'static> {
    KeywordSearchArgs {
        query: query.to_owned(),
        filter: None,
        auth_scope: record.scope.clone(),
        visibility_allowlist: vec![record.visibility],
        limit: 10,
        cursor: None,
        with_explain: false,
    }
}

// ── Task 8 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn forget_record_removes_content_from_every_reader() {
    let store = open_in_memory().await.expect("open");
    let record = sample();
    let target = record.target_id.clone();
    let body = record.body.clone();

    store.upsert(&record).await.expect("upsert seed record");

    // Pre-condition: the seeded record is visible.
    let pre_list = store
        .list(&ListArgs {
            limit: 10,
            ..ListArgs::default()
        })
        .await
        .expect("list pre");
    assert_eq!(
        pre_list.records.len(),
        1,
        "list returns the seeded record before forget"
    );

    let receipt: ForgetReceipt = store
        .forget_record(&target, &alice())
        .await
        .expect("forget_record");

    // Post-condition 1: list / get_active_by_target return nothing.
    let post_list = store
        .list(&ListArgs {
            limit: 10,
            ..ListArgs::default()
        })
        .await
        .expect("list post");
    assert!(
        post_list.records.is_empty(),
        "list returns no rows after forget"
    );

    let post_active = store
        .get_active_by_target(&target)
        .await
        .expect("get_active_by_target post");
    assert!(post_active.is_none(), "no active record after forget");

    // Post-condition 2: keyword search misses every body token.
    let body_substr = body.split_whitespace().next().unwrap_or("user");
    let kw_args = make_keyword_args(body_substr, &record);
    let kw_page = store
        .search_keyword(&kw_args)
        .await
        .expect("keyword search");
    assert!(
        kw_page.candidates.is_empty(),
        "keyword search misses the forgotten body"
    );

    // Post-condition 3: receipt is body-free and well-shaped.
    assert!(
        receipt.target_id_hash.starts_with("sha256:"),
        "receipt carries sha256-prefixed hash, not raw target id"
    );
    assert!(receipt.op_id.starts_with("forget_record-"));
    assert!(receipt.purged_at > 0);
}
