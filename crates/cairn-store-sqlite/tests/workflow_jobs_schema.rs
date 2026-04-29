//! Schema-level guarantees for the `workflow_jobs` table (migration 0011).
//!
//! These exercise the CHECK constraints and state-machine triggers without
//! pulling in any orchestrator code. The orchestrator's behaviour lives in
//! `cairn-workflows`; here we just pin the substrate.

use cairn_store_sqlite::open_in_memory_sync as open_in_memory;
use rusqlite::params;

const INSERT_QUEUED: &str = "\
    INSERT INTO workflow_jobs \
        (job_id, kind, payload, state, attempts, max_attempts, queue_key, \
         dedupe_key, next_run_at, lease_owner, lease_expires_at, last_error, \
         enqueued_at, updated_at) \
    VALUES (?, ?, ?, 'queued', 0, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?)";

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
              (job_id, kind, payload, state, attempts, max_attempts, queue_key, dedupe_key, \
               next_run_at, lease_owner, lease_expires_at, last_error, enqueued_at, updated_at) \
             VALUES ('j', 'k', x'', 'queued', 0, 3, NULL, NULL, 0, 'worker-a', 0, NULL, 0, 0)",
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
            SET state = 'leased', lease_owner = ?, lease_expires_at = ?, attempts = attempts + 1, \
                updated_at = ? \
          WHERE job_id = 'j'",
        params!["worker-a", 1_000_i64, 1_i64],
    )
    .expect("queued -> leased");
    conn.execute(
        "UPDATE workflow_jobs \
            SET state = 'done', lease_owner = NULL, lease_expires_at = NULL, updated_at = ? \
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
fn queue_key_unique_for_active_states_only() {
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
    let err = conn
        .execute(
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
        .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("unique"));

    // Once j1 finishes, a new queue_key=qk row is allowed.
    conn.execute(
        "UPDATE workflow_jobs \
            SET state = 'leased', lease_owner = 'w', lease_expires_at = 1, attempts = 1, \
                updated_at = 1 \
          WHERE job_id = 'j1'",
        [],
    )
    .expect("lease");
    conn.execute(
        "UPDATE workflow_jobs \
            SET state = 'done', lease_owner = NULL, lease_expires_at = NULL, updated_at = 2 \
          WHERE job_id = 'j1'",
        [],
    )
    .expect("done");
    conn.execute(
        INSERT_QUEUED,
        params![
            "j3",
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
    .expect("second queued after first finished");
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
}

#[test]
fn attempts_cannot_exceed_max() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO workflow_jobs \
              (job_id, kind, payload, state, attempts, max_attempts, queue_key, dedupe_key, \
               next_run_at, lease_owner, lease_expires_at, last_error, enqueued_at, updated_at) \
             VALUES ('j', 'k', x'', 'queued', 5, 3, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)",
            [],
        )
        .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("check"));
}
