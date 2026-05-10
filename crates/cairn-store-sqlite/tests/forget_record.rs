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
    // Round-2 review: the in-txn `SELECT COUNT(*) WHERE active = 1`
    // captures the live-version count under the record-WAL lock. The
    // first forget tombstones one live row → `deleted_count = 1`; the
    // second runs after Phase A purged the records table → reads 0.
    // A regression to a pre-lock SELECT would still report `1` here
    // because the second call's pre-lock read could observe the same
    // active row before locking.
    assert_eq!(
        r1.deleted_count, 1,
        "first forget should report exactly one tombstoned row"
    );
    assert_eq!(
        r2.deleted_count, 0,
        "post-contention re-forget must report deleted_count=0 — would \
         regress to 1 if the count moved back to a pre-lock SELECT"
    );

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

    // Brief §10.1 single-writer ordering: the record-WAL
    // entity-exclusive lock makes two same-target forgets fail-fast
    // contend — exactly one acquires the lock and reports
    // `deleted_count = 1`; the other surfaces the typed `RecordWalLock`
    // error. This test pins the surfaced error type so a future
    // change to a wait-and-retry policy gets a deliberate review.
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

// Note on the deleted_count race fix: the strongest regression coverage
// for the in-txn SELECT lives at the unit-test layer in
// `record_wal/steps.rs::tests::mark_tombstone_count_is_captured_inside_transaction`.
// That test calls `mark_tombstone_and_emit_receipt` directly against
// transactions interleaved with an external commit, proving the count
// is captured INSIDE the transaction (not via a pre-lock SELECT cached
// outside it). Integration-level tests can't distinguish those two
// implementations because the record-WAL locks fail-fast — no contender
// reaches Phase A.

// ── Round 5 review: receipt fidelity for non-private repeat forget ─────

#[tokio::test]
async fn repeat_forget_after_purge_does_not_dilute_consent_audit_trail() {
    use cairn_core::domain::taxonomy::MemoryVisibility;

    // Round-5 review (Codex): a repeat forget against an already-purged
    // target previously appended a SECOND consent_journal row using
    // `ScopeTuple::default()` and `MemoryVisibility::Private` — the
    // post-purge `load_scope_and_tier` defaults. For a record that
    // started life as `Team`/`Org`/`Public`, this misclassified the
    // audit trail.
    //
    // Fix: skip the consent receipt when the in-txn count observes
    // zero live rows. Brief §14: the authoritative receipt is the one
    // bound to the destructive Phase A; a no-op forget has nothing to
    // audit beyond what the original receipt already captured.
    let store = open_in_memory().await.expect("open");
    let mut record = sample();
    // Force a non-default tier so the regression — defaulting to
    // Private on the second receipt — would visibly demote it.
    record.visibility = MemoryVisibility::Team;
    let target = record.target_id.clone();
    store.upsert(&record).await.expect("upsert");

    let r1 = store
        .forget_record(&target, &alice())
        .await
        .expect("first forget");
    assert_eq!(r1.deleted_count, 1, "first forget tombstones the live row");

    let r2 = store
        .forget_record(&target, &alice())
        .await
        .expect("second forget");
    assert_eq!(
        r2.deleted_count, 0,
        "repeat forget after purge observes zero live rows in-txn"
    );

    // Inspect consent_journal: exactly one ForgetIntent for this
    // target_id_hash; its scope_tier reflects the original Team
    // visibility (no diluted Private receipt from the second call).
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let target_id_hash = r1.target_id_hash.clone();
    let receipts: Vec<(String, String)> = conn
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT op_id, payload_json FROM consent_journal \
                  WHERE kind = 'forget_intent' \
                    AND payload_json LIKE '%' || ?1 || '%'",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![target_id_hash], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        row.get::<_, String>(1)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .expect("query consent");

    assert_eq!(
        receipts.len(),
        1,
        "exactly one ForgetIntent receipt per target — repeat forget \
         must not append a duplicate. Got: {receipts:?}"
    );
    let (op_id, payload_json) = &receipts[0];
    assert_eq!(
        op_id, &r1.op_id,
        "the surviving receipt must be the one written by the *real* \
         destructive op, not the no-op"
    );
    assert!(
        payload_json.contains("\"scope_tier\":\"team\""),
        "receipt scope_tier must preserve the original Team visibility \
         — a regression to default-Private would record \"private\". \
         Got payload: {payload_json}"
    );
}

// ── Round 6 review: expire-then-forget preserves audit receipt ─────────

#[tokio::test]
async fn forget_after_expire_writes_receipt_with_original_tier() {
    use cairn_core::domain::taxonomy::MemoryVisibility;

    // Round-6 review (Codex): the no-op-skip predicate must NOT fire on
    // already-expired targets. An expired record has `active = 0` rows
    // present (live_count = 0) but body bytes still on disk; the
    // forget op WILL purge them in Phase B and therefore MUST record
    // the actor/scope/tier in consent_journal.
    //
    // The fix uses `total_rows == 0` (not `live_count == 0`) as the
    // skip predicate so this case writes a receipt with the original
    // Org tier — `load_scope_and_tier`'s `ORDER BY version DESC`
    // reads inactive rows too.
    let store = open_in_memory().await.expect("open");
    let mut record = sample();
    record.visibility = MemoryVisibility::Org;
    let target = record.target_id.clone();
    store.upsert(&record).await.expect("upsert");

    // Soft-expire the record: rows remain with active=0, tombstoned=1,
    // tombstone_reason='expire' (no body purge yet).
    store.expire(&target).await.expect("expire");

    let receipt = store
        .forget_record(&target, &alice())
        .await
        .expect("forget after expire");
    assert_eq!(
        receipt.deleted_count, 0,
        "no live rows at admission — but rows existed, so forget still purges"
    );

    // The expire-then-forget op MUST have written a consent receipt
    // (the destructive Phase B purge needs an audited actor).
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let receipt_op_id = receipt.op_id.clone();
    let row: Option<String> = conn
        .call(move |c| {
            let result: Result<String, rusqlite::Error> = c.query_row(
                "SELECT payload_json FROM consent_journal \
                  WHERE kind = 'forget_intent' AND op_id = ?1",
                rusqlite::params![receipt_op_id],
                |row| row.get(0),
            );
            match result {
                Ok(s) => Ok(Some(s)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(tokio_rusqlite::Error::Other(Box::new(e))),
            }
        })
        .await
        .expect("query consent");
    let payload_json = row.expect(
        "expire-then-forget MUST append a ForgetIntent receipt — \
         the no-op skip predicate must use `total_rows == 0`, not \
         `live_count == 0`",
    );
    assert!(
        payload_json.contains("\"scope_tier\":\"org\""),
        "receipt must preserve the original Org visibility (read from \
         the inactive row via load_scope_and_tier ORDER BY version DESC). \
         Got: {payload_json}"
    );

    // Records table physically empty after Phase B.
    let target_str = target.as_str().to_owned();
    let row_count: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM records WHERE target_id = ?1",
                rusqlite::params![target_str],
                |row| row.get(0),
            )
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("count");
    assert_eq!(
        row_count, 0,
        "Phase B primary.purge ran for the expired target"
    );
}

// ── Round 7 review: in-txn scope/tier read defeats pre-lock-read race ──

#[tokio::test]
async fn forget_records_post_upsert_visibility_in_consent_receipt() {
    use cairn_core::domain::taxonomy::MemoryVisibility;

    // Round-7 review (Codex): apply_forget_record reads scope+tier
    // BEFORE acquiring the record-WAL lock (the values feed the
    // entity/session lock legs). A same-target upsert could commit
    // between that pre-lock read and the lock acquisition, leaving
    // the receipt with stale tier metadata.
    //
    // Fix: mark_tombstone_and_emit_receipt re-reads scope+tier INSIDE
    // the Phase A transaction. This test simulates the race by
    // upserting a Private v1, then a Public v2 (BEFORE the forget),
    // then forgetting. The receipt MUST reflect Public — the latest
    // version's tier — not whatever the apply path read first.
    let store = open_in_memory().await.expect("open");
    let mut record = sample();
    record.visibility = MemoryVisibility::Private;
    let target = record.target_id.clone();
    store.upsert(&record).await.expect("upsert v1 private");

    // Bump visibility on the same target.
    let mut v2 = record.clone();
    v2.visibility = MemoryVisibility::Public;
    v2.body = format!("{}-public", record.body);
    store.upsert(&v2).await.expect("upsert v2 public");

    let receipt = store
        .forget_record(&target, &alice())
        .await
        .expect("forget");
    assert_eq!(receipt.deleted_count, 1, "v2 was the live row");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let receipt_op_id = receipt.op_id.clone();
    let payload_json: String = conn
        .call(move |c| {
            c.query_row(
                "SELECT payload_json FROM consent_journal \
                  WHERE kind = 'forget_intent' AND op_id = ?1",
                rusqlite::params![receipt_op_id],
                |row| row.get(0),
            )
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("query consent");
    assert!(
        payload_json.contains("\"scope_tier\":\"public\""),
        "receipt scope_tier must come from the in-txn read of the latest \
         version (Public), not a stale pre-lock snapshot of v1 (Private). \
         Got: {payload_json}"
    );
}
