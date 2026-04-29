//! Integration tests for the async consent log materializer.

use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryVisibility, Rfc3339Timestamp,
};
use cairn_store_sqlite::consent::append;
use cairn_store_sqlite::open_in_memory;
use cairn_workflows::{ConsentLogMaterializer, MirrorError};
use tempfile::tempdir;

/// Build a fixture hash of the form `hash:<32 lowercase hex>` from a
/// numeric seed.
fn h(seed: u32) -> String {
    format!("hash:{seed:0>32x}")
}

fn forget_event(consent_id: &str, target_hash: &str) -> ConsentEvent {
    ConsentEvent {
        consent_id: consent_id.to_owned(),
        kind: ConsentKind::ForgetIntent,
        actor: Identity::parse("usr:tafeng").expect("id"),
        subject: target_hash.to_owned(),
        scope: "private".to_owned(),
        op_id: Some(format!("op-{consent_id}")),
        sensor_id: None,
        payload: ConsentPayload::IntentReceipt {
            target_id_hash: target_hash.to_owned(),
            scope_tier: MemoryVisibility::Private,
            reason_code: "user_command".to_owned(),
        },
        decided_at: Rfc3339Timestamp::parse("2026-04-28T12:00:00Z").expect("ts"),
        expires_at: None,
    }
}

#[test]
fn tick_appends_jsonl_and_advances_cursor() {
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open mirror");
    assert_eq!(mirror.cursor(), 0);

    append(&conn, &forget_event("c-1", &h(1))).expect("append 1");
    append(&conn, &forget_event("c-2", &h(2))).expect("append 2");

    let n = mirror.tick(&conn).expect("tick");
    assert_eq!(n, 2);
    assert!(mirror.cursor() > 0);

    let events = mirror.read_events().expect("events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].consent_id, "c-1");
    assert_eq!(events[1].consent_id, "c-2");
    let raw = mirror.read_lines().expect("raw");
    assert!(raw[0].contains("\"rowid\":"), "envelope must carry rowid");
}

#[test]
fn tick_is_idempotent_when_no_new_rows() {
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");

    append(&conn, &forget_event("c-1", &h(1))).expect("append");
    mirror.tick(&conn).expect("tick");
    let cursor_after_first = mirror.cursor();
    let lines_after_first = mirror.read_lines().expect("read");

    let n = mirror.tick(&conn).expect("re-tick");
    assert_eq!(n, 0);
    assert_eq!(mirror.cursor(), cursor_after_first);
    assert_eq!(mirror.read_lines().expect("read again"), lines_after_first);
}

#[test]
fn cursor_recovers_across_reopen() {
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    {
        let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
        append(&conn, &forget_event("c-1", &h(1))).expect("append");
        mirror.tick(&conn).expect("tick");
    }

    // New materializer instance — must recover the cursor from disk.
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("reopen");
    assert!(mirror.cursor() > 0);

    append(&conn, &forget_event("c-2", &h(2))).expect("append 2");
    let n = mirror.tick(&conn).expect("tick after reopen");
    assert_eq!(n, 1, "should mirror only the new row");

    let lines = mirror.read_lines().expect("lines");
    assert_eq!(lines.len(), 2);
}

#[test]
fn rebuild_from_db_replays_every_event() {
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");
    append(&conn, &forget_event("c-3", &h(3))).expect("a3");
    mirror.tick(&conn).expect("first tick");
    let original = mirror.read_lines().expect("read");

    // Corrupt the on-disk log + cursor.
    std::fs::write(mirror.log_path(), "garbage that cannot deserialize\n").expect("corrupt");
    std::fs::write(mirror.cursor_path(), "999999\n").expect("corrupt cursor");

    // Open must fail closed because the log is non-empty but has no
    // parseable envelope. The caller has to opt into a rebuild.
    let err = ConsentLogMaterializer::open(dir.path())
        .expect_err("open should fail closed on corrupt log");
    assert!(matches!(err, MirrorError::LogCorrupt));

    let mirror = ConsentLogMaterializer::rebuild_at(dir.path(), &conn).expect("rebuild_at");
    let rebuilt = mirror.read_lines().expect("lines");
    assert_eq!(rebuilt, original, "rebuild should be byte-identical");
}

#[test]
fn rebuild_works_when_log_was_deleted() {
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    mirror.tick(&conn).expect("tick");

    std::fs::remove_file(mirror.log_path()).expect("delete log");
    std::fs::remove_file(mirror.cursor_path()).expect("delete cursor");

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("reopen with no files");
    let n = mirror.rebuild_from_db(&conn).expect("rebuild");
    assert_eq!(n, 1);
    let lines = mirror.read_lines().expect("read");
    assert_eq!(lines.len(), 1);
}

#[test]
fn forget_receipt_log_is_body_free() {
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");

    let secret = "TOPSECRETBODY";
    let event = forget_event("c-leak", &h(0xdead_beef));
    append(&conn, &event).expect("append");
    mirror.tick(&conn).expect("tick");

    let raw = std::fs::read_to_string(mirror.log_path()).expect("read");
    assert!(
        !raw.contains(secret),
        "consent.log leaked forgotten body: {raw}"
    );
    assert!(!raw.contains("\"body\""));
    assert!(!raw.contains("\"text\""));
    assert!(!raw.contains("\"raw\""));
}

#[test]
fn tick_fails_closed_when_log_corrupted_after_open() {
    // Round 5 hardening: a long-lived materializer must not silently
    // continue when the on-disk log is replaced with garbage between
    // ticks. `tick()` re-reads the cursor under the lock and fails
    // closed if recovery finds nothing parseable in a non-empty log.
    let conn = open_in_memory().expect("open");
    let dir = tempdir().expect("tempdir");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    mirror.tick(&conn).expect("first tick");

    // Corrupt the log behind the materializer's back.
    std::fs::write(dir.path().join("consent.log"), "garbage line\n").expect("corrupt");

    append(&conn, &forget_event("c-2", &h(2))).expect("a2");
    let err = mirror.tick(&conn).expect_err("tick must fail closed");
    assert!(matches!(err, MirrorError::LogCorrupt));
}

#[test]
fn tick_resets_cursor_when_log_truncated_to_empty() {
    // Truncation-to-empty is recoverable in-place: there are no
    // unparseable bytes to honor, so the materializer resets its
    // cursor and replays from rowid 0 on the next read.
    let conn = open_in_memory().expect("open");
    let dir = tempdir().expect("tempdir");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    mirror.tick(&conn).expect("first tick");

    // Operator truncates the log to zero bytes.
    std::fs::write(dir.path().join("consent.log"), "").expect("truncate");

    let n = mirror.tick(&conn).expect("re-tick after truncate");
    assert_eq!(n, 1, "must re-mirror the existing row");
    assert_eq!(mirror.read_lines().expect("read").len(), 1);
}

#[test]
fn cursor_recovery_uses_log_when_sidecar_lies() {
    // The log is the authoritative cursor source. If the sidecar
    // disagrees with the log (e.g., crash between fsync and rename), the
    // materializer must trust the log and skip the rows it already wrote.
    let conn = open_in_memory().expect("open");
    let dir = tempdir().expect("tempdir");

    {
        let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
        append(&conn, &forget_event("c-1", &h(1))).expect("a1");
        append(&conn, &forget_event("c-2", &h(2))).expect("a2");
        mirror.tick(&conn).expect("tick");
    }

    // Tamper with the sidecar: claim a future rowid that the log can't
    // back up. The materializer must distrust the sidecar and recover
    // from the log itself.
    std::fs::write(dir.path().join("consent.cursor"), "999999\n").expect("tamper");

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("reopen");
    append(&conn, &forget_event("c-3", &h(3))).expect("a3");
    let n = mirror.tick(&conn).expect("tick");
    assert_eq!(n, 1, "must add only the new row, not duplicate older ones");

    let events = mirror.read_events().expect("events");
    assert_eq!(events.len(), 3);
    assert_eq!(events[2].consent_id, "c-3");
}

#[test]
fn cursor_recovery_skips_torn_last_line() {
    // Simulate a crash mid-line: the last line of the log is a partial
    // envelope. Recovery must skip it and base the cursor on the last
    // well-formed envelope.
    let conn = open_in_memory().expect("open");
    let dir = tempdir().expect("tempdir");

    {
        let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
        append(&conn, &forget_event("c-1", &h(1))).expect("a1");
        append(&conn, &forget_event("c-2", &h(2))).expect("a2");
        mirror.tick(&conn).expect("tick");
    }

    // Append a torn line.
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("consent.log"))
            .expect("open log");
        write!(f, "{{\"rowid\":99,\"event\":{{\"partial").expect("torn write");
    }

    let mirror = ConsentLogMaterializer::open(dir.path()).expect("reopen");
    // Cursor must reflect the last well-formed envelope, not the torn
    // line's truncated rowid.
    assert!(mirror.cursor() > 0);
    assert!(mirror.cursor() < 99, "must not adopt rowid from torn line");
}

#[test]
fn cursor_survives_missing_sidecar() {
    // If the sidecar is deleted but the log is intact, recovery must
    // still place the cursor at the log's last envelope.
    let conn = open_in_memory().expect("open");
    let dir = tempdir().expect("tempdir");

    let cursor_when_full;
    {
        let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
        append(&conn, &forget_event("c-1", &h(1))).expect("a1");
        mirror.tick(&conn).expect("tick");
        cursor_when_full = mirror.cursor();
    }

    std::fs::remove_file(dir.path().join("consent.cursor")).expect("rm sidecar");

    let mirror = ConsentLogMaterializer::open(dir.path()).expect("reopen");
    assert_eq!(mirror.cursor(), cursor_when_full);
}

#[test]
fn rebuild_is_authoritative_over_db_only() {
    // The database remains the source of truth: rebuilding from a vault
    // with a deleted log produces a log identical to the one constructed
    // by tick-from-zero on a fresh vault.
    let dir_a = tempdir().expect("a");
    let dir_b = tempdir().expect("b");
    let conn = open_in_memory().expect("open");

    let mut mirror_a = ConsentLogMaterializer::open(dir_a.path()).expect("a");
    let mut mirror_b = ConsentLogMaterializer::open(dir_b.path()).expect("b");

    append(&conn, &forget_event("c-1", &h(1))).expect("1");
    append(&conn, &forget_event("c-2", &h(2))).expect("2");

    mirror_a.tick(&conn).expect("tick a");
    mirror_b.rebuild_from_db(&conn).expect("rebuild b");

    let a = mirror_a.read_lines().expect("a lines");
    let b = mirror_b.read_lines().expect("b lines");
    assert_eq!(a, b);
}
