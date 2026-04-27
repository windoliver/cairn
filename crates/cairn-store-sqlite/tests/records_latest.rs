//! Round-trip semantics for `records_latest` view + `updates`-edge supersession.

use cairn_store_sqlite::open_in_memory;
use rusqlite::{Connection, params};

fn insert_record(conn: &Connection, id: &str, target: &str, body: &str, tombstoned: i64) {
    insert_with(conn, id, target, body, 1, tombstoned);
}

fn insert_with(
    conn: &Connection,
    id: &str,
    target: &str,
    body: &str,
    active: i64,
    tombstoned: i64,
) {
    insert_v(conn, id, target, 1, body, active, tombstoned);
}

fn insert_v(
    conn: &Connection,
    id: &str,
    target: &str,
    version: i64,
    body: &str,
    active: i64,
    tombstoned: i64,
) {
    conn.execute(
        "INSERT INTO records \
          (record_id, target_id, version, path, kind, class, visibility, scope, \
           actor_chain, body, body_hash, created_at, updated_at, active, tombstoned, is_static) \
          VALUES (?,?,?,'p','note','n','public','s','[]',?,'h',0,0,?,?,0)",
        params![id, target, version, body, active, tombstoned],
    )
    .expect("insert record");
}

fn latest_ids(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT record_id FROM records_latest ORDER BY record_id")
        .expect("prep");
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).expect("rows");
    rows.collect::<Result<Vec<_>, _>>().expect("collect")
}

#[test]
fn supersession_hides_old_record() {
    let conn = open_in_memory().expect("open");
    insert_record(&conn, "r1", "t1", "old", 0);
    insert_record(&conn, "r2", "t2", "new", 0);
    assert_eq!(latest_ids(&conn), vec!["r1", "r2"]);

    conn.execute(
        "INSERT INTO edges (src, dst, kind) VALUES ('r2', 'r1', 'updates')",
        [],
    )
    .expect("supersede r1 with r2");

    // r1 is dst of an `updates` edge -> hidden by records_latest.
    assert_eq!(latest_ids(&conn), vec!["r2"]);
}

#[test]
fn updates_edge_rejected_for_same_target() {
    let conn = open_in_memory().expect("open");
    // Two non-tombstoned records sharing target_id `t1`. Only one can be
    // active per target (records_active_target_idx), so r1 is historical.
    insert_v(&conn, "r1", "t1", 1, "v1", 0, 0);
    insert_v(&conn, "r2", "t1", 2, "v2", 1, 0);
    let err = conn
        .execute(
            "INSERT INTO edges (src, dst, kind) VALUES ('r2', 'r1', 'updates')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("distinct target_ids"),
        "same-target updates edge must be rejected: {err}"
    );
}

#[test]
fn updates_edge_rejected_for_tombstoned_endpoint() {
    let conn = open_in_memory().expect("open");
    insert_record(&conn, "r1", "t1", "v1", 1); // tombstoned
    insert_record(&conn, "r2", "t2", "v2", 0);
    let err = conn
        .execute(
            "INSERT INTO edges (src, dst, kind) VALUES ('r2', 'r1', 'updates')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("non-tombstoned"),
        "tombstoned endpoint must be rejected: {err}"
    );
}

#[test]
fn updates_edge_immutable_after_insert() {
    let conn = open_in_memory().expect("open");
    insert_record(&conn, "r1", "t1", "v1", 0);
    insert_record(&conn, "r2", "t2", "v2", 0);
    insert_record(&conn, "r3", "t3", "v3", 0);
    conn.execute(
        "INSERT INTO edges (src, dst, kind) VALUES ('r2', 'r1', 'updates')",
        [],
    )
    .expect("supersede");
    let err = conn
        .execute(
            "UPDATE edges SET dst = 'r3' WHERE src = 'r2' AND kind = 'updates'",
            [],
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("updates edges are immutable") || msg.contains("updates edge identity"),
        "updates edge dst must be immutable: {err}"
    );
}

#[test]
fn fts_au_reflects_body_update() {
    let conn = open_in_memory().expect("open");
    insert_record(&conn, "r1", "t1", "alpha", 0);
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records_fts WHERE records_fts MATCH ?",
            params!["alpha"],
            |r| r.get(0),
        )
        .expect("match alpha");
    assert_eq!(n, 1);

    conn.execute(
        "UPDATE records SET body = 'beta', body_hash = 'h2' WHERE record_id = 'r1'",
        [],
    )
    .expect("update body");

    let n_alpha: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records_fts WHERE records_fts MATCH ?",
            params!["alpha"],
            |r| r.get(0),
        )
        .expect("match alpha post-update");
    let n_beta: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records_fts WHERE records_fts MATCH ?",
            params!["beta"],
            |r| r.get(0),
        )
        .expect("match beta post-update");
    assert_eq!(n_alpha, 0, "old body should be removed from FTS");
    assert_eq!(n_beta, 1, "new body should be searchable");
}
