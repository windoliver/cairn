//! Issue #58: record-level forget through body-bearing WAL.
//!
//! Engine-level integration coverage. The CLI dispatch wiring and the
//! `wiring::FORGET_RECORD_WIRED_CLI` constant flip landed in #58; SDK and
//! MCP wiring (`FORGET_RECORD_WIRED_SDK` / `_MCP`) are deferred to #9.
//! These tests call `SqliteMemoryStore::forget_record` directly to
//! exercise the WAL apply path end-to-end.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::{ForgetReceipt, KeywordSearchArgs, ListArgs, MemoryStore};
use cairn_core::domain::{Identity, MemoryRecord};
use cairn_store_sqlite::{open, open_in_memory};

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

// ── Task 12 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn forget_record_phase_a_crash_leaves_no_state() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cairn.sqlite");

    let seed = sample();
    let target = seed.target_id.clone();

    {
        let store = open(&path).await.expect("open");
        store.upsert(&seed).await.expect("upsert seed");

        // Stage a PREPARED forget_record op + payload but skip the step
        // runner so Phase A never commits its tombstone.
        let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
        let target_str = target.as_str().to_owned();
        let payload_json = serde_json::to_string(
            &cairn_store_sqlite::record_wal::payload::RecordWalPayload::Forget(Box::new(
                cairn_store_sqlite::record_wal::payload::ForgetPayload {
                    target_id: target.clone(),
                    scope: seed.scope.clone(),
                    reason_code: "user_command".to_owned(),
                    actor: alice(),
                    scope_tier: cairn_core::domain::taxonomy::MemoryVisibility::Private,
                },
            )),
        )
        .expect("serialize");
        conn.call(move |c| {
            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES ('op-crash-58', \
                         COALESCE((SELECT MAX(issued_seq) FROM wal_ops),0)+1, \
                         'forget_record','PREPARED','{}','test', \
                         ?1, '{}', 0, 'sig', 1, 1)",
                rusqlite::params![target_str],
            )?;
            c.execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES ('op-crash-58', 'forget_record', ?1, 1)",
                rusqlite::params![payload_json],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed PREPARED op");
    } // drop store — simulates crash before Phase A txn could commit

    // Reopen and assert recovery completes the prepared op.
    let store = open(&path).await.expect("reopen");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let state: String = conn
        .call(|c| {
            c.query_row(
                "SELECT state FROM wal_ops WHERE operation_id = 'op-crash-58'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("read op state");
    assert_eq!(
        state, "COMMITTED",
        "boot-time recovery resumed the prepared forget op to COMMITTED"
    );

    // The seed record must be gone from every reader after recovery.
    let post = store
        .get_active_by_target(&target)
        .await
        .expect("post lookup");
    assert!(post.is_none(), "recovered forget purges the target");
}

// ── Task 13 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn forget_record_phase_b_crash_resumes_from_last_done_step() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cairn.sqlite");

    let seed = sample();
    let target = seed.target_id.clone();

    {
        let store = open(&path).await.expect("open");
        store.upsert(&seed).await.expect("upsert seed");

        // Stage a PREPARED forget op + payload + a wal_steps row
        // marking step 0 (primary.mark_tombstone) DONE. Apply the
        // tombstone effect manually so the on-disk state matches
        // "Phase A commit succeeded, crashed before step 1."
        let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
        let target_str = target.as_str().to_owned();
        let payload_json = serde_json::to_string(
            &cairn_store_sqlite::record_wal::payload::RecordWalPayload::Forget(Box::new(
                cairn_store_sqlite::record_wal::payload::ForgetPayload {
                    target_id: target.clone(),
                    scope: seed.scope.clone(),
                    reason_code: "user_command".to_owned(),
                    actor: alice(),
                    scope_tier: cairn_core::domain::taxonomy::MemoryVisibility::Private,
                },
            )),
        )
        .expect("serialize");
        let target_for_tx = target_str.clone();
        conn.call(move |c| {
            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES ('op-phaseb-58', \
                         COALESCE((SELECT MAX(issued_seq) FROM wal_ops),0)+1, \
                         'forget_record','PREPARED','{}','test', \
                         ?1, '{}', 0, 'sig', 1, 1)",
                rusqlite::params![target_for_tx],
            )?;
            c.execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES ('op-phaseb-58', 'forget_record', ?1, 1)",
                rusqlite::params![payload_json],
            )?;
            // Mark step 0 DONE so recovery resumes from step 1.
            c.execute(
                "INSERT INTO wal_steps \
                   (operation_id, step_ord, step_kind, state, attempts, \
                    started_at, finished_at) \
                 VALUES ('op-phaseb-58', 0, 'primary.mark_tombstone', 'DONE', 1, 1, 2)",
                [],
            )?;
            // Apply the tombstone effect of step 0 directly.
            c.execute(
                "UPDATE records \
                    SET active = 0, tombstoned = 1, tombstone_reason = 'forget' \
                  WHERE target_id = ?1",
                rusqlite::params![target_for_tx],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed half-run state");
    }

    // Reopen — recovery picks up the prepared op and runs steps 1-6.
    let store = open(&path).await.expect("reopen");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let (state, done_steps): (String, i64) = conn
        .call(|c| {
            let s: String = c.query_row(
                "SELECT state FROM wal_ops WHERE operation_id = 'op-phaseb-58'",
                [],
                |row| row.get(0),
            )?;
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM wal_steps \
                  WHERE operation_id = 'op-phaseb-58' AND state = 'DONE'",
                [],
                |row| row.get(0),
            )?;
            Ok((s, n))
        })
        .await
        .expect("query state");

    assert_eq!(
        state, "COMMITTED",
        "recovery drove the prepared op to COMMITTED"
    );
    assert_eq!(
        done_steps, 7,
        "every step in FORGET_RECORD_STEPS reached DONE during recovery"
    );

    let target_for_count = target.as_str().to_owned();
    let row_count: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM records WHERE target_id = ?1",
                rusqlite::params![target_for_count],
                |row| row.get(0),
            )
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("scan records");
    assert_eq!(row_count, 0, "Phase B primary.purge ran during recovery");
}

// ── Round 2 review: deleted_count is captured under the lock ───────────

#[tokio::test]
async fn concurrent_forgets_serialize_via_record_wal_lock() {
    use std::sync::Arc as StdArc;

    // In-process concurrency: two tokio tasks call forget_record on the
    // same target through the same SqliteMemoryStore. The record-WAL
    // entity-exclusive lock (brief §10.1 single-writer ordering)
    // serializes them — exactly one acquires the lock and reports
    // `deleted_count = 1`; the other fails fast with `RecordWalLock`.
    //
    // Round-2 review concern: a pre-lock SELECT could let two
    // concurrent forgets both observe `active = 1` and both report
    // `deleted_count = 1`. The fix moves the count read INSIDE the
    // Phase A transaction (under the same lock), so the second forget
    // — even if it eventually retried past the lock — would see
    // `active = 0` and report `0`.
    let store = StdArc::new(open_in_memory().await.expect("open"));
    let record = sample();
    let target = record.target_id.clone();
    store.upsert(&record).await.expect("upsert");

    let s1 = StdArc::clone(&store);
    let s2 = StdArc::clone(&store);
    let t1 = target.clone();
    let t2 = target.clone();
    let h1 = tokio::spawn(async move { s1.forget_record(&t1, &alice()).await });
    let h2 = tokio::spawn(async move { s2.forget_record(&t2, &alice()).await });

    let r1 = h1.await.expect("task1 join");
    let r2 = h2.await.expect("task2 join");

    let outcomes: Vec<Result<u64, String>> = [r1, r2]
        .into_iter()
        .map(|r| match r {
            Ok(receipt) => Ok(receipt.deleted_count),
            Err(e) => Err(e.to_string()),
        })
        .collect();

    let successes: Vec<u64> = outcomes
        .iter()
        .filter_map(|o| o.as_ref().ok().copied())
        .collect();
    let failures: Vec<&String> = outcomes.iter().filter_map(|o| o.as_ref().err()).collect();

    assert_eq!(
        successes,
        vec![1u64],
        "exactly one concurrent forget must succeed with deleted_count=1; \
         successes={successes:?} failures={failures:?}"
    );
    assert_eq!(
        failures.len(),
        1,
        "the other concurrent forget must fail-fast on lock contention; got {failures:?}"
    );
    assert!(
        failures[0].contains("record wal lock") || failures[0].contains("RecordWalLock"),
        "loser must surface the typed RecordWalLock error; got {:?}",
        failures[0]
    );
}
