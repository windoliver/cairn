//! Integration tests for the async consent log materializer.

use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryVisibility, Rfc3339Timestamp,
};
use cairn_store_sqlite::consent::append;
use cairn_store_sqlite::open_in_memory;
use cairn_workflows::ConsentLogMaterializer;
use tempfile::tempdir;

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

    append(&conn, &forget_event("c-1", "hash:1")).expect("append 1");
    append(&conn, &forget_event("c-2", "hash:2")).expect("append 2");

    let n = mirror.tick(&conn).expect("tick");
    assert_eq!(n, 2);
    assert!(mirror.cursor() > 0);

    let lines = mirror.read_lines().expect("read");
    assert_eq!(lines.len(), 2);
    let event_one: ConsentEvent = serde_json::from_str(&lines[0]).expect("decode 1");
    assert_eq!(event_one.consent_id, "c-1");
}

#[test]
fn tick_is_idempotent_when_no_new_rows() {
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");

    append(&conn, &forget_event("c-1", "hash:1")).expect("append");
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
        append(&conn, &forget_event("c-1", "hash:1")).expect("append");
        mirror.tick(&conn).expect("tick");
    }

    // New materializer instance — must recover the cursor from disk.
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("reopen");
    assert!(mirror.cursor() > 0);

    append(&conn, &forget_event("c-2", "hash:2")).expect("append 2");
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

    append(&conn, &forget_event("c-1", "hash:1")).expect("a1");
    append(&conn, &forget_event("c-2", "hash:2")).expect("a2");
    append(&conn, &forget_event("c-3", "hash:3")).expect("a3");
    mirror.tick(&conn).expect("first tick");
    let original = mirror.read_lines().expect("read");

    // Corrupt the on-disk log + cursor, then rebuild.
    std::fs::write(mirror.log_path(), "garbage that cannot deserialize\n").expect("corrupt");
    std::fs::write(mirror.cursor_path(), "999999\n").expect("corrupt cursor");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("reopen corrupt");

    let n = mirror.rebuild_from_db(&conn).expect("rebuild");
    assert_eq!(n, 3);
    let rebuilt = mirror.read_lines().expect("lines");
    assert_eq!(rebuilt, original, "rebuild should be byte-identical");
}

#[test]
fn rebuild_works_when_log_was_deleted() {
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
    append(&conn, &forget_event("c-1", "hash:1")).expect("a1");
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
    let event = forget_event("c-leak", "hash:salted");
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
fn rebuild_is_authoritative_over_db_only() {
    // The database remains the source of truth: rebuilding from a vault
    // with a deleted log produces a log identical to the one constructed
    // by tick-from-zero on a fresh vault.
    let dir_a = tempdir().expect("a");
    let dir_b = tempdir().expect("b");
    let conn = open_in_memory().expect("open");

    let mut mirror_a = ConsentLogMaterializer::open(dir_a.path()).expect("a");
    let mut mirror_b = ConsentLogMaterializer::open(dir_b.path()).expect("b");

    append(&conn, &forget_event("c-1", "hash:1")).expect("1");
    append(&conn, &forget_event("c-2", "hash:2")).expect("2");

    mirror_a.tick(&conn).expect("tick a");
    mirror_b.rebuild_from_db(&conn).expect("rebuild b");

    let a = mirror_a.read_lines().expect("a lines");
    let b = mirror_b.read_lines().expect("b lines");
    assert_eq!(a, b);
}
