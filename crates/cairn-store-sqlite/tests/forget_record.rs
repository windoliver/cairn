//! Issue #58: record-level forget through body-bearing WAL.
//!
//! Engine-level integration coverage. The CLI/MCP/SDK dispatch wiring
//! and the `wiring::FORGET_RECORD_WIRED` constant flip are deferred
//! to issue #9; these tests call `SqliteMemoryStore::forget_record`
//! directly to exercise the WAL apply path end-to-end.

#![allow(missing_docs)]

use std::sync::Arc;

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

// ── Task 9 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn forget_record_is_idempotent_under_repeat() {
    let store = open_in_memory().await.expect("open");
    let record = sample();
    let target = record.target_id.clone();

    store.upsert(&record).await.expect("upsert");

    let r1 = store
        .forget_record(&target, &alice())
        .await
        .expect("first forget");

    // Second call against the already-forgotten target. Records table is
    // empty, but the WAL apply path should still succeed: tombstone +
    // drains + purge are all no-ops, snapshot.purge stays no-op, and the
    // receipt comes back with the same target_id_hash.
    let r2 = store
        .forget_record(&target, &alice())
        .await
        .expect("second forget");

    assert_eq!(r1.target_id_hash, r2.target_id_hash);
    assert_ne!(r1.op_id, r2.op_id, "every call mints a fresh op_id");

    // Verify two COMMITTED forget_record ops landed in wal_ops.
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let count: i64 = conn
        .call(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM wal_ops \
                  WHERE kind = 'forget_record' AND state = 'COMMITTED'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("wal count");
    assert_eq!(count, 2, "both forget calls reach COMMITTED");
}

// ── Task 10 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn forget_record_scrubs_wal_pre_image_blobs() {
    let store = open_in_memory().await.expect("open");
    let record = sample();
    let target = record.target_id.clone();
    let body = record.body.clone();

    // First upsert seeds the row. Second upsert (with a mutated body)
    // forces snapshot.stage to capture a pre_image referencing the
    // target's lineage — the very blob we want to verify is body-free
    // post-forget.
    store.upsert(&record).await.expect("upsert v1");
    let mut v2 = record.clone();
    v2.body = format!("{body}-revised");
    store.upsert(&v2).await.expect("upsert v2");

    store
        .forget_record(&target, &alice())
        .await
        .expect("forget");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let target_str = target.as_str().to_owned();
    let body_str = body.clone();
    let leaks: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM wal_steps \
                  WHERE pre_image IS NOT NULL \
                    AND ( \
                          CAST(pre_image AS TEXT) LIKE '%' || ?1 || '%' \
                       OR CAST(pre_image AS TEXT) LIKE '%' || ?2 || '%' \
                    )",
                rusqlite::params![target_str, body_str],
                |row| row.get(0),
            )
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("scan");
    assert_eq!(
        leaks, 0,
        "no wal_steps.pre_image blob may reference the forgotten target id or body"
    );
}

// ── Task 11 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn forget_record_emits_body_free_consent_receipt() {
    use cairn_core::domain::{ConsentEvent, ConsentKind, ConsentPayload};

    let store = open_in_memory().await.expect("open");
    let record = sample();
    let target = record.target_id.clone();

    store.upsert(&record).await.expect("upsert");
    let receipt = store
        .forget_record(&target, &alice())
        .await
        .expect("forget");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let op_id = receipt.op_id.clone();

    let events: Vec<ConsentEvent> = conn
        .call(move |c| {
            cairn_store_sqlite::consent::query_by_op(c, &op_id)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("query consent");

    assert_eq!(events.len(), 1, "exactly one ForgetIntent receipt per op");
    let event = &events[0];
    assert!(matches!(event.kind, ConsentKind::ForgetIntent));
    match &event.payload {
        ConsentPayload::IntentReceipt {
            target_id_hash,
            reason_code,
            ..
        } => {
            assert_eq!(target_id_hash, &receipt.target_id_hash);
            assert_eq!(reason_code, "user_command");
        }
        other => panic!("expected IntentReceipt payload, got {other:?}"),
    }

    // Defense-in-depth: the JSON wire form must not contain any body-bearing
    // field NAME. Substring matching would false-positive on legitimate
    // values (e.g. reason_code "user_command" contains "command"), so
    // recurse the JSON value tree and check key names only — mirroring the
    // canonical `forbids_body_bearing_field_names_anywhere` test in
    // `cairn_core::domain::consent`.
    let value = serde_json::to_value(event).expect("serialize event");
    let mut keys = std::collections::BTreeSet::new();
    collect_json_keys(&value, &mut keys);
    for banned in ConsentEvent::BANNED_FIELDS {
        assert!(
            !keys.contains(*banned),
            "consent event JSON must not contain banned field {banned}; saw keys {keys:?}"
        );
    }
}

fn collect_json_keys(value: &serde_json::Value, out: &mut std::collections::BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                out.insert(k.clone());
                collect_json_keys(v, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_json_keys(item, out);
            }
        }
        _ => {}
    }
}
