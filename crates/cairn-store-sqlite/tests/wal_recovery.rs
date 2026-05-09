//! Integration tests for boot-recovery (issue #55, brief §5.6).
//!
//! Uses a synthetic step body keyed on `WalKind::Upsert` (the upsert step
//! graph has 6 ords, plenty for the scenarios). Each test seeds `wal_ops`
//! and `wal_steps` directly, runs `recover_pending`, then asserts on the
//! resulting `wal_ops.state` and on the synthetic body's call counts.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use cairn_core::wal::{OperationId, WalKind};
use cairn_store_sqlite::open_in_memory;
use cairn_store_sqlite::wal::{
    RecoveryConfig, StepBody, StepBodyError, StepBodyRegistry, recover_pending,
};
use rusqlite::Transaction;
use rusqlite::params;
use tokio_rusqlite::Connection;

#[derive(Clone, Copy, Debug)]
enum BodyBehavior {
    Succeed,
    FailOnceThenSucceed,
    AlwaysFail,
}

struct SyntheticBody {
    behaviors: Vec<BodyBehavior>,
    /// Per-ord call counters — `[ord]` increments on every call to `run`.
    call_counts: Vec<AtomicU32>,
}

impl SyntheticBody {
    fn new(behaviors: Vec<BodyBehavior>) -> Arc<Self> {
        let n = behaviors.len();
        Arc::new(Self {
            behaviors,
            call_counts: (0..n).map(|_| AtomicU32::new(0)).collect(),
        })
    }

    fn calls(&self, ord: u32) -> u32 {
        self.call_counts[ord as usize].load(Ordering::SeqCst)
    }
}

impl StepBody for SyntheticBody {
    // The two `Ok(())` arms (`Succeed` always, `FailOnceThenSucceed` when
    // count > 1) are intentionally distinct match arms — collapsing them
    // with an or-pattern would obscure the per-behavior semantics that
    // make the synthetic body easy to extend.
    #[allow(clippy::match_same_arms)]
    fn run(
        &self,
        _tx: &mut Transaction<'_>,
        _op_id: &OperationId,
        step: &cairn_core::wal::StepDef,
    ) -> Result<(), StepBodyError> {
        let count = self.call_counts[step.ord as usize].fetch_add(1, Ordering::SeqCst) + 1;
        match self.behaviors[step.ord as usize] {
            BodyBehavior::Succeed => Ok(()),
            BodyBehavior::FailOnceThenSucceed if count == 1 => {
                Err(StepBodyError::Failed("synthetic fail-once".into()))
            }
            BodyBehavior::FailOnceThenSucceed => Ok(()),
            BodyBehavior::AlwaysFail => Err(StepBodyError::Failed("synthetic always-fail".into())),
        }
    }
}

struct OneKindRegistry {
    kind: WalKind,
    body: Arc<dyn StepBody>,
    requested_ops: parking_lot::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl StepBodyRegistry for OneKindRegistry {
    async fn body_for(
        &self,
        _conn: &Arc<Connection>,
        kind: WalKind,
        op_id: &OperationId,
    ) -> Result<Option<Arc<dyn StepBody>>, cairn_store_sqlite::wal::RecoveryError> {
        self.requested_ops.lock().push(op_id.as_str().to_owned());
        if kind == self.kind {
            Ok(Some(Arc::clone(&self.body)))
        } else {
            Ok(None)
        }
    }
}

/// Open an in-memory async connection with the migrated schema applied.
///
/// We piggy-back on `open_in_memory`, which runs `bootstrap` (PRAGMAs +
/// migrations to head), then borrow the underlying admin connection — this
/// is the same pattern `tests/locks.rs` uses.
async fn open_db() -> Arc<Connection> {
    let store = open_in_memory().await.expect("open in-memory store");
    Arc::clone(
        store
            .raw_conn_for_admin()
            .expect("store has a live connection"),
    )
}

async fn seed_op(conn: &Arc<Connection>, op_id: &str, kind: WalKind, state: &str, issued_seq: i64) {
    let op = op_id.to_owned();
    let kind_str = kind.as_str().to_owned();
    let state_str = state.to_owned();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, '{}', 'i', 'h', '{}', 0, 'sig', 0, 0)",
            params![op, issued_seq, kind_str, state_str],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("seed wal_ops");
}

async fn forward_to_prepared(conn: &Arc<Connection>, op_id: &str) {
    let op = op_id.to_owned();
    conn.call(move |c| {
        c.execute(
            "UPDATE wal_ops SET state = 'PREPARED', updated_at = 1 WHERE operation_id = ?1",
            params![op],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("forward to PREPARED");
}

async fn forward_to_committed(conn: &Arc<Connection>, op_id: &str) {
    let op = op_id.to_owned();
    conn.call(move |c| {
        c.execute(
            "UPDATE wal_ops SET state = 'PREPARED', updated_at = 1 WHERE operation_id = ?1",
            params![op.clone()],
        )?;
        c.execute(
            "UPDATE wal_ops SET state = 'COMMITTED', updated_at = 2 WHERE operation_id = ?1",
            params![op],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("forward to COMMITTED");
}

async fn seed_step(
    conn: &Arc<Connection>,
    op_id: &str,
    ord: u32,
    name: &str,
    state: &str,
    attempts: u32,
) {
    let op = op_id.to_owned();
    let name_owned = name.to_owned();
    let state_owned = state.to_owned();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_steps \
               (operation_id, step_ord, step_kind, state, attempts) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![op, ord, name_owned, state_owned, attempts],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("seed wal_steps");
}

async fn seed_payload_json(
    conn: &Arc<Connection>,
    op_id: &str,
    kind: WalKind,
    payload_json: String,
) {
    let op = op_id.to_owned();
    let kind_str = kind.as_str().to_owned();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
             VALUES (?1, ?2, ?3, 0)",
            params![op, kind_str, payload_json],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("seed wal_payloads");
}

async fn read_op_state(conn: &Arc<Connection>, op_id: &str) -> String {
    let op = op_id.to_owned();
    conn.call(move |c| {
        let s: String = c.query_row(
            "SELECT state FROM wal_ops WHERE operation_id = ?1",
            params![op],
            |r| r.get(0),
        )?;
        Ok::<_, tokio_rusqlite::Error>(s)
    })
    .await
    .expect("read op state")
}

async fn read_step_row(conn: &Arc<Connection>, op_id: &str, ord: u32) -> (String, u32) {
    let op = op_id.to_owned();
    conn.call(move |c| {
        let row: (String, u32) = c.query_row(
            "SELECT state, attempts FROM wal_steps \
             WHERE operation_id = ?1 AND step_ord = ?2",
            params![op, ord],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)),
        )?;
        Ok::<_, tokio_rusqlite::Error>(row)
    })
    .await
    .expect("read step row")
}

fn upsert_step_names() -> Vec<&'static str> {
    cairn_core::wal::UPSERT_STEPS
        .iter()
        .map(|s| s.name)
        .collect()
}

fn upsert_with_body(body: Arc<dyn StepBody>) -> RecoveryConfig {
    RecoveryConfig {
        enabled: true,
        bodies: Box::new(OneKindRegistry {
            kind: WalKind::Upsert,
            body,
            requested_ops: parking_lot::Mutex::new(Vec::new()),
        }),
    }
}

// ------------------------------------------------------------------
// Scenarios
// ------------------------------------------------------------------

#[tokio::test]
async fn empty_wal_recovery_returns_empty_report() {
    let conn = open_db().await;
    let report = recover_pending(&conn, &RecoveryConfig::default())
        .await
        .expect("recover");
    assert!(report.finalized_committed.is_empty());
    assert!(report.finalized_rejected.is_empty());
    assert!(report.aborted.is_empty());
    assert!(report.resumed_committed.is_empty());
    assert!(report.skipped_no_body.is_empty());
    assert!(report.no_op.is_empty());
    assert!(report.skipped_unhandled_kind.is_empty());
}

#[tokio::test]
async fn issued_orphan_finalizes_rejected() {
    let conn = open_db().await;
    seed_op(&conn, "op-issued", WalKind::Upsert, "ISSUED", 1).await;

    let report = recover_pending(&conn, &RecoveryConfig::default())
        .await
        .expect("recover");

    assert_eq!(report.finalized_rejected.len(), 1);
    assert_eq!(read_op_state(&conn, "op-issued").await, "REJECTED");
}

#[tokio::test]
async fn terminal_committed_is_idempotent_under_repeated_recovery() {
    let conn = open_db().await;
    seed_op(&conn, "op-done", WalKind::Upsert, "ISSUED", 1).await;
    forward_to_committed(&conn, "op-done").await;

    for _ in 0..3 {
        let report = recover_pending(&conn, &RecoveryConfig::default())
            .await
            .expect("recover");
        // A terminal COMMITTED op is filtered out by `list_open_ops`
        // (which only returns ISSUED / PREPARED rows), so the recovery
        // pass produces an empty report.
        assert!(report.no_op.is_empty());
        assert!(report.finalized_committed.is_empty());
        assert!(report.finalized_rejected.is_empty());
        assert!(report.aborted.is_empty());
        assert!(report.resumed_committed.is_empty());
    }
    assert_eq!(read_op_state(&conn, "op-done").await, "COMMITTED");
}

#[tokio::test]
async fn prepared_no_steps_resumes_from_zero_with_body() {
    let conn = open_db().await;
    seed_op(&conn, "op-prep", WalKind::Upsert, "ISSUED", 1).await;
    forward_to_prepared(&conn, "op-prep").await;

    let body = SyntheticBody::new(vec![BodyBehavior::Succeed; 6]);
    let cfg = upsert_with_body(body.clone());

    let report = recover_pending(&conn, &cfg).await.expect("recover");

    assert_eq!(report.resumed_committed.len(), 1);
    assert_eq!(read_op_state(&conn, "op-prep").await, "COMMITTED");
    for ord in 0..6 {
        assert_eq!(body.calls(ord), 1, "step {ord} should run exactly once");
    }
}

#[tokio::test]
async fn prepared_partial_resumes_from_next_step_only() {
    let conn = open_db().await;
    seed_op(&conn, "op-partial", WalKind::Upsert, "ISSUED", 1).await;
    forward_to_prepared(&conn, "op-partial").await;
    let names = upsert_step_names();
    seed_step(&conn, "op-partial", 0, names[0], "DONE", 1).await;
    seed_step(&conn, "op-partial", 1, names[1], "DONE", 1).await;

    let body = SyntheticBody::new(vec![BodyBehavior::Succeed; 6]);
    let cfg = upsert_with_body(body.clone());

    let report = recover_pending(&conn, &cfg).await.expect("recover");

    assert_eq!(report.resumed_committed.len(), 1);
    assert_eq!(read_op_state(&conn, "op-partial").await, "COMMITTED");
    // Steps 0,1 already DONE — body NOT re-invoked.
    assert_eq!(body.calls(0), 0, "step 0 already DONE; body must not run");
    assert_eq!(body.calls(1), 0, "step 1 already DONE; body must not run");
    for ord in 2..6 {
        assert_eq!(body.calls(ord), 1, "step {ord} should run exactly once");
    }
    // Steps 0,1 were pre-seeded as DONE with attempts=1 and stay that way.
    assert_eq!(
        read_step_row(&conn, "op-partial", 0).await,
        ("DONE".into(), 1)
    );
    assert_eq!(
        read_step_row(&conn, "op-partial", 1).await,
        ("DONE".into(), 1)
    );
    // Steps 2..=5 ran exactly once via the runner's fresh-row path: attempts=1.
    for ord in 2..6 {
        assert_eq!(
            read_step_row(&conn, "op-partial", ord).await,
            ("DONE".into(), 1),
            "step {ord} should be DONE with attempts=1"
        );
    }
}

#[tokio::test]
async fn retry_exhaustion_aborts_op() {
    let conn = open_db().await;
    seed_op(&conn, "op-fail", WalKind::Upsert, "ISSUED", 1).await;
    forward_to_prepared(&conn, "op-fail").await;

    // Body succeeds on step 0, always fails on step 1.
    let mut behaviors = vec![BodyBehavior::Succeed; 6];
    behaviors[1] = BodyBehavior::AlwaysFail;
    let body = SyntheticBody::new(behaviors);
    let cfg = upsert_with_body(body.clone());

    let report = recover_pending(&conn, &cfg).await.expect("recover");

    assert_eq!(report.aborted.len(), 1);
    assert_eq!(report.aborted[0].1, 1, "step 1 should be the failed ord");
    assert_eq!(read_op_state(&conn, "op-fail").await, "ABORTED");
    // Step 1 body called 3 times (MAX_STEP_ATTEMPTS).
    assert_eq!(body.calls(1), cairn_core::wal::MAX_STEP_ATTEMPTS);
    // wal_steps row for step 1 must be durably FAILED with attempts == MAX.
    let (state, attempts) = read_step_row(&conn, "op-fail", 1).await;
    assert_eq!(state, "FAILED");
    assert_eq!(attempts, cairn_core::wal::MAX_STEP_ATTEMPTS);
}

#[tokio::test]
async fn recovery_stops_at_durable_attempt_ceiling() {
    let conn = open_db().await;
    seed_op(&conn, "op-near-ceiling", WalKind::Upsert, "ISSUED", 1).await;
    forward_to_prepared(&conn, "op-near-ceiling").await;
    let names = upsert_step_names();
    seed_step(
        &conn,
        "op-near-ceiling",
        0,
        names[0],
        "FAILED",
        cairn_core::wal::MAX_STEP_ATTEMPTS - 1,
    )
    .await;

    let body = SyntheticBody::new(vec![BodyBehavior::AlwaysFail; 6]);
    let cfg = upsert_with_body(body.clone());

    let report = recover_pending(&conn, &cfg).await.expect("recover");

    assert_eq!(report.aborted.len(), 1);
    assert_eq!(report.aborted[0].1, 0, "step 0 should hit the ceiling");
    assert_eq!(read_op_state(&conn, "op-near-ceiling").await, "ABORTED");
    assert_eq!(
        body.calls(0),
        1,
        "only one additional body invocation is allowed at MAX - 1"
    );
    assert_eq!(
        read_step_row(&conn, "op-near-ceiling", 0).await,
        ("FAILED".into(), cairn_core::wal::MAX_STEP_ATTEMPTS)
    );
}

#[tokio::test]
async fn recovery_rejects_payload_variant_that_disagrees_with_wal_kind() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = Arc::clone(
        store
            .raw_conn_for_admin()
            .expect("store has a live connection"),
    );
    seed_op(&conn, "op-kind-mismatch", WalKind::Expire, "ISSUED", 1).await;
    forward_to_prepared(&conn, "op-kind-mismatch").await;

    let record = cairn_core::domain::record::tests_export::sample_record();
    let payload = cairn_store_sqlite::record_wal::payload::UpsertPayload::new_for_test(record);
    let payload_json = serde_json::to_string(
        &cairn_store_sqlite::record_wal::payload::RecordWalPayload::Upsert(payload),
    )
    .expect("payload json");
    seed_payload_json(&conn, "op-kind-mismatch", WalKind::Expire, payload_json).await;

    let cfg = RecoveryConfig {
        enabled: true,
        bodies: Box::new(cairn_store_sqlite::record_wal::RecordWalRegistry::new(
            Arc::clone(store.incarnation().expect("incarnation")),
        )),
    };

    let err = recover_pending(&conn, &cfg)
        .await
        .expect_err("kind/payload mismatch must fail recovery");
    assert!(
        err.to_string().contains("payload"),
        "error should identify payload mismatch: {err}"
    );
    assert_eq!(read_op_state(&conn, "op-kind-mismatch").await, "PREPARED");
}

#[tokio::test]
async fn fault_injection_fail_once_then_succeed() {
    let conn = open_db().await;
    seed_op(&conn, "op-flake", WalKind::Upsert, "ISSUED", 1).await;
    forward_to_prepared(&conn, "op-flake").await;

    // Step 2 fails on attempt 1, succeeds on attempt 2.
    let mut behaviors = vec![BodyBehavior::Succeed; 6];
    behaviors[2] = BodyBehavior::FailOnceThenSucceed;
    let body = SyntheticBody::new(behaviors);
    let cfg = upsert_with_body(body.clone());

    let report = recover_pending(&conn, &cfg).await.expect("recover");

    assert_eq!(report.resumed_committed.len(), 1);
    assert_eq!(read_op_state(&conn, "op-flake").await, "COMMITTED");
    // Step 2 called twice (1 fail + 1 success); other steps once each.
    assert_eq!(body.calls(2), 2);
    assert_eq!(body.calls(0), 1);
    assert_eq!(body.calls(5), 1);
}

#[tokio::test]
async fn repeated_recovery_after_partial_is_idempotent() {
    let conn = open_db().await;
    seed_op(&conn, "op-rep", WalKind::Upsert, "ISSUED", 1).await;
    forward_to_prepared(&conn, "op-rep").await;
    let names = upsert_step_names();
    seed_step(&conn, "op-rep", 0, names[0], "DONE", 1).await;

    let body = SyntheticBody::new(vec![BodyBehavior::Succeed; 6]);
    let cfg = upsert_with_body(body.clone());

    // First pass: drives PREPARED -> COMMITTED.
    let r1 = recover_pending(&conn, &cfg).await.expect("recover #1");
    assert_eq!(r1.resumed_committed.len(), 1);

    // Two more passes should be no-ops: the op is now terminal COMMITTED,
    // so `list_open_ops` filters it out and the report is empty.
    for i in 2..=3 {
        let r = recover_pending(&conn, &cfg)
            .await
            .unwrap_or_else(|e| panic!("recover #{i}: {e}"));
        assert!(r.no_op.is_empty());
        assert!(r.resumed_committed.is_empty());
        assert!(r.aborted.is_empty());
    }

    // Each step body called exactly once across all 3 passes.
    for ord in 1..6 {
        assert_eq!(body.calls(ord), 1, "step {ord} body must run once");
    }
    // Step 0 was pre-seeded DONE — body never invoked.
    assert_eq!(body.calls(0), 0, "step 0 was pre-DONE; body must not run");
    // After 3 recovery passes, every wal_steps row is durably DONE with
    // attempts=1 — recovery never re-stamped already-DONE rows.
    for ord in 0..6 {
        assert_eq!(
            read_step_row(&conn, "op-rep", ord).await,
            ("DONE".into(), 1),
            "step {ord} must be DONE with attempts=1 after repeated recovery"
        );
    }
}

#[tokio::test]
async fn decision_only_mode_skips_resume_with_warn() {
    let conn = open_db().await;
    seed_op(&conn, "op-skip", WalKind::Upsert, "ISSUED", 1).await;
    forward_to_prepared(&conn, "op-skip").await;

    // Default config = EmptyRegistry — no bodies registered.
    let cfg = RecoveryConfig::default();
    let report = recover_pending(&conn, &cfg).await.expect("recover");

    assert_eq!(report.skipped_no_body.len(), 1);
    assert!(report.resumed_committed.is_empty());
    assert!(report.aborted.is_empty());
    // Op stays in PREPARED — recovery did not advance it.
    assert_eq!(read_op_state(&conn, "op-skip").await, "PREPARED");
}

/// Wiring smoke test for Task 10: opens a tempdir-backed DB through the
/// production async `open` path, pre-seeds a PREPARED op + all 6 upsert
/// step DONE rows, drops the store, then re-opens through the same async
/// `open` path and asserts the second open ran `run_boot_recovery` and
/// finalized the op to COMMITTED.
#[tokio::test]
async fn open_path_runs_boot_recovery_and_finalizes_committed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cairn.sqlite");

    // First open via the production file-path async open.
    {
        let store = cairn_store_sqlite::open(&path).await.expect("open #1");
        let conn = Arc::clone(
            store
                .raw_conn_for_admin()
                .expect("store has a live connection"),
        );

        // Seed a PREPARED op with all 6 upsert steps DONE — terminal-finalizable.
        seed_op(&conn, "op-wire", WalKind::Upsert, "ISSUED", 1).await;
        forward_to_prepared(&conn, "op-wire").await;
        let names = upsert_step_names();
        for (ord, name) in names.iter().enumerate() {
            let ord_u32 = u32::try_from(ord).expect("invariant: upsert step ord fits in u32");
            seed_step(&conn, "op-wire", ord_u32, name, "DONE", 1).await;
        }
        assert_eq!(read_op_state(&conn, "op-wire").await, "PREPARED");
        // Drop the store/conn (closes connection); `tempdir` keeps the DB file.
    }

    // Second open via the SAME production async path. `run_boot_recovery`
    // should fire from inside `open()` and finalize the op to COMMITTED
    // before the store is returned.
    let store = cairn_store_sqlite::open(&path).await.expect("open #2");
    let conn = Arc::clone(
        store
            .raw_conn_for_admin()
            .expect("store has a live connection"),
    );

    assert_eq!(
        read_op_state(&conn, "op-wire").await,
        "COMMITTED",
        "boot recovery must finalize a PREPARED op with all DONE steps"
    );
}
