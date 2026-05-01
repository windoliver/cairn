//! Integration tests for migration 0030: 4-column weighted FTS5 over
//! `(kind, class, scope, body)`.
//!
//! Pins:
//! - The shadow table indexes four columns, so weighted `bm25()` (Task 5)
//!   has somewhere to plug positional weights into.
//! - A weighted `bm25()` query orders results sensibly (both records
//!   matching `alice` come back).
//! - The UPDATE trigger re-syncs all four columns, so a `kind`
//!   reclassification flows to the FTS shadow without a manual rebuild.

use cairn_store_sqlite::open_in_memory_sync;
use rusqlite::params;

/// Standard insert template used across the tests in this module. Mirrors
/// the column list used in `tests/migrations.rs::fts_round_trip` so the
/// shapes line up.
const INSERT_SQL: &str = "INSERT INTO records (
        record_id, target_id, version, path, kind, class, visibility,
        scope, actor_chain, body, body_hash, created_at, updated_at,
        active, tombstoned, is_static
    ) VALUES (?, ?, 1, ?, ?, ?, 'private', 'u/a/p', '[]', ?, 'h', 0, 0, 1, 0, 0)";

#[test]
fn fts_table_has_four_indexed_columns() {
    let conn = open_in_memory_sync().expect("open_in_memory_sync");
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('records_fts')",
            [],
            |r| r.get(0),
        )
        .expect("pragma_table_info");
    assert!(
        n >= 4,
        "expected at least 4 columns in records_fts, got {n}",
    );
}

#[test]
fn weighted_bm25_returns_results() {
    let conn = open_in_memory_sync().expect("open_in_memory_sync");
    let mut stmt = conn.prepare(INSERT_SQL).expect("prepare insert");
    stmt.execute(params![
        "rec_a",
        "tgt_a",
        "p/a",
        "identity",
        "person",
        "alice chen at novapay",
    ])
    .expect("insert A");
    stmt.execute(params![
        "rec_b",
        "tgt_b",
        "p/b",
        "note",
        "casual",
        "alice mentioned in notes",
    ])
    .expect("insert B");

    let mut q = conn
        .prepare(
            "SELECT record_id FROM records_fts \
             JOIN records ON records.rowid = records_fts.rowid \
             WHERE records_fts MATCH 'alice' \
             ORDER BY bm25(records_fts, 10.0, 10.0, 5.0, 1.0) \
             LIMIT 10",
        )
        .expect("prepare query");
    let ids: Vec<String> = q
        .query_map(params![], |r| r.get::<_, String>(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(ids.len(), 2, "expected both records to match alice");
}

#[test]
fn fts_trigger_resyncs_on_kind_change() {
    let conn = open_in_memory_sync().expect("open_in_memory_sync");
    conn.prepare(INSERT_SQL)
        .expect("prepare insert")
        .execute(params![
            "rec_x",
            "tgt_x",
            "p/x",
            "kindold",
            "cls",
            "body text",
        ])
        .expect("insert");

    let n_old: i64 = conn
        .query_row(
            "SELECT count(*) FROM records_fts WHERE records_fts MATCH 'kindold'",
            [],
            |r| r.get(0),
        )
        .expect("query before update");
    assert_eq!(n_old, 1, "kindold should match before update");

    conn.execute(
        "UPDATE records SET kind = 'kindnew' WHERE record_id = 'rec_x'",
        [],
    )
    .expect("update");

    let n_new: i64 = conn
        .query_row(
            "SELECT count(*) FROM records_fts WHERE records_fts MATCH 'kindnew'",
            [],
            |r| r.get(0),
        )
        .expect("query new kind");
    let n_old_after: i64 = conn
        .query_row(
            "SELECT count(*) FROM records_fts WHERE records_fts MATCH 'kindold'",
            [],
            |r| r.get(0),
        )
        .expect("query old kind after");
    assert_eq!(n_new, 1, "new kind should match after update");
    assert_eq!(n_old_after, 0, "old kind should not match after update");
}
