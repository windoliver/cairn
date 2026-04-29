// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Upgrade-path coverage. Seeds a database with the schema as it
//! existed at intermediate migration levels (mid-0008 for pre-0009
//! NULL `record_json`, mid-0013 for pre-0014 single-snapshot purge
//! markers), then runs the remaining migrations and asserts the
//! migration repair semantics:
//!
//! - 0016 quarantines pre-0009 records and their dependent edges
//! - 0017 lifts pre-0014 single-snapshot markers into the
//!   per-version `version_snapshots` JSON array

use cairn_store_sqlite::schema::{MIGRATIONS, runner::apply_pending};
use rusqlite::{Connection, params};
use tempfile::tempdir;

/// Apply migrations 0001..=`through_id`. Used to land the schema in a
/// state that predates the migration we want to test the *repair* of.
fn apply_through(conn: &mut Connection, through_id: u32) {
    let subset: Vec<_> = MIGRATIONS
        .iter()
        .copied()
        .filter(|m| m.id <= through_id)
        .collect();
    apply_pending(conn, &subset).expect("partial migrate");
}

#[test]
fn migration_0016_quarantines_pre_0009_record_and_its_edges() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("upgrade.db");
    let mut conn = Connection::open(&path).expect("open");
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("pragmas");

    // Land at migration 0008: records exists (0002) and edges exists
    // (0003), but record_json column does NOT exist yet (0009 adds it).
    apply_through(&mut conn, 8);

    // Insert a pre-0009 row: record_json column does not exist yet,
    // so the row is naturally without record_json. Use the schema as
    // it stands at id=8.
    conn.execute(
        "INSERT INTO records ( \
             record_id, target_id, version, active, tombstoned, \
             created_at, created_by, body, provenance, actor_chain, \
             evidence, scope, taxonomy, confidence, salience \
         ) VALUES (?1, ?2, 1, 1, 0, '2026-04-22T14:02:11Z', \
             'usr:legacy', 'old body', '{}', '[]', '{}', '{}', \
             '{\"visibility\":\"public\"}', 0.5, 0.5)",
        params!["rec/legacy#1", "rec/legacy"],
    )
    .expect("insert pre-0009 record");

    // And an edge that references it.
    conn.execute(
        "INSERT INTO edges (from_id, to_id, kind, weight, metadata, created_at) \
         VALUES (?1, ?2, 'refines', 1.0, '{}', '2026-04-22T14:02:11Z')",
        params!["rec/legacy#1", "rec/legacy#1"],
    )
    .expect("insert edge");
    conn.execute(
        "INSERT INTO edge_versions (from_id, to_id, kind, change_kind, at) \
         VALUES (?1, ?2, 'refines', 'insert', '2026-04-22T14:02:11Z')",
        params!["rec/legacy#1", "rec/legacy#1"],
    )
    .expect("insert edge_version");

    // Now run remaining migrations including 0009 (adds record_json
    // NULL), 0016 (quarantine).
    apply_pending(&mut conn, &MIGRATIONS).expect("full migrate");

    // The record is gone from `records`.
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE record_id = ?1",
            params!["rec/legacy#1"],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(
        remaining, 0,
        "pre-0009 record must be removed from `records`"
    );

    // It survived in records_legacy_quarantine.
    let quarantined: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records_legacy_quarantine WHERE record_id = ?1",
            params!["rec/legacy#1"],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(quarantined, 1, "pre-0009 record must be in quarantine");

    // The dependent edges also moved to quarantine and are gone from
    // the live tables.
    let live_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE from_id = ?1",
            params!["rec/legacy#1"],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(
        live_edges, 0,
        "edges referencing quarantined record must be removed"
    );
    let q_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges_legacy_quarantine WHERE from_id = ?1",
            params!["rec/legacy#1"],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(q_edges, 1, "edges quarantine must hold the dependent edge");
    let q_edge_versions: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edge_versions_legacy_quarantine WHERE from_id = ?1",
            params!["rec/legacy#1"],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(
        q_edge_versions, 1,
        "edge_versions quarantine must hold the audit row"
    );
}

#[test]
fn migration_0017_lifts_pre_0014_single_snapshot_into_version_snapshots() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("upgrade.db");
    let mut conn = Connection::open(&path).expect("open");
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("pragmas");

    // Land at migration 0013: scope_snapshot/taxonomy_snapshot columns
    // exist (0010) but version_snapshots does not (0014).
    apply_through(&mut conn, 13);

    // Insert a pre-0014 purge marker with single-snapshot fields.
    conn.execute(
        "INSERT INTO record_purges (target_id, op_id, purged_at, purged_by, body_hash_salt, \
                                    scope_snapshot, taxonomy_snapshot) \
         VALUES (?1, ?2, '2026-04-22T15:00:00Z', 'usr:purger', 'salt', \
                 ?3, ?4)",
        params![
            "rec/legacy",
            "op:purge:1",
            r#"{"user":"usr:legacy"}"#,
            r#"{"visibility":"public"}"#,
        ],
    )
    .expect("insert pre-0014 marker");

    // Run remaining migrations (0014 adds the column, 0017 backfills).
    apply_pending(&mut conn, &MIGRATIONS).expect("full migrate");

    // version_snapshots is now populated as a one-element array.
    let snaps: String = conn
        .query_row(
            "SELECT version_snapshots FROM record_purges WHERE op_id = ?1",
            params!["op:purge:1"],
            |r| r.get(0),
        )
        .expect("read snapshots");
    let parsed: serde_json::Value = serde_json::from_str(&snaps).expect("valid json");
    let arr = parsed.as_array().expect("array");
    assert_eq!(
        arr.len(),
        1,
        "single-snapshot pair must lift into one-element array"
    );
    let entry = &arr[0];
    assert_eq!(
        entry
            .get("scope")
            .and_then(|s| s.get("user"))
            .and_then(|u| u.as_str()),
        Some("usr:legacy"),
        "scope must round-trip"
    );
    assert_eq!(
        entry
            .get("taxonomy")
            .and_then(|t| t.get("visibility"))
            .and_then(|v| v.as_str()),
        Some("public"),
        "taxonomy must round-trip"
    );
}

#[test]
fn migration_0017_skips_markers_already_carrying_version_snapshots() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("upgrade.db");
    let mut conn = Connection::open(&path).expect("open");
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("pragmas");

    // Land at migration 0014 so version_snapshots exists.
    apply_through(&mut conn, 14);

    // Insert a marker that already has version_snapshots (a record
    // purged after 0014 landed). Backfill must NOT touch it.
    let original = r#"[{"scope":{"user":"usr:already"},"taxonomy":{"visibility":"private"}}]"#;
    conn.execute(
        "INSERT INTO record_purges (target_id, op_id, purged_at, purged_by, body_hash_salt, \
                                    scope_snapshot, taxonomy_snapshot, version_snapshots) \
         VALUES (?1, ?2, '2026-04-23T10:00:00Z', 'usr:purger', 'salt', \
                 ?3, ?4, ?5)",
        params![
            "rec/already",
            "op:purge:2",
            r#"{"user":"usr:already"}"#,
            r#"{"visibility":"private"}"#,
            original,
        ],
    )
    .expect("insert marker");

    apply_pending(&mut conn, &MIGRATIONS).expect("full migrate");

    let snaps: String = conn
        .query_row(
            "SELECT version_snapshots FROM record_purges WHERE op_id = ?1",
            params!["op:purge:2"],
            |r| r.get(0),
        )
        .expect("read");
    assert_eq!(
        snaps, original,
        "0017 must not overwrite existing version_snapshots"
    );
}
