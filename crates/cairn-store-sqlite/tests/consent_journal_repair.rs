//! Integration tests for consent journal repair helpers.

use cairn_store_sqlite::migrations::migrations;
use cairn_store_sqlite::repair::consent_journal::{BlockerCode, delete_blocker, list_blockers};

fn open_at_version(version: usize) -> rusqlite::Connection {
    let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory");
    migrations()
        .to_version(&mut conn, version)
        .expect("apply migrations to version");
    conn
}

#[test]
fn list_blockers_finds_legacy_non_positive_rowid() {
    let conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (rowid, consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES (0, 'rowid-zero', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("seed blocker");

    let blockers = list_blockers(&conn).expect("list blockers");
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0].rowid, 0);
    assert!(
        blockers[0]
            .blocker_codes
            .contains(&BlockerCode::NonPositiveRowid)
    );
}

#[test]
fn list_blockers_finds_unrenderable_legacy_decided_at() {
    let conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('bad-time', 'sub', 'private', 'GRANT', 'hmn:t', 253402300800000000)",
        [],
    )
    .expect("seed blocker");

    let blockers = list_blockers(&conn).expect("list blockers");
    assert_eq!(blockers.len(), 1);
    assert!(
        blockers[0]
            .blocker_codes
            .contains(&BlockerCode::UnrenderableDecidedAt)
    );
}

#[test]
fn list_blockers_finds_kind_null_event_field_drift() {
    let conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, actor) \
         VALUES ('drift', 'sub', 'private', 'GRANT', 'hmn:t', 0, 'hmn:real')",
        [],
    )
    .expect("seed blocker");

    let blockers = list_blockers(&conn).expect("list blockers");
    assert_eq!(blockers.len(), 1);
    assert!(
        blockers[0]
            .blocker_codes
            .contains(&BlockerCode::KindNullEventFieldDrift)
    );
}

#[test]
fn delete_blocker_removes_row_audits_and_allows_migration() {
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (rowid, consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES (0, 'delete-me', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("seed blocker");

    let receipt = delete_blocker(&mut conn, 0, "fixture repair", "hmn:test").expect("delete");

    assert_eq!(receipt.target_rowid, 0);
    assert_eq!(receipt.operator, "hmn:test");
    assert_eq!(receipt.reason, "fixture repair");
    assert!(
        receipt
            .blocker_codes
            .contains(&BlockerCode::NonPositiveRowid)
    );

    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE consent_id = 'delete-me'",
            [],
            |row| row.get(0),
        )
        .expect("count consent_journal rows");
    assert_eq!(remaining, 0);

    let audit_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal_repair_audit \
             WHERE repair_id = ?1 AND target_rowid = 0",
            [&receipt.repair_id],
            |row| row.get(0),
        )
        .expect("count audit rows");
    assert_eq!(audit_count, 1);

    migrations()
        .to_version(&mut conn, 21)
        .expect("repair should unblock migrations");
    let head: i64 = conn
        .query_row(
            "SELECT MAX(migration_id) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .expect("query head");
    assert_eq!(head, 21);
    let reset_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_mirror_resets WHERE migration_id = 21",
            [],
            |row| row.get(0),
        )
        .expect("count mirror resets");
    assert_eq!(reset_count, 1);
}

#[test]
fn delete_blocker_refuses_non_blocker() {
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('not-a-blocker', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("seed non-blocker");
    let rowid: i64 = conn
        .query_row(
            "SELECT rowid FROM consent_journal WHERE consent_id = 'not-a-blocker'",
            [],
            |row| row.get(0),
        )
        .expect("fetch rowid");

    let err =
        delete_blocker(&mut conn, rowid, "fixture repair", "hmn:test").expect_err("must refuse");

    assert!(err.to_string().contains("not repair-eligible"));
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE consent_id = 'not-a-blocker'",
            [],
            |row| row.get(0),
        )
        .expect("count remaining rows");
    assert_eq!(remaining, 1);
}
