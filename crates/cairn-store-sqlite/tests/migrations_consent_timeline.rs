//! 0023 `consent_timeline` — append-only receipt event log.
//!
//! Brief §14, Issue #253.

use cairn_store_sqlite::open_in_memory_sync;

#[test]
fn consent_timeline_table_has_expected_columns_in_order() {
    let conn = open_in_memory_sync().expect("open in-memory store");

    let mut stmt = conn
        .prepare("SELECT name FROM pragma_table_info('consent_timeline') ORDER BY cid")
        .expect("prepare pragma");
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("collect");

    assert_eq!(
        cols,
        vec![
            "consent_ref",
            "seq",
            "kind",
            "sensor_id",
            "scope",
            "decided_at",
            "expires_at",
            "payload_json",
        ],
        "consent_timeline columns must match brief §14 layout",
    );
}

#[test]
fn consent_timeline_primary_key_is_consent_ref_seq() {
    let conn = open_in_memory_sync().expect("open in-memory store");

    // pragma_table_info: pk column = position within PK (1-based, 0 for non-PK).
    let mut stmt = conn
        .prepare(
            "SELECT name, pk FROM pragma_table_info('consent_timeline') \
              WHERE pk > 0 ORDER BY pk",
        )
        .expect("prepare");
    let pk_cols: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("collect");

    assert_eq!(
        pk_cols,
        vec![("consent_ref".to_string(), 1), ("seq".to_string(), 2),],
        "PK must be (consent_ref, seq) in that order",
    );
}

#[test]
fn consent_timeline_accepts_well_formed_insert() {
    let conn = open_in_memory_sync().expect("open in-memory store");

    let n = conn
        .execute(
            "INSERT INTO consent_timeline \
              (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
              VALUES ('cref-1', 1, 'issued', 'hooks', 'private', '2025-01-01T00:00:00.000000000Z', '2025-02-01T00:00:00.000000000Z', '{}')",
            [],
        )
        .expect("insert should succeed");
    assert_eq!(n, 1);
}

#[test]
fn consent_timeline_update_aborts() {
    let conn = open_in_memory_sync().expect("open in-memory store");

    conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-2', 1, 'issued', 'hooks', 'private', '2025-01-01T00:00:00.000000000Z', '2025-02-01T00:00:00.000000000Z', '{}')",
        [],
    )
    .expect("seed insert");

    let res = conn.execute(
        "UPDATE consent_timeline SET kind = 'revoked' WHERE consent_ref = 'cref-2'",
        [],
    );
    assert!(
        res.is_err(),
        "UPDATE must be rejected by consent_timeline_immutable; got {res:?}",
    );
}

#[test]
fn consent_timeline_delete_aborts() {
    let conn = open_in_memory_sync().expect("open in-memory store");

    conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-3', 1, 'issued', 'hooks', 'private', '2025-01-01T00:00:00.000000000Z', '2025-02-01T00:00:00.000000000Z', '{}')",
        [],
    )
    .expect("seed insert");

    let res = conn.execute(
        "DELETE FROM consent_timeline WHERE consent_ref = 'cref-3'",
        [],
    );
    assert!(
        res.is_err(),
        "DELETE must be rejected by consent_timeline_no_delete; got {res:?}",
    );
}

#[test]
fn consent_timeline_check_rejects_unknown_kind() {
    let conn = open_in_memory_sync().expect("open in-memory store");

    let res = conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-4', 1, 'banana', 'hooks', 'private', '2025-01-01T00:00:00.000000000Z', NULL, '{}')",
        [],
    );
    assert!(res.is_err(), "CHECK must reject unknown kinds; got {res:?}");
}

#[test]
fn migration_head_is_at_least_23() {
    let conn = open_in_memory_sync().expect("open in-memory store");
    let head: i64 = conn
        .query_row("SELECT MAX(migration_id) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .expect("query head");
    assert!(
        head >= 23,
        "head migration must include 0023_consent_timeline; got {head}",
    );
}

/// Round 2 (Issue #253): one `consent_ref` must keep a single
/// `(sensor_id, scope)` tuple across every event. Reusing a
/// `consent_ref` for a different sensor or scope would let lint
/// silently widen a grant — so the trigger added in 0024 rejects
/// any second insert that disagrees with the first.
#[test]
fn consent_timeline_rejects_grant_widening_on_sensor_change() {
    let conn = open_in_memory_sync().expect("open in-memory store");

    conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-w1', 1, 'issued', 'hooks.a', 'private', \
                  '2025-01-01T00:00:00.000000000Z', NULL, '{}')",
        [],
    )
    .expect("seed insert under sensor=hooks.a");

    let res = conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-w1', 2, 'issued', 'hooks.b', 'private', \
                  '2025-01-02T00:00:00.000000000Z', NULL, '{}')",
        [],
    );
    assert!(
        res.is_err(),
        "trigger must reject sensor change under same consent_ref; got {res:?}",
    );
}

#[test]
fn consent_timeline_rejects_grant_widening_on_scope_change() {
    let conn = open_in_memory_sync().expect("open in-memory store");

    conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-w2', 1, 'issued', 'hooks.a', 'user=alice', \
                  '2025-01-01T00:00:00.000000000Z', NULL, '{}')",
        [],
    )
    .expect("seed insert under scope=user=alice");

    let res = conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-w2', 2, 'revoked', 'hooks.a', 'user=bob', \
                  '2025-01-02T00:00:00.000000000Z', NULL, '{}')",
        [],
    );
    assert!(
        res.is_err(),
        "trigger must reject scope change under same consent_ref; got {res:?}",
    );
}

#[test]
fn consent_timeline_accepts_consistent_grant_lifecycle() {
    let conn = open_in_memory_sync().expect("open in-memory store");

    for (seq, kind) in [(1u64, "issued"), (2, "expired")] {
        conn.execute(
            "INSERT INTO consent_timeline \
              (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
              VALUES ('cref-w3', ?1, ?2, 'hooks.a', 'private', \
                      '2025-01-01T00:00:00.000000000Z', NULL, '{}')",
            (seq, kind),
        )
        .unwrap_or_else(|e| panic!("event seq={seq} kind={kind} must be accepted: {e}"));
    }
}

// ─── Round 8: lifecycle invariants (migration 0025) ───────────────────

/// First event for a fresh `consent_ref` must be `issued`.
#[test]
fn consent_timeline_rejects_first_event_revoked() {
    let conn = open_in_memory_sync().expect("open in-memory store");
    let res = conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-l1', 1, 'revoked', 'hooks.a', 'private', \
                  '2025-01-01T00:00:00.000000000Z', NULL, '{}')",
        [],
    );
    assert!(res.is_err(), "first event must be issued; got {res:?}");
}

/// `seq` must equal `MAX(existing seq) + 1` per `consent_ref`.
#[test]
fn consent_timeline_rejects_seq_gap() {
    let conn = open_in_memory_sync().expect("open in-memory store");
    conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-l2', 1, 'issued', 'hooks.a', 'private', \
                  '2025-01-01T00:00:00.000000000Z', NULL, '{}')",
        [],
    )
    .expect("seed seq=1");

    let res = conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-l2', 3, 'revoked', 'hooks.a', 'private', \
                  '2025-01-02T00:00:00.000000000Z', NULL, '{}')",
        [],
    );
    assert!(
        res.is_err(),
        "seq gap (1 -> 3) must be rejected; got {res:?}",
    );
}

/// `decided_at` must be non-decreasing per `consent_ref`. This is the
/// load-bearing invariant: without it, a backdated `issued` event
/// after a record's creation time could retroactively make the record
/// appear authorized.
#[test]
fn consent_timeline_rejects_backdated_decided_at() {
    let conn = open_in_memory_sync().expect("open in-memory store");
    conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-l3', 1, 'issued', 'hooks.a', 'private', \
                  '2025-06-01T00:00:00.000000000Z', NULL, '{}')",
        [],
    )
    .expect("seed seq=1 at 2025-06-01");

    let res = conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-l3', 2, 'revoked', 'hooks.a', 'private', \
                  '2025-01-01T00:00:00.000000000Z', NULL, '{}')",
        [],
    );
    assert!(
        res.is_err(),
        "backdated decided_at must be rejected; got {res:?}",
    );
}

/// Equal `decided_at` across consecutive events is allowed (atomic write
/// of two events at the same instant is realistic).
#[test]
fn consent_timeline_accepts_equal_decided_at() {
    let conn = open_in_memory_sync().expect("open in-memory store");
    for (seq, kind) in [(1u64, "issued"), (2, "revoked")] {
        conn.execute(
            "INSERT INTO consent_timeline \
              (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
              VALUES ('cref-l4', ?1, ?2, 'hooks.a', 'private', \
                      '2025-01-01T00:00:00.000000000Z', NULL, '{}')",
            (seq, kind),
        )
        .unwrap_or_else(|e| panic!("seq={seq} must be accepted: {e}"));
    }
}

// ─── Round 9: lifecycle tightening (migration 0026) ──────────────────

/// Offset-form `decided_at` ("...+01:00") sorts later as TEXT than an
/// existing UTC row but is chronologically earlier. The Round-8
/// non-decreasing trigger compared raw text and would let it through;
/// 0026 rejects any non-Z form at the trigger level so text comparison
/// equals chronological comparison.
#[test]
fn consent_timeline_rejects_offset_form_decided_at() {
    let conn = open_in_memory_sync().expect("open in-memory store");
    let res = conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-r9-1', 1, 'issued', 'hooks.a', 'private', \
                  '2026-01-01T00:30:00+01:00', NULL, '{}')",
        [],
    );
    assert!(
        res.is_err(),
        "offset-form decided_at must be rejected; got {res:?}",
    );
}

/// Same rule for `expires_at` when present.
#[test]
fn consent_timeline_rejects_offset_form_expires_at() {
    let conn = open_in_memory_sync().expect("open in-memory store");
    let res = conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-r9-2', 1, 'issued', 'hooks.a', 'private', \
                  '2026-01-01T00:00:00.000000000Z', '2026-02-01T00:00:00+02:00', '{}')",
        [],
    );
    assert!(
        res.is_err(),
        "offset-form expires_at must be rejected; got {res:?}",
    );
}

/// Fractional UTC bypass: lexically "00.100Z" sorts BEFORE "00Z" even
/// though it's later in time. 0027 pins `decided_at` to the 20-char
/// seconds form so TEXT comparison stays chronological.
#[test]
fn consent_timeline_rejects_fractional_utc_decided_at() {
    let conn = open_in_memory_sync().expect("open in-memory store");
    let res = conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-r10-1', 1, 'issued', 'hooks.a', 'private', \
                  '2026-01-01T00:00:00.100Z', NULL, '{}')",
        [],
    );
    assert!(
        res.is_err(),
        "fractional decided_at must be rejected; got {res:?}",
    );
}

/// Same canonical-seconds rule for `expires_at`.
#[test]
fn consent_timeline_rejects_fractional_utc_expires_at() {
    let conn = open_in_memory_sync().expect("open in-memory store");
    let res = conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-r10-2', 1, 'issued', 'hooks.a', 'private', \
                  '2026-01-01T00:00:00.000000000Z', '2026-02-01T00:00:00.500Z', '{}')",
        [],
    );
    assert!(
        res.is_err(),
        "fractional expires_at must be rejected; got {res:?}",
    );
}

/// A fresh `consent_ref` must enter the table at seq=1. Without this
/// guard, the first-event-issued trigger (which only fires when seq=1)
/// silently passes a malformed first row at seq=99.
#[test]
fn consent_timeline_rejects_fresh_consent_ref_with_seq_not_one() {
    let conn = open_in_memory_sync().expect("open in-memory store");
    let res = conn.execute(
        "INSERT INTO consent_timeline \
          (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
          VALUES ('cref-r9-3', 99, 'issued', 'hooks.a', 'private', \
                  '2026-01-01T00:00:00.000000000Z', NULL, '{}')",
        [],
    );
    assert!(
        res.is_err(),
        "fresh consent_ref with seq != 1 must be rejected; got {res:?}",
    );
}
