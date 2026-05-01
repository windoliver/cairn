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

/// Read the `db_nonce` the migration minted for the
/// `consent_mirror_resets` row at `migration_id`. Used by tests that
/// seed the per-mirror sidecar in the bound
/// `migration_id:db_nonce:watermark` format (round-12 finding:
/// consumption is keyed to the DB schema instance via a per-DB UUID,
/// not by the second-resolution `applied_at`).
fn db_nonce_for(conn: &rusqlite::Connection, migration_id: i64) -> String {
    conn.query_row(
        "SELECT db_nonce FROM consent_mirror_resets WHERE migration_id = ?1",
        rusqlite::params![migration_id],
        |r| r.get::<_, String>(0),
    )
    .expect("read db_nonce")
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
    // tracking out of the DB; round-12 finding binds the entry to the
    // DB instance via `db_nonce` (a per-DB UUID minted at migration
    // time, supersedes the second-resolution `applied_at`); round-4
    // finding adds a `watermark_rowid` so a cursor below that rowid
    // forces replay). Seed watermark=0 so the initial cursor (also 0)
    // satisfies `cursor >= watermark` and the first tick processes
    // normally.
    let resets_consumed = dir.path().join("consent.mirror_resets_consumed");
    let nonce = db_nonce_for(&conn, 21);
    // Seed the round-13 four-field bound format with watermark=0,
    // line_count=0 so the initial cursor (also 0) and empty log
    // (also 0 lines) satisfy `cursor >= watermark` AND
    // `live_line_count >= line_count` and the first tick processes
    // normally (no reset replay).
    std::fs::write(&resets_consumed, format!("21:{nonce}:0:0\n")).expect("seed sidecar");

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
    // is a no-op. Watermark = high_water of the replay = 2 (rowids 1,2);
    // line_count = 2 (one envelope per replayed rowid, round-13 finding).
    let sidecar = std::fs::read_to_string(&resets_consumed).expect("sidecar");
    let expected = format!("21:{nonce}:2:2");
    assert!(
        sidecar.lines().any(|l| l.trim() == expected),
        "sidecar must record (21,{nonce},watermark=2,line_count=2) as consumed: {sidecar:?}"
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
        let nonce = db_nonce_for(&conn, 21);
        std::fs::write(
            dir.path().join("consent.mirror_resets_consumed"),
            format!("21:{nonce}:0:0\n"),
        )
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
    let nonce = db_nonce_for(&conn, 21);
    let expected = format!("21:{nonce}:2:2");
    assert!(sidecar.lines().any(|l| l.trim() == expected));

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
    let nonce = db_nonce_for(&conn, 21);
    let expected = format!("21:{nonce}:2:2");
    assert!(
        std::fs::read_to_string(dir_a.path().join("consent.mirror_resets_consumed"))
            .expect("a sidecar")
            .lines()
            .any(|l| l.trim() == expected),
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
            .any(|l| l.trim() == expected),
        "mirror B must also record consumption locally"
    );
}

#[test]
fn reset_marker_replays_when_cursor_regresses_below_watermark() {
    // Round-4 medium finding: the consumption sidecar is bound to the
    // log state via a `watermark_rowid`. If the on-disk log is rolled
    // back below that watermark (e.g., vault restored from a backup
    // without the DB), the sidecar still claims "consumed" but the
    // audit no longer covers the migrated rows. The next tick must
    // replay.
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");

    // First tick replays the reset and writes sidecar `21:T:2:2`.
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
    let n = mirror.tick(&conn).expect("first tick");
    assert_eq!(n, 2);

    let resets_consumed = dir.path().join("consent.mirror_resets_consumed");
    let nonce = db_nonce_for(&conn, 21);
    let after_first = std::fs::read_to_string(&resets_consumed).expect("after first");
    assert!(
        after_first
            .lines()
            .any(|l| l.trim() == format!("21:{nonce}:2:2")),
        "watermark+line_count must be (2,2) after replay: {after_first:?}"
    );

    // Roll the log back: keep only the first envelope. The sidecar
    // still claims watermark=2 but the on-disk audit only goes to
    // rowid 1.
    let lines = mirror.read_lines().expect("read lines");
    let truncated = format!("{}\n", lines[0]);
    std::fs::write(dir.path().join("consent.log"), &truncated).expect("rollback log");
    // Drop the materializer so the next tick reopens against the
    // rolled-back log and recovers cursor=1.
    drop(mirror);

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("reopen");
    assert_eq!(mirror.cursor(), 1, "recovered cursor must reflect rollback");

    let n = mirror.tick(&conn).expect("re-tick after rollback");
    assert_eq!(
        n, 2,
        "cursor below watermark must force replay even though sidecar claims consumed"
    );
    assert_eq!(mirror.read_lines().expect("after replay").len(), 2);
}

#[test]
fn reset_marker_consumption_survives_normal_cursor_advance() {
    // Round-4 medium finding companion: when the cursor advances
    // *past* the watermark via normal appends, the sidecar entry must
    // continue to suppress replays. Watermark forces replay on
    // regression, not on growth.
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
    let n = mirror.tick(&conn).expect("first tick");
    assert_eq!(n, 2);
    let watermark_after = mirror.cursor();
    assert_eq!(watermark_after, 2);

    // Normal append advances cursor past the watermark.
    append(&conn, &forget_event("c-3", &h(3))).expect("a3");
    let n = mirror.tick(&conn).expect("normal tick");
    assert_eq!(n, 1, "normal append, no replay");
    assert!(mirror.cursor() > watermark_after);

    // Idempotent — sidecar still suppresses replay.
    let n = mirror.tick(&conn).expect("idempotent tick");
    assert_eq!(n, 0);
    assert_eq!(mirror.read_lines().expect("lines").len(), 3);
}

#[test]
fn sidecar_with_two_field_legacy_format_forces_replay() {
    // Round-4 forward-compat: lines that don't carry a watermark
    // (legacy bare `migration_id` and round-3 `migration_id:applied_at`
    // formats) cannot prove the log still reflects the replay. They
    // must be treated as unknown state and skipped, forcing the next
    // tick to replay.
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");

    // Seed the round-3 two-field format (no watermark).
    let resets_consumed = dir.path().join("consent.mirror_resets_consumed");
    let nonce = db_nonce_for(&conn, 21);
    // The middle field doesn't matter here — any string would work for
    // the legacy two-field shape — but using the live nonce keeps the
    // negative assertion below tight.
    std::fs::write(&resets_consumed, format!("21:{nonce}\n")).expect("seed two-field sidecar");

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
    let n = mirror.tick(&conn).expect("tick");
    assert_eq!(
        n, 2,
        "two-field sidecar with no watermark must not suppress replay"
    );
    assert_eq!(mirror.read_lines().expect("lines").len(), 2);

    // Sidecar is rewritten in the new bound format.
    let after = std::fs::read_to_string(&resets_consumed).expect("after");
    let expected = format!("21:{nonce}:2:2");
    assert!(
        after.lines().any(|l| l.trim() == expected),
        "post-replay sidecar must include watermark and line_count: {after:?}"
    );
    assert!(
        !after.lines().any(|l| l.trim() == format!("21:{nonce}")),
        "two-field legacy line must not survive: {after:?}"
    );
}

#[test]
fn sidecar_with_applied_at_format_forces_replay() {
    // Round-12 finding: round-4 sidecar entries used a decimal
    // `applied_at` integer in the middle field instead of a hex
    // `db_nonce`. Such lines cannot match the (migration_id, db_nonce)
    // tuple the DB now exposes, but more importantly we *defensively*
    // skip any three-field line whose middle field decimal-parses as
    // an i64 — round-4 millis-since-epoch values are always decimal,
    // hex nonces never are. The next tick forces a replay.
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");

    // Seed the round-4 three-field `applied_at`-bound format. Use a
    // value high enough that it won't collide with any plausible nonce.
    let applied_at: i64 = 1_761_600_000_000;
    let resets_consumed = dir.path().join("consent.mirror_resets_consumed");
    std::fs::write(&resets_consumed, format!("21:{applied_at}:2\n"))
        .expect("seed round-4 applied_at sidecar");

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open mirror");
    let n = mirror.tick(&conn).expect("tick");
    assert_eq!(
        n, 2,
        "round-4 applied_at-bound sidecar must not suppress replay under round-12 nonce binding"
    );
    assert_eq!(mirror.read_lines().expect("lines").len(), 2);

    // Sidecar is rewritten in the round-12 nonce-bound format.
    let after = std::fs::read_to_string(&resets_consumed).expect("after");
    let nonce = db_nonce_for(&conn, 21);
    let expected = format!("21:{nonce}:2:2");
    assert!(
        after.lines().any(|l| l.trim() == expected),
        "post-replay sidecar must be in nonce-bound form with line_count: {after:?}"
    );
    assert!(
        !after
            .lines()
            .any(|l| l.trim() == format!("21:{applied_at}:2")),
        "round-4 applied_at-bound line must not survive: {after:?}"
    );
}

#[test]
fn reset_marker_replays_after_db_clone_with_same_applied_at() {
    // Round-12 finding: `applied_at` is second-resolution and DB copies
    // preserve it. Two distinct DBs migrated in the same wall-clock
    // second (or one DB restored from a backup that preserved the
    // original applied_at) collide on (migration_id, applied_at). The
    // `db_nonce` minted via `randomblob(16)` is fresh on every
    // migration apply, so two distinct DBs always hold distinct nonces
    // even under same-second collisions — and a sidecar bound to one
    // DB's nonce cannot suppress replay against another DB.
    let conn_a = open_in_memory().expect("open a");
    let conn_b = open_in_memory().expect("open b");

    append(&conn_a, &forget_event("c-a1", &h(1))).expect("a1");
    append(&conn_b, &forget_event("c-b1", &h(2))).expect("b1");

    // Force the (otherwise-random) applied_at to be identical on both
    // DBs — simulates the same-second migration race AND the
    // backup-restore-preserves-applied_at case in one stroke.
    let shared_applied: i64 = 1_761_600_000_000;
    conn_a
        .execute(
            "UPDATE consent_mirror_resets SET applied_at = ?1 WHERE migration_id = 21",
            rusqlite::params![shared_applied],
        )
        .expect("force a applied_at");
    conn_b
        .execute(
            "UPDATE consent_mirror_resets SET applied_at = ?1 WHERE migration_id = 21",
            rusqlite::params![shared_applied],
        )
        .expect("force b applied_at");

    // Confirm the nonces ARE different — this is the round-12 fix.
    let nonce_a = db_nonce_for(&conn_a, 21);
    let nonce_b = db_nonce_for(&conn_b, 21);
    assert_ne!(
        nonce_a, nonce_b,
        "randomblob(16) must mint distinct nonces per DB"
    );

    let dir_a = tempdir().expect("dir a");
    let dir_b = tempdir().expect("dir b");

    // Mirror A consumes the marker against DB-A and writes its sidecar.
    let mut mirror_a = ConsentLogMaterializer::open(dir_a.path()).expect("open a");
    assert_eq!(mirror_a.tick(&conn_a).expect("tick a"), 1);
    let sidecar_a = std::fs::read_to_string(dir_a.path().join("consent.mirror_resets_consumed"))
        .expect("a sidecar");
    assert!(
        sidecar_a.contains(&nonce_a),
        "mirror A sidecar must record DB-A's nonce: {sidecar_a:?}"
    );

    // Now copy mirror A's sidecar into mirror B's vault — this models
    // the failure mode where same applied_at would have falsely
    // matched. Under nonce binding, the entry cannot match DB-B's
    // marker (different nonce), so mirror B must still replay.
    std::fs::copy(
        dir_a.path().join("consent.mirror_resets_consumed"),
        dir_b.path().join("consent.mirror_resets_consumed"),
    )
    .expect("copy sidecar");

    let mut mirror_b = ConsentLogMaterializer::open(dir_b.path()).expect("open b");
    let n_b = mirror_b.tick(&conn_b).expect("tick b");
    assert_eq!(
        n_b, 1,
        "mirror B must replay against DB-B even though same-applied_at sidecar from DB-A is present"
    );

    let sidecar_b = std::fs::read_to_string(dir_b.path().join("consent.mirror_resets_consumed"))
        .expect("b sidecar");
    assert!(
        sidecar_b.contains(&nonce_b),
        "mirror B sidecar must now record DB-B's nonce: {sidecar_b:?}"
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

#[test]
fn reset_marker_replays_after_db_swap_with_stale_sidecar() {
    // Round-12 finding: a sidecar that records only `migration_id`
    // cannot distinguish DB instances. If a vault_dir is reused against
    // a different SQLite file (restore from backup, copy between
    // environments), a stale sidecar entry would silently suppress the
    // replay even though the new DB has freshly promoted legacy rows
    // that were never mirrored. Binding consumption to
    // `(migration_id, db_nonce)` defeats this: a different DB instance
    // has a different `db_nonce` (minted via randomblob(16) at
    // migration time) for the same migration_id, so the sidecar entry
    // no longer matches and tick() must replay.
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");

    // Simulate a stale sidecar from a *different* DB-instance: the
    // migration_id matches the live DB row, but the nonce does not. We
    // pick a fresh hex string that cannot collide with the real one.
    let real_nonce = db_nonce_for(&conn, 21);
    let stale_nonce = if real_nonce == "0".repeat(32) {
        "1".repeat(32)
    } else {
        "0".repeat(32)
    };
    assert_ne!(real_nonce, stale_nonce);
    let resets_consumed = dir.path().join("consent.mirror_resets_consumed");
    // Seed in the round-13 four-field bound format with a watermark
    // and line_count low enough to satisfy `cursor >= watermark` and
    // `live_line_count >= line_count` on their own — that way the
    // replay is forced specifically by the nonce mismatch, not by a
    // missing watermark, line_count, or a stale one.
    std::fs::write(&resets_consumed, format!("21:{stale_nonce}:0:0\n"))
        .expect("seed stale sidecar");

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open mirror");
    let n = mirror.tick(&conn).expect("tick");
    assert_eq!(
        n, 2,
        "stale sidecar from a different DB instance must not suppress replay"
    );
    assert_eq!(mirror.read_lines().expect("lines").len(), 2);

    // Sidecar must now record the *real* (migration_id, db_nonce,
    // watermark, line_count). Watermark = high_water of the replay = 2,
    // line_count = 2.
    let after = std::fs::read_to_string(&resets_consumed).expect("after");
    let expected = format!("21:{real_nonce}:2:2");
    assert!(
        after.lines().any(|l| l.trim() == expected),
        "post-replay sidecar must bind to the live DB's nonce: {after:?}"
    );
}

#[test]
fn reset_marker_does_not_replay_on_same_db() {
    // Companion to the swap test: when the DB instance is unchanged,
    // consumption recorded in `(migration_id, db_nonce)` form must
    // suppress further replays across tick() calls.
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
    let n = mirror.tick(&conn).expect("first tick");
    assert_eq!(n, 1, "first tick replays the marker");

    // Subsequent ticks must be no-ops — the sidecar entry now matches
    // the live DB's (21, db_nonce).
    let n2 = mirror.tick(&conn).expect("second tick");
    assert_eq!(n2, 0, "second tick must not replay on the same DB");

    append(&conn, &forget_event("c-2", &h(2))).expect("a2");
    let n3 = mirror.tick(&conn).expect("third tick");
    assert_eq!(
        n3, 1,
        "third tick mirrors the new row only, no reset replay"
    );
    assert_eq!(mirror.read_lines().expect("lines").len(), 2);
}

#[test]
fn sidecar_with_legacy_format_forces_replay() {
    // Round-3 finding: bare `migration_id` lines (the format from this
    // PR's earlier commit, before consumption was bound to a DB
    // identifier) must be treated as "unknown DB instance" and skipped.
    // They cannot match any (migration_id, db_nonce) tuple, so the next
    // tick forces a replay and the sidecar is rewritten in the new
    // bound format.
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");

    // Seed the legacy format the earlier commit produced.
    let resets_consumed = dir.path().join("consent.mirror_resets_consumed");
    std::fs::write(&resets_consumed, "21\n").expect("seed legacy sidecar");

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open mirror");
    let n = mirror.tick(&conn).expect("tick");
    assert_eq!(
        n, 2,
        "legacy-format sidecar must not suppress replay (no db_nonce to match)"
    );
    assert_eq!(mirror.read_lines().expect("lines").len(), 2);

    // Sidecar is rewritten in the new
    // `migration_id:db_nonce:watermark_rowid:line_count` form.
    let after = std::fs::read_to_string(&resets_consumed).expect("after");
    let nonce = db_nonce_for(&conn, 21);
    let expected = format!("21:{nonce}:2:2");
    assert!(
        after.lines().any(|l| l.trim() == expected),
        "sidecar must be rewritten in bound format: {after:?}"
    );
    assert!(
        !after.lines().any(|l| l.trim() == "21"),
        "legacy bare `21` line must not survive: {after:?}"
    );
}

#[test]
fn reset_marker_replays_when_log_truncated_below_recorded_count() {
    // Round-13 medium finding: `cursor >= watermark` is too weak as
    // proof the replay is intact. `recover_cursor_from_log` only
    // recovers the LAST well-formed envelope in a bounded scan
    // window, so a log truncated to keep only the watermark envelope
    // (or any tail-only rewrite that preserves the watermark line)
    // would falsely satisfy `cursor >= watermark` while erasing the
    // earlier rows the replay had committed. Binding consumption to
    // the live log's line count too defeats this: a tick-time count
    // below the recorded count proves a rollback the watermark
    // alone cannot see.
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");
    append(&conn, &forget_event("c-3", &h(3))).expect("a3");

    // First tick replays the reset and writes sidecar
    // `21:nonce:3:3` (watermark=3, line_count=3).
    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open");
    let n = mirror.tick(&conn).expect("first tick");
    assert_eq!(n, 3);

    let resets_consumed = dir.path().join("consent.mirror_resets_consumed");
    let nonce = db_nonce_for(&conn, 21);
    let after_first = std::fs::read_to_string(&resets_consumed).expect("after first");
    assert!(
        after_first
            .lines()
            .any(|l| l.trim() == format!("21:{nonce}:3:3")),
        "post-replay sidecar must record (watermark=3, line_count=3): {after_first:?}"
    );

    // Truncate the log to keep ONLY the last (watermark) envelope.
    // `recover_cursor_from_log` will still find the watermark envelope
    // and report cursor=3 → `cursor >= watermark` holds. But the log
    // has been gutted from 3 lines to 1; line_count check must catch
    // the rollback and force a replay.
    let lines = mirror.read_lines().expect("read lines");
    let truncated = format!("{}\n", lines[2]);
    std::fs::write(dir.path().join("consent.log"), &truncated).expect("rollback log");
    drop(mirror);

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("reopen");
    assert_eq!(
        mirror.cursor(),
        3,
        "recovered cursor still satisfies cursor >= watermark"
    );

    let n = mirror
        .tick(&conn)
        .expect("re-tick after tail-only truncation");
    assert_eq!(
        n, 3,
        "live line_count below recorded count must force replay even though cursor >= watermark"
    );
    assert_eq!(mirror.read_lines().expect("after replay").len(), 3);

    // Sidecar is rewritten with the freshly-measured line_count=3.
    let after = std::fs::read_to_string(&resets_consumed).expect("after");
    assert!(
        after.lines().any(|l| l.trim() == format!("21:{nonce}:3:3")),
        "post-replay sidecar must re-bind to (watermark=3, line_count=3): {after:?}"
    );
}

#[test]
fn sidecar_with_three_field_format_forces_replay() {
    // Round-13 medium finding: round-12 sidecar entries used the
    // three-field `migration_id:db_nonce:watermark` format with no
    // `line_count`. Such lines cannot prove the log still reflects
    // the replay (watermark alone is satisfied by a tail-only
    // truncation that preserves only the watermark envelope). The
    // sidecar parser treats any non-four-field line as unknown state
    // and the next tick force-replays.
    let conn = open_in_memory().expect("open store");
    let dir = tempdir().expect("tempdir");

    append(&conn, &forget_event("c-1", &h(1))).expect("a1");
    append(&conn, &forget_event("c-2", &h(2))).expect("a2");

    // Seed the round-12 three-field nonce-bound format.
    let resets_consumed = dir.path().join("consent.mirror_resets_consumed");
    let nonce = db_nonce_for(&conn, 21);
    std::fs::write(&resets_consumed, format!("21:{nonce}:2\n"))
        .expect("seed round-12 three-field sidecar");

    let mut mirror = ConsentLogMaterializer::open(dir.path()).expect("open mirror");
    let n = mirror.tick(&conn).expect("tick");
    assert_eq!(
        n, 2,
        "round-12 three-field sidecar (no line_count) must not suppress replay"
    );
    assert_eq!(mirror.read_lines().expect("lines").len(), 2);

    // Sidecar is rewritten in the round-13 four-field bound format.
    let after = std::fs::read_to_string(&resets_consumed).expect("after");
    let expected = format!("21:{nonce}:2:2");
    assert!(
        after.lines().any(|l| l.trim() == expected),
        "post-replay sidecar must include line_count: {after:?}"
    );
    assert!(
        !after.lines().any(|l| l.trim() == format!("21:{nonce}:2")),
        "round-12 three-field line must not survive: {after:?}"
    );
}
