//! Issue #92 — `SqliteWorkflowJobsReader` returns correct counts, ages,
//! and dead-letter rows against a real `workflow_jobs` table.
//!
//! Spec §4.8 (trait surface), §4.10 (lint check consumer).

use cairn_core::contract::job_store::{FailureClass, JobKind};
use cairn_core::contract::workflow_jobs::WorkflowJobsReader;
use cairn_store_sqlite::SqliteWorkflowJobsReader;
use cairn_store_sqlite::open_in_memory_sync;
use rusqlite::Connection;

fn fresh_db() -> Connection {
    open_in_memory_sync().expect("open in-memory store with vec0 + migrations")
}

/// Build a `Connection` that already contains `workflow_jobs` data; the
/// helper inserts directly (bypassing `JobStore`) so the test can stage
/// exactly the row shape the reader is queried against. Extras override
/// the helper's default column values when the column name matches.
fn insert_workflow_row(
    conn: &Connection,
    job_id: &str,
    kind: &str,
    state: &str,
    next_run_at: i64,
    extras: &[(&str, &dyn rusqlite::ToSql)],
) {
    let payload: Vec<u8> = Vec::new();
    // Base defaults satisfying CHECK constraints. Mutable so extras can
    // override individual columns (e.g. attempts, failure_class).
    let mut row: Vec<(&str, &dyn rusqlite::ToSql)> = vec![
        ("job_id", &job_id),
        ("kind", &kind),
        ("payload", &payload),
        ("state", &state),
        ("attempts", &0_i64),
        ("delivery_count", &0_i64),
        ("max_attempts", &3_i64),
        ("base_backoff_ms", &1_i64),
        ("backoff_multiplier", &2_i64),
        ("max_backoff_ms", &60_000_i64),
        ("next_run_at", &next_run_at),
        ("enqueued_at", &0_i64),
        ("updated_at", &0_i64),
    ];
    for (k, v) in extras {
        if let Some(slot) = row.iter_mut().find(|(name, _)| name == k) {
            slot.1 = *v;
        } else {
            row.push((k, *v));
        }
    }
    let col_names: Vec<&str> = row.iter().map(|(n, _)| *n).collect();
    let placeholders: Vec<String> = (1..=col_names.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "INSERT INTO workflow_jobs ({}) VALUES ({})",
        col_names.join(", "),
        placeholders.join(", ")
    );
    let params: Vec<&dyn rusqlite::ToSql> = row.iter().map(|(_, v)| *v).collect();
    conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
        .expect("insert workflow_jobs row");
}

#[test]
fn dead_letter_rows_returns_failed_row_with_typed_columns() {
    let conn = fresh_db();
    insert_workflow_row(
        &conn,
        "j-1",
        "dream.light",
        "failed",
        0,
        &[
            ("attempts", &3_i64),
            ("delivery_count", &3_i64),
            ("failure_class", &"validation"),
            ("dead_letter_at_ms", &500_i64),
            ("last_error", &"bad payload"),
        ],
    );
    let reader = SqliteWorkflowJobsReader::new(conn);
    let rows = reader.dead_letter_rows(10);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].job_id.as_str(), "j-1");
    assert_eq!(rows[0].kind.as_str(), "dream.light");
    assert_eq!(rows[0].attempts, 3);
    assert_eq!(rows[0].failure_class, FailureClass::Validation);
    assert_eq!(rows[0].last_error, "bad payload");
    assert_eq!(rows[0].dead_letter_at_ms, 500);

    assert_eq!(reader.dead_letter_count(None), 1);
    assert_eq!(
        reader.dead_letter_count(Some(&JobKind::new("dream.light"))),
        1
    );
    assert_eq!(
        reader.dead_letter_count(Some(&JobKind::new("expire.tier"))),
        0
    );
}

#[test]
fn dead_letter_rows_empty_table_returns_empty() {
    let conn = fresh_db();
    let reader = SqliteWorkflowJobsReader::new(conn);
    assert!(reader.dead_letter_rows(10).is_empty());
    assert_eq!(reader.dead_letter_count(None), 0);
}

#[test]
fn oldest_queued_age_ms_returns_now_minus_next_run_for_queued() {
    let conn = fresh_db();
    insert_workflow_row(&conn, "j-a", "dream.light", "queued", 100, &[]);
    insert_workflow_row(&conn, "j-b", "dream.light", "queued", 200, &[]);
    let reader = SqliteWorkflowJobsReader::new(conn);
    assert_eq!(reader.oldest_queued_age_ms(None, 1_000), Some(900));
    assert_eq!(
        reader.oldest_queued_age_ms(Some(&JobKind::new("dream.light")), 1_000),
        Some(900)
    );
    assert_eq!(
        reader.oldest_queued_age_ms(Some(&JobKind::new("expire.tier")), 1_000),
        None
    );
}

#[test]
fn oldest_queued_age_ms_empty_returns_none() {
    let conn = fresh_db();
    let reader = SqliteWorkflowJobsReader::new(conn);
    assert!(reader.oldest_queued_age_ms(None, 1_000).is_none());
}

#[test]
fn last_success_ms_returns_max_completed_for_kind() {
    let conn = fresh_db();
    insert_workflow_row(
        &conn,
        "j-done-1",
        "dream.light",
        "done",
        0,
        &[("completed_at_ms", &100_i64)],
    );
    insert_workflow_row(
        &conn,
        "j-done-2",
        "dream.light",
        "done",
        0,
        &[("completed_at_ms", &500_i64)],
    );
    insert_workflow_row(
        &conn,
        "j-done-3",
        "expire.tier",
        "done",
        0,
        &[("completed_at_ms", &200_i64)],
    );
    let reader = SqliteWorkflowJobsReader::new(conn);
    assert_eq!(
        reader.last_success_ms(&JobKind::new("dream.light")),
        Some(500)
    );
    assert_eq!(
        reader.last_success_ms(&JobKind::new("expire.tier")),
        Some(200)
    );
    assert_eq!(reader.last_success_ms(&JobKind::new("absent.kind")), None);
}

#[test]
fn last_success_ms_empty_returns_none() {
    let conn = fresh_db();
    let reader = SqliteWorkflowJobsReader::new(conn);
    assert!(
        reader
            .last_success_ms(&JobKind::new("dream.light"))
            .is_none()
    );
}

#[test]
fn longest_held_lease_ms_returns_now_minus_min_expires() {
    let conn = fresh_db();
    insert_workflow_row(
        &conn,
        "j-leased-1",
        "dream.light",
        "leased",
        0,
        &[
            ("lease_owner", &"worker-a"),
            ("lease_nonce", &"nonce-a"),
            ("lease_started", &1_i64),
            ("lease_expires_at", &800_i64),
        ],
    );
    insert_workflow_row(
        &conn,
        "j-leased-2",
        "dream.light",
        "leased",
        0,
        &[
            ("lease_owner", &"worker-b"),
            ("lease_nonce", &"nonce-b"),
            ("lease_started", &1_i64),
            ("lease_expires_at", &900_i64),
        ],
    );
    let reader = SqliteWorkflowJobsReader::new(conn);
    // min(lease_expires_at) is 800; now - 800 = 200.
    assert_eq!(reader.longest_held_lease_ms(1_000), Some(200));
}

#[test]
fn longest_held_lease_ms_empty_returns_none() {
    let conn = fresh_db();
    let reader = SqliteWorkflowJobsReader::new(conn);
    assert!(reader.longest_held_lease_ms(1_000).is_none());
}

#[test]
fn dead_letter_rows_orders_desc_and_respects_limit() {
    let conn = fresh_db();
    insert_workflow_row(
        &conn,
        "j-old",
        "dream.light",
        "failed",
        0,
        &[
            ("attempts", &3_i64),
            ("delivery_count", &3_i64),
            ("failure_class", &"transient"),
            ("dead_letter_at_ms", &100_i64),
            ("last_error", &"old"),
        ],
    );
    insert_workflow_row(
        &conn,
        "j-mid",
        "dream.light",
        "failed",
        0,
        &[
            ("attempts", &3_i64),
            ("delivery_count", &3_i64),
            ("failure_class", &"poison"),
            ("dead_letter_at_ms", &200_i64),
            ("last_error", &"mid"),
        ],
    );
    insert_workflow_row(
        &conn,
        "j-new",
        "dream.light",
        "failed",
        0,
        &[
            ("attempts", &3_i64),
            ("delivery_count", &3_i64),
            ("failure_class", &"validation"),
            ("dead_letter_at_ms", &300_i64),
            ("last_error", &"new"),
        ],
    );
    let reader = SqliteWorkflowJobsReader::new(conn);
    let rows = reader.dead_letter_rows(2);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].job_id.as_str(), "j-new");
    assert_eq!(rows[1].job_id.as_str(), "j-mid");
}
