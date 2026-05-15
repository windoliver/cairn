//! Schema-level guarantees for the `workflow_jobs` table (migration 0020).
//!
//! These exercise the CHECK constraints and state-machine triggers without
//! pulling in any orchestrator code. The orchestrator's behaviour lives in
//! `cairn-workflows`; here we just pin the substrate.

use cairn_store_sqlite::open_in_memory_sync as open_in_memory;
use rusqlite::params;

const INSERT_QUEUED: &str = "\
    INSERT INTO workflow_jobs \
        (job_id, kind, payload, state, attempts, delivery_count, max_attempts, \
         base_backoff_ms, backoff_multiplier, max_backoff_ms, \
         queue_key, dedupe_key, next_run_at, \
         lease_owner, lease_nonce, lease_started, lease_expires_at, last_error, \
         enqueued_at, updated_at) \
    VALUES (?, ?, ?, 'queued', 0, 0, ?, 1000, 2, 60000, ?, ?, ?, \
            NULL, NULL, NULL, NULL, NULL, ?, ?)";

#[test]
fn enqueue_minimal_row_succeeds() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        INSERT_QUEUED,
        params![
            "j1",
            "dream.light",
            &b""[..],
            3,
            None::<&str>,
            None::<&str>,
            0_i64,
            0_i64,
            0_i64
        ],
    )
    .expect("insert queued job");
}

#[test]
fn queued_row_with_owner_rejected() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO workflow_jobs \
              (job_id, kind, payload, state, attempts, delivery_count, max_attempts, \
               base_backoff_ms, backoff_multiplier, max_backoff_ms, \
               queue_key, dedupe_key, next_run_at, \
               lease_owner, lease_nonce, lease_started, lease_expires_at, last_error, \
               enqueued_at, updated_at) \
             VALUES ('j', 'k', x'', 'queued', 0, 0, 3, 1000, 2, 60000, NULL, NULL, 0, \
                     'worker-a', NULL, NULL, 0, NULL, 0, 0)",
            [],
        )
        .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("check"));
}

#[test]
fn lease_transition_succeeds_and_terminal_absorbing() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        INSERT_QUEUED,
        params![
            "j",
            "k",
            &b""[..],
            3,
            None::<&str>,
            None::<&str>,
            0_i64,
            0_i64,
            0_i64
        ],
    )
    .expect("insert");
    conn.execute(
        "UPDATE workflow_jobs \
            SET state = 'leased', lease_owner = ?, lease_nonce = 'n1', \
                lease_started = 0, lease_expires_at = ?, \
                attempts = attempts + 1, delivery_count = delivery_count + 1, updated_at = ? \
          WHERE job_id = 'j'",
        params!["worker-a", 1_000_i64, 1_i64],
    )
    .expect("queued -> leased");
    conn.execute(
        "UPDATE workflow_jobs \
            SET state = 'done', lease_owner = NULL, lease_nonce = NULL, \
                lease_started = NULL, lease_expires_at = NULL, updated_at = ? \
          WHERE job_id = 'j'",
        params![2_i64],
    )
    .expect("leased -> done");

    let err = conn
        .execute(
            "UPDATE workflow_jobs SET state = 'queued', updated_at = 3 WHERE job_id = 'j'",
            [],
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("absorbing") || msg.contains("transition not allowed"),
        "expected terminal-state rejection, got: {msg}"
    );
}

#[test]
fn illegal_transition_blocked() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        INSERT_QUEUED,
        params![
            "j",
            "k",
            &b""[..],
            3,
            None::<&str>,
            None::<&str>,
            0_i64,
            0_i64,
            0_i64
        ],
    )
    .expect("insert");
    let err = conn
        .execute(
            "UPDATE workflow_jobs SET state = 'done', updated_at = 1 WHERE job_id = 'j'",
            [],
        )
        .unwrap_err();
    assert!(format!("{err}").contains("transition not allowed"));
}

#[test]
fn queue_key_unique_only_when_leased() {
    // The queue_key partial-unique index covers `state = 'leased'`
    // only — many queued rows for the same key may coexist (the
    // scheduler serializes them at lease time, per brief §10 v0.1).
    // The index fires only when a second row tries to enter `leased`
    // with the same queue_key.
    let conn = open_in_memory().expect("open");
    conn.execute(
        INSERT_QUEUED,
        params![
            "j1",
            "k",
            &b""[..],
            3,
            "qk",
            None::<&str>,
            0_i64,
            0_i64,
            0_i64
        ],
    )
    .expect("first queued for queue_key");
    // Second queued row with the same queue_key must succeed.
    conn.execute(
        INSERT_QUEUED,
        params![
            "j2",
            "k",
            &b""[..],
            3,
            "qk",
            None::<&str>,
            0_i64,
            0_i64,
            0_i64
        ],
    )
    .expect("second queued for same queue_key allowed");

    // Lease j1 — fine.
    conn.execute(
        "UPDATE workflow_jobs \
            SET state = 'leased', lease_owner = 'w', lease_nonce = 'n1', \
                lease_started = 0, lease_expires_at = 1, \
                attempts = 1, delivery_count = 1, updated_at = 1 \
          WHERE job_id = 'j1'",
        [],
    )
    .expect("lease j1");
    // Trying to lease j2 with the same queue_key while j1 is leased
    // must fail the unique index.
    let err = conn
        .execute(
            "UPDATE workflow_jobs \
                SET state = 'leased', lease_owner = 'w', lease_nonce = 'n2', \
                    lease_started = 0, lease_expires_at = 1, \
                    attempts = 1, delivery_count = 1, updated_at = 1 \
              WHERE job_id = 'j2'",
            [],
        )
        .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("unique"));
}

#[test]
fn dedupe_key_unique_per_kind() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        INSERT_QUEUED,
        params![
            "j1",
            "kindA",
            &b""[..],
            3,
            None::<&str>,
            "op-1",
            0_i64,
            0_i64,
            0_i64
        ],
    )
    .expect("first dedupe row");
    let err = conn
        .execute(
            INSERT_QUEUED,
            params![
                "j2",
                "kindA",
                &b""[..],
                3,
                None::<&str>,
                "op-1",
                0_i64,
                0_i64,
                0_i64
            ],
        )
        .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("unique"));

    // Different kind, same dedupe_key allowed.
    conn.execute(
        INSERT_QUEUED,
        params![
            "j3",
            "kindB",
            &b""[..],
            3,
            None::<&str>,
            "op-1",
            0_i64,
            0_i64,
            0_i64
        ],
    )
    .expect("different kind allowed");

    // After a row reaches a terminal state, its dedupe_key slot
    // frees up — operators must be able to safely replay an
    // operation_id whose first attempt failed. Walk j1 through
    // queued -> leased -> failed, then a fresh queued row with the
    // same (kind, dedupe_key) must be allowed.
    conn.execute(
        "UPDATE workflow_jobs \
            SET state = 'leased', lease_owner = 'w', lease_nonce = 'n', \
                lease_started = 0, lease_expires_at = 1, \
                attempts = 1, delivery_count = 1, updated_at = 1 \
          WHERE job_id = 'j1'",
        [],
    )
    .expect("lease j1");
    conn.execute(
        "UPDATE workflow_jobs \
            SET state = 'failed', lease_owner = NULL, lease_nonce = NULL, \
                lease_started = NULL, lease_expires_at = NULL, updated_at = 2 \
          WHERE job_id = 'j1'",
        [],
    )
    .expect("fail j1");
    conn.execute(
        INSERT_QUEUED,
        params![
            "j4",
            "kindA",
            &b""[..],
            3,
            None::<&str>,
            "op-1",
            0_i64,
            0_i64,
            0_i64
        ],
    )
    .expect("dedupe slot must free up after terminal state");
}

#[test]
fn migration_0062_adds_dead_letter_columns() {
    // Run the full migration chain on a fresh in-memory DB (canonical
    // opener registers the vec0 extension migration 0022 needs) and
    // assert the three nullable columns introduced by 0062 are present.
    let conn = open_in_memory().expect("open");
    let cols: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('workflow_jobs') ORDER BY cid")
        .expect("prepare")
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .map(Result::unwrap)
        .collect();
    assert!(
        cols.contains(&"failure_class".to_string()),
        "failure_class column missing; cols = {cols:?}"
    );
    assert!(
        cols.contains(&"dead_letter_at_ms".to_string()),
        "dead_letter_at_ms column missing; cols = {cols:?}"
    );
    assert!(
        cols.contains(&"completed_at_ms".to_string()),
        "completed_at_ms column missing; cols = {cols:?}"
    );

    // Sanity-check that the new indexes were created.
    let indexes: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_schema \
              WHERE type = 'index' AND tbl_name = 'workflow_jobs' \
                AND name IN ('workflow_jobs_dead_letter_idx', \
                             'workflow_jobs_kind_completed_idx') \
              ORDER BY name",
        )
        .expect("prepare")
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        indexes,
        vec![
            "workflow_jobs_dead_letter_idx".to_string(),
            "workflow_jobs_kind_completed_idx".to_string(),
        ]
    );
}

#[test]
fn attempts_cannot_exceed_max() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO workflow_jobs \
              (job_id, kind, payload, state, attempts, delivery_count, max_attempts, \
               base_backoff_ms, backoff_multiplier, max_backoff_ms, \
               queue_key, dedupe_key, next_run_at, \
               lease_owner, lease_nonce, lease_started, lease_expires_at, last_error, \
               enqueued_at, updated_at) \
             VALUES ('j', 'k', x'', 'queued', 5, 5, 3, 1000, 2, 60000, NULL, NULL, 0, \
                     NULL, NULL, NULL, NULL, NULL, 0, 0)",
            [],
        )
        .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("check"));
}
