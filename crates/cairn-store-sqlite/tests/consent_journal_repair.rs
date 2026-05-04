//! Integration tests for consent journal repair helpers.

use cairn_store_sqlite::migrations::migrations;
use cairn_store_sqlite::repair::consent_journal::{BlockerCode, list_blockers};

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
