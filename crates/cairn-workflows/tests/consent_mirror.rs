//! Integration tests for the async consent log materializer.

use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryVisibility, Rfc3339Timestamp,
};
use cairn_store_sqlite::consent::append;
use cairn_store_sqlite::open_in_memory_sync as open_in_memory;
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
        actor: Identity::parse("hmn:tafeng").expect("id"),
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
fn tick_fails_closed_when_log_regresses_to_valid_prefix() {
    // Round 6 hardening: if the log on disk is truncated/restored to a
    // valid prefix that ends BEFORE the in-memory cursor, honoring the
    // disk value would skip rows between the new tail and the cursor;
    // honoring the in-memory value would append past the gap. Either
    // way is a no-gaps violation, so `tick` must fail closed.
    let conn = open_in_memory().expect("open");
    let dir = tempdir().expect("tempdir");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");
    append(&conn, &forget_event("c-3", &h(3))).expect("a3");
    mirror.tick(&conn).expect("tick");
    let lines = mirror.read_lines().expect("read");

    // Operator (or backup restore) overwrites the log with only the
    // first envelope — the disk cursor is now lower than memory.
    let first_line = format!("{}\n", lines[0]);
    std::fs::write(dir.path().join("consent.log"), first_line).expect("regress");

    let err = mirror
        .tick(&conn)
        .expect_err("tick must fail closed on disk regression");
    assert!(matches!(err, MirrorError::LogCorrupt));
}

#[test]
fn tick_fails_closed_when_log_cursor_exceeds_db() {
    // Round 7 hardening: a recovered envelope rowid greater than the
    // current journal high-water mark cannot be trusted (log restored
    // from another vault, or tampered). Honoring it would cause the
    // next `read_since_rowid` call to skip every real DB row up to the
    // bogus value. The materializer must fail closed.
    let conn = open_in_memory().expect("open");
    let dir = tempdir().expect("tempdir");
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    mirror.tick(&conn).expect("tick");

    // Replace the log with a single envelope claiming rowid 999. The
    // DB only holds rowid 1, so 999 cannot be a peer-advanced cursor.
    let raw = mirror.read_lines().expect("read");
    let real = serde_json::from_str::<serde_json::Value>(&raw[0]).expect("json");
    let real_event = real.get("event").cloned().expect("event field");
    let bogus = serde_json::json!({"rowid": 999, "event": real_event});
    let line = format!("{bogus}\n");
    std::fs::write(dir.path().join("consent.log"), line).expect("tamper");

    let err = mirror
        .tick(&conn)
        .expect_err("tick must fail closed on rowid > db_high");
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
fn tick_rebuilds_when_migration_0021_reset_marker_unconsumed() {
    // Round-5 review hardening: migration 0021 promotes legacy
    // `kind IS NULL` rows to event-shape rows preserving their original
    // rowid. Existing vaults' .cairn/consent.cursor sidecar may already
    // point ABOVE those legacy rowids (the mirror tailed only event-kind
    // rows pre-0021), so a plain tick() would skip them. Migration 0021
    // inserts a row into `consent_mirror_resets` to instruct the mirror
    // to replay from rowid 0 once after upgrade.
    //
    // This test simulates the post-upgrade state: events already exist
    // in the journal AND a cursor that's already past them, AND a
    // pending reset marker. The next tick must rebuild the log from
    // rowid 0 (re-mirroring everything), then mark the reset consumed
    // so subsequent ticks behave normally.
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    // Pre-existing journal rows.
    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");

    // Pre-mark the migration-0021 reset row as consumed in this
    // mirror's per-mirror sidecar (Phase-B finding 2 moved consumption
    // tracking out of the DB), so the first tick processes normally.
    let resets_consumed = dir.path().join("consent.mirror_resets_consumed");
    std::fs::write(&resets_consumed, "21\n").expect("seed sidecar");

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open mirror");
    let n = mirror.tick(&conn).expect("first tick");
    assert_eq!(n, 2);
    let cursor_before_reset = mirror.cursor();
    assert!(cursor_before_reset > 0);

    // Now wipe the log and the sidecar to re-arm the reset for THIS
    // mirror. The DB row is unchanged — it's still the source of truth
    // for "a reset was needed." The reset path must rebuild from the DB
    // — even though the in-memory cursor is already past every row —
    // and re-mirror everything.
    std::fs::write(mirror.log_path(), "").expect("truncate");
    std::fs::remove_file(&resets_consumed).expect("re-arm sidecar");

    let n = mirror.tick(&conn).expect("reset tick");
    assert_eq!(n, 2, "reset must replay every row from rowid 0");

    let lines = mirror.read_lines().expect("lines");
    assert_eq!(lines.len(), 2);

    // The reset row must now be in this mirror's sidecar; another tick
    // is a no-op.
    let sidecar = std::fs::read_to_string(&resets_consumed).expect("sidecar");
    assert!(
        sidecar.lines().any(|l| l.trim() == "21"),
        "sidecar must record migration_id 21 as consumed: {sidecar:?}"
    );

    let n = mirror.tick(&conn).expect("idempotent tick");
    assert_eq!(n, 0, "no further work once reset is consumed");
}

#[test]
fn tick_surfaces_log_corrupt_when_reset_pending_and_log_damaged() {
    // Phase-B finding 1: if the consent.log is corrupt and a 0021-style
    // reset is pending, tick() must still surface MirrorError::LogCorrupt
    // — the reset auto-replay is reserved for a known-good log so the
    // operator sees corruption at exactly the moment it matters. The
    // damaged log must be left untouched on disk (no silent overwrite).
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    // Seed a healthy mirror first.
    {
        let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
        // Pre-consume the migration-0021 reset so the initial tick
        // processes normally.
        std::fs::write(dir.path().join("consent.mirror_resets_consumed"), "21\n")
            .expect("seed sidecar");
        append(&conn, &forget_event("c-1", &h(1))).expect("a1");
        append(&conn, &forget_event("c-2", &h(2))).expect("a2");
        mirror.tick(&conn).expect("seed tick");
    }

    // Now reopen, corrupt the on-disk log behind the materializer's
    // back, AND re-arm the reset marker by clearing the sidecar.
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("reopen");
    let corrupt_bytes = b"not even close to a json envelope, no newline either";
    std::fs::write(dir.path().join("consent.log"), &corrupt_bytes[..]).expect("corrupt");
    std::fs::remove_file(dir.path().join("consent.mirror_resets_consumed"))
        .expect("re-arm sidecar");

    let err = mirror
        .tick(&conn)
        .expect_err("tick must fail closed on corrupt log even with reset pending");
    assert!(
        matches!(err, MirrorError::LogCorrupt),
        "expected LogCorrupt, got {err:?}"
    );

    // The damaged log must be unchanged — no silent rebuild.
    let on_disk = std::fs::read(dir.path().join("consent.log")).expect("read log");
    assert_eq!(
        on_disk,
        &corrupt_bytes[..],
        "tick must not overwrite a corrupt log when it fails closed"
    );
}

#[test]
fn tick_replays_reset_when_log_intact() {
    // Phase-B finding 1 sibling test: the corruption-first reorder
    // must not regress the happy path. With a healthy log AND a pending
    // reset marker, tick() still rebuilds from rowid 0.
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");

    // Note: no sidecar yet → the migration-0021 marker is pending for
    // this mirror.
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
    let n = mirror.tick(&conn).expect("tick");
    assert_eq!(n, 2, "reset path must replay every row from rowid 0");

    let lines = mirror.read_lines().expect("lines");
    assert_eq!(lines.len(), 2);

    let sidecar = std::fs::read_to_string(dir.path().join("consent.mirror_resets_consumed"))
        .expect("sidecar written");
    assert!(sidecar.lines().any(|l| l.trim() == "21"));

    let n = mirror.tick(&conn).expect("idempotent");
    assert_eq!(n, 0);
}

#[test]
fn reset_marker_consumed_per_mirror_not_per_db() {
    // Phase-B finding 2: two mirrors at different vault_dirs sharing
    // one DB must each replay the migration-0021 reset independently.
    // Consuming on mirror A must not silence the marker for mirror B.
    let conn = open_in_memory().expect("open store");
    let dir_a = tempdir().expect("tempdir a");
    let dir_b = tempdir().expect("tempdir b");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");

    // Mirror A: pending reset → replays everything, then writes its
    // sidecar.
    let mut mirror_a = ConsentLogMaterializer::open(dir_a.path()).expect("open a");
    let n_a = mirror_a.tick(&conn).expect("tick a");
    assert_eq!(n_a, 2);
    assert!(
        std::fs::read_to_string(dir_a.path().join("consent.mirror_resets_consumed"))
            .expect("a sidecar")
            .lines()
            .any(|l| l.trim() == "21"),
        "mirror A must record consumption locally"
    );

    // Mirror B has its own (empty) sidecar, so the marker is still
    // pending for B even though A consumed it. B must replay every row
    // independently.
    let mut mirror_b = ConsentLogMaterializer::open(dir_b.path()).expect("open b");
    let n_b = mirror_b.tick(&conn).expect("tick b");
    assert_eq!(
        n_b, 2,
        "mirror B must still see the reset marker after A consumed it"
    );
    assert_eq!(mirror_b.read_lines().expect("b lines").len(), 2);
    assert!(
        std::fs::read_to_string(dir_b.path().join("consent.mirror_resets_consumed"))
            .expect("b sidecar")
            .lines()
            .any(|l| l.trim() == "21"),
        "mirror B must also record consumption locally"
    );
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
