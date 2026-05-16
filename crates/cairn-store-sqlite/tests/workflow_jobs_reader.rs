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
    let reader = SqliteWorkflowJobsReader::new(conn).expect("reader needs migration 0062");
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
    let reader = SqliteWorkflowJobsReader::new(conn).expect("reader needs migration 0062");
    assert!(reader.dead_letter_rows(10).is_empty());
    assert_eq!(reader.dead_letter_count(None), 0);
}

#[test]
fn oldest_queued_age_ms_returns_now_minus_next_run_for_queued() {
    let conn = fresh_db();
    insert_workflow_row(&conn, "j-a", "dream.light", "queued", 100, &[]);
    insert_workflow_row(&conn, "j-b", "dream.light", "queued", 200, &[]);
    let reader = SqliteWorkflowJobsReader::new(conn).expect("reader needs migration 0062");
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
    let reader = SqliteWorkflowJobsReader::new(conn).expect("reader needs migration 0062");
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
    let reader = SqliteWorkflowJobsReader::new(conn).expect("reader needs migration 0062");
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
    let reader = SqliteWorkflowJobsReader::new(conn).expect("reader needs migration 0062");
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
    let reader = SqliteWorkflowJobsReader::new(conn).expect("reader needs migration 0062");
    // min(lease_expires_at) is 800; now - 800 = 200.
    assert_eq!(reader.longest_held_lease_ms(1_000), Some(200));
}

#[test]
fn longest_held_lease_ms_empty_returns_none() {
    let conn = fresh_db();
    let reader = SqliteWorkflowJobsReader::new(conn).expect("reader needs migration 0062");
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
    let reader = SqliteWorkflowJobsReader::new(conn).expect("reader needs migration 0062");
    let rows = reader.dead_letter_rows(2);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].job_id.as_str(), "j-new");
    assert_eq!(rows[1].job_id.as_str(), "j-mid");
}

#[test]
fn take_last_error_captures_runtime_query_failure() {
    // Issue #92 round-7 finding 7.2: when a query method's SQL fails
    // at runtime (e.g. the table was dropped post-construction, the
    // DB is locked, or a column went missing under us), the reader
    // must NOT silently return `0` / `None` / empty `Vec` indistinguishable
    // from a healthy quiet vault. Instead the failure is captured in
    // an internal slot and surfaced through `take_last_error`, which
    // `workflow_health` drains and reports as a `DeferredCheck` Info
    // finding. This test exercises the round-trip end-to-end against
    // a real SQLite connection.
    let conn = fresh_db();
    let reader = SqliteWorkflowJobsReader::new(conn).expect("reader needs migration 0062");
    // Sanity check: a fresh DB has no errors to report.
    assert!(
        reader.take_last_error().is_none(),
        "fresh reader must have no last_error"
    );

    // Now break the table out from under the reader. Open a second
    // connection (the reader's connection is held in a Mutex but
    // SQLite shares the on-disk database; for in-memory DBs the
    // simplest path is to use ATTACH-like trickery, so instead we
    // explicitly take advantage of the fact that `open_in_memory_sync`
    // builds the schema and we can replicate that by reaching through
    // the public API: drop the table via a fresh connection won't see
    // the same memory DB. Use a file-backed DB instead so both
    // handles see the same data.
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let path = tmp.path().to_path_buf();
    // Drop the file so `open_sync` can lay down a fresh DB at this path.
    drop(tmp);
    let _staged = cairn_store_sqlite::open_sync(&path).expect("stage file-backed DB");
    // Build the reader against a fresh read-write connection.
    let reader_conn = Connection::open(&path).expect("open path for reader");
    let reader = SqliteWorkflowJobsReader::new(reader_conn).expect("reader needs migration 0062");
    // Independently break the table via a *separate* connection.
    let mutator = Connection::open(&path).expect("open path for mutator");
    mutator
        .execute("DROP TABLE workflow_jobs", [])
        .expect("drop workflow_jobs out from under the reader");
    drop(mutator);
    // The next query must fail under the covers. The reader returns
    // its empty sentinel (so workflow_health continues running) but
    // records the failure for take_last_error to surface.
    let count = reader.dead_letter_count(None);
    assert_eq!(count, 0, "broken table returns sentinel count");
    let err = reader
        .take_last_error()
        .expect("dropped table must register a last_error");
    assert!(
        err.contains("dead_letter_count"),
        "last_error must name the failing method: {err}"
    );
    // Drain semantics: subsequent take returns None until the next
    // failure occurs.
    assert!(
        reader.take_last_error().is_none(),
        "take_last_error drains the slot"
    );
    // A second failing call should re-arm the slot.
    let _ = reader.dead_letter_rows(10);
    assert!(
        reader.take_last_error().is_some(),
        "second failing call refills the slot"
    );
}

#[test]
fn new_rejects_db_missing_0062_dead_letter_index() {
    // Issue #92 round-7 finding 7.3: a vault with the migration-0062
    // columns present but the dead-letter index dropped passes the
    // column probe and ends up serving lint queries via full-table
    // scans. Surface the gap at construction time so the operator
    // can recreate the index.
    let conn = fresh_db();
    conn.execute("DROP INDEX workflow_jobs_dead_letter_idx", [])
        .expect("drop dead-letter idx to simulate drift");
    let err = SqliteWorkflowJobsReader::new(conn)
        .err()
        .expect("missing index must fail construction");
    match err {
        cairn_store_sqlite::SqliteWorkflowJobsReaderError::IndexMissing { name } => {
            assert_eq!(name, "workflow_jobs_dead_letter_idx");
        }
        other => panic!("expected IndexMissing, got {other:?}"),
    }
}

#[test]
fn new_rejects_db_missing_0062_kind_completed_index() {
    // Companion to the dead-letter-idx case: the kind+completed index
    // backs `last_success_ms` lookups. A missing index here turns
    // every `dream.light` / `expire.tier` / `evaluate.sweep` last-success
    // probe into a full scan. Fail loud at construction.
    let conn = fresh_db();
    conn.execute("DROP INDEX workflow_jobs_kind_completed_idx", [])
        .expect("drop kind+completed idx to simulate drift");
    let err = SqliteWorkflowJobsReader::new(conn)
        .err()
        .expect("missing index must fail construction");
    match err {
        cairn_store_sqlite::SqliteWorkflowJobsReaderError::IndexMissing { name } => {
            assert_eq!(name, "workflow_jobs_kind_completed_idx");
        }
        other => panic!("expected IndexMissing, got {other:?}"),
    }
}

#[test]
fn new_rejects_db_stuck_at_migration_0020() {
    // Issue #92 round-6 finding 6.2: a DB with only migration 0020
    // applied has no `failure_class`, `dead_letter_at_ms`, or
    // `completed_at_ms` columns. The reader's trait methods all
    // swallow the resulting `no such column` SQLite error via
    // `.ok().flatten()` / `.unwrap_or_default()`, so a `lint` run
    // against such a vault silently reports "no workflow issues" —
    // the worst possible failure mode for a health-check pipeline.
    // Probe the columns at construction time so the gap surfaces
    // loudly instead.
    let conn = Connection::open_in_memory().expect("in-memory db");
    // 0020 inserts into schema_migrations; create the bookkeeping
    // table first so we can apply the workflow_jobs migration in
    // isolation (full open_sync would apply 0062 too).
    conn.execute_batch(
        "CREATE TABLE schema_migrations (\
            migration_id INTEGER NOT NULL PRIMARY KEY, \
            name TEXT NOT NULL, \
            sql_hash TEXT NOT NULL DEFAULT '', \
            applied_at INTEGER NOT NULL \
         );",
    )
    .expect("create schema_migrations");
    conn.execute_batch(cairn_store_sqlite::migrations::WORKFLOW_JOBS_MIGRATION_SQL)
        .expect("apply 0020 only — simulate a vault that has not yet run 0062");
    let err = SqliteWorkflowJobsReader::new(conn)
        .err()
        .expect("0020-only DB must fail the column probe");
    let msg = err.to_string();
    assert!(
        msg.contains("migration 0062"),
        "error message must point operator at migration 0062: {msg}"
    );
    match err {
        cairn_store_sqlite::SqliteWorkflowJobsReaderError::ColumnMissing { name } => {
            // Probe order is implementation-defined — any of the
            // three migration-0062 columns is acceptable.
            assert!(
                matches!(
                    name,
                    "failure_class" | "dead_letter_at_ms" | "completed_at_ms"
                ),
                "unexpected missing column name: {name}"
            );
        }
        other => panic!("expected ColumnMissing, got {other:?}"),
    }
}
