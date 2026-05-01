//! Integration tests for the migration set.

use cairn_store_sqlite::{
    migrations::migrations, open_in_memory_sync as open_in_memory, open_sync as open,
};
use rusqlite::params;
use tempfile::tempdir;

#[test]
fn fresh_in_memory_opens_to_head() {
    let conn = open_in_memory().expect("open in-memory store");
    let head: i64 = conn
        .query_row("SELECT MAX(migration_id) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .expect("query head");
    assert_eq!(head, 21);
}

#[test]
fn fresh_vault_opens_and_reopens_idempotent() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _conn = open(&db).expect("first open");
    }
    let conn = open(&db).expect("reopen");
    let head: i64 = conn
        .query_row("SELECT MAX(migration_id) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .expect("query head");
    assert_eq!(head, 21);
}

#[test]
fn pragmas_applied() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    let conn = open(&db).expect("open");

    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("journal_mode");
    assert_eq!(journal.to_lowercase(), "wal");

    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .expect("foreign_keys");
    assert_eq!(fk, 1);
}

#[test]
fn migrations_validate() {
    migrations()
        .validate()
        .expect("migrations validate against schema");
}

#[test]
fn fts_round_trip() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO records \
         (record_id, target_id, version, path, kind, class, visibility, scope, \
          actor_chain, body, body_hash, created_at, updated_at, active, tombstoned, is_static) \
         VALUES ('r1','t1',1,'p','note','n','public','s','[]','hello world','h',0,0,1,0,0)",
        [],
    )
    .expect("insert record");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records_fts WHERE records_fts MATCH ?",
            params!["hello"],
            |r| r.get(0),
        )
        .expect("fts query");
    assert_eq!(count, 1);
}

#[test]
fn updates_edge_kind_flip_blocked() {
    let conn = open_in_memory().expect("open");
    conn.execute_batch(
        "INSERT INTO records \
          (record_id, target_id, version, path, kind, class, visibility, scope, \
           actor_chain, body, body_hash, created_at, updated_at, active, tombstoned, is_static) \
          VALUES ('r1','t1',1,'p','note','n','public','s','[]','b1','h',0,0,1,0,0); \
         INSERT INTO records \
          (record_id, target_id, version, path, kind, class, visibility, scope, \
           actor_chain, body, body_hash, created_at, updated_at, active, tombstoned, is_static) \
          VALUES ('r2','t2',1,'p','note','n','public','s','[]','b2','h',0,0,1,0,0); \
         INSERT INTO edges (src, dst, kind) VALUES ('r1','r2','related');",
    )
    .expect("seed records + benign edge");
    let err = conn
        .execute(
            "UPDATE edges SET kind = 'updates' WHERE src = 'r1' AND dst = 'r2'",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("updates edge identity"),
        "kind-flip should be blocked, got: {err}"
    );
}

#[test]
fn schema_drift_detected_on_weakened_trigger() {
    use tempfile::tempdir;
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _ = open(&db).expect("first open");
    }
    {
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        // Same name, weaker body — name-only fingerprint would miss this.
        conn.execute_batch(
            "DROP TRIGGER schema_migrations_no_delete; \
             CREATE TRIGGER schema_migrations_no_delete \
               BEFORE DELETE ON schema_migrations \
               FOR EACH ROW WHEN 0 \
             BEGIN SELECT 1; END;",
        )
        .expect("recreate weaker trigger");
    }
    let err = open(&db).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("DDL digest mismatch") || msg.contains("schema fingerprint"),
        "DDL drift should be detected, got: {err}"
    );
}

#[test]
fn schema_drift_detected_on_relaxed_consent_journal_kind_check() {
    // Round-3 adversarial-review (Medium) follow-up for #255: the kind
    // CHECK introduced in 0021 is a column-level constraint, not a named
    // schema object — `EXPECTED_OBJECTS` cannot enumerate it. The
    // verifier's named-substring check (`verify_consent_journal_kind_check`)
    // pins the §14 invariant by inspecting the live CREATE TABLE text.
    // Drop + recreate without the CHECK; verifier must surface the named
    // drift.
    use tempfile::tempdir;
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _ = open(&db).expect("first open");
    }
    {
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        // Drop the append-only triggers so we can DROP TABLE, then
        // recreate consent_journal WITHOUT the kind CHECK clause.
        // Mirrors the columns of the post-0021 table exactly otherwise.
        conn.execute_batch(
            "DROP TRIGGER consent_journal_immutable; \
             DROP TRIGGER consent_journal_no_delete; \
             DROP TABLE consent_journal; \
             CREATE TABLE consent_journal ( \
               consent_id      TEXT NOT NULL PRIMARY KEY, \
               subject         TEXT NOT NULL, \
               scope           TEXT NOT NULL, \
               decision        TEXT NOT NULL CHECK (decision IN ('GRANT','REVOKE')), \
               reason          TEXT, \
               granted_by      TEXT NOT NULL, \
               decided_at      INTEGER NOT NULL, \
               expires_at      INTEGER, \
               op_id           TEXT, \
               kind            TEXT NOT NULL, \
               sensor_id       TEXT, \
               actor           TEXT, \
               payload_json    TEXT, \
               decided_at_iso  TEXT, \
               expires_at_iso  TEXT \
             );",
        )
        .expect("recreate consent_journal without kind CHECK");
    }
    let err = open(&db).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("consent_journal kind CHECK") || msg.contains("DDL digest mismatch"),
        "kind CHECK drift should be detected by name, got: {err}"
    );
}

#[test]
fn schema_drift_detected_on_dropped_trigger() {
    use tempfile::tempdir;
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _ = open(&db).expect("first open");
    }
    {
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        conn.execute("DROP TRIGGER records_fts_ai", [])
            .expect("drop trigger");
    }
    let err = open(&db).unwrap_err();
    assert!(
        format!("{err}").contains("schema fingerprint mismatch"),
        "drift should be detected, got: {err}"
    );
}

#[test]
fn migration_hash_drift_detected() {
    use tempfile::tempdir;
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    {
        let _ = open(&db).expect("first open");
    }
    {
        // Bypass the immutable trigger by dropping it, then tampering.
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        conn.execute("DROP TRIGGER schema_migrations_immutable", [])
            .expect("drop immutability trigger");
        conn.execute(
            "UPDATE schema_migrations SET sql_hash = 'deadbeef' WHERE migration_id = 1",
            [],
        )
        .expect("tamper hash");
    }
    let err = open(&db).unwrap_err();
    assert!(
        format!("{err}").contains("hash mismatch") || format!("{err}").contains("schema drift"),
        "hash drift should be detected, got: {err}"
    );
}

#[test]
fn schema_migrations_is_append_only() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute("DELETE FROM schema_migrations WHERE migration_id = 1", [])
        .unwrap_err();
    assert!(format!("{err}").contains("schema_migrations is append-only"));
}

#[test]
fn consent_journal_kind_domain_enforced() {
    // Phase-B (#255): the §14 domain is enforced by a column CHECK; the
    // trigger path is gone but the assertion stays broad to keep the test
    // robust to either form.
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c1', 's1', 'private', 'GRANT', 'hmn:t', 0, \
                     'not_a_kind', 'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"decision\",\"subject_code\":\"x\"}')",
            [],
        )
        .unwrap_err();
    let msg = format!("{err}");
    let msg_uc = msg.to_uppercase();
    assert!(
        msg.contains("§14 domain") || (msg_uc.contains("CHECK") && msg.contains("kind")),
        "kind domain gate should fire (trigger or column CHECK on kind), got: {err}"
    );
}

#[test]
fn consent_journal_kind_check_constraint_rejects_unknown() {
    // Phase-B (#255): the column-level CHECK on consent_journal.kind is the
    // canonical §14 domain gate. Pin the new behavior tightly: an unknown
    // kind must surface a CHECK-constraint error naming the kind column,
    // not bleed through to a downstream trigger.
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-bad-kind', 'agent.x', 'private', 'GRANT', 'hmn:t', 0, \
                     'totally_made_up_kind', 'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"decision\",\"subject_code\":\"agent.x\"}')",
            [],
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("CHECK") && msg.contains("kind"),
        "column CHECK on kind should fire, got: {err}"
    );
}

#[test]
fn consent_journal_accepts_known_kinds() {
    let conn = open_in_memory().expect("open");
    let hash = "hash:11111111111111111111111111111111";
    let label = "local:hook:host:v1";
    let snr_subject = format!("snr:{label}");
    let sensor_payload = format!(
        "{{\"shape\":\"sensor_toggle\",\"sensor_label\":\"{label}\",\
          \"reason_code\":\"first_run_prompt\"}}"
    );
    let intent_payload = format!(
        "{{\"shape\":\"intent_receipt\",\"target_id_hash\":\"{hash}\",\
          \"scope_tier\":\"private\",\"reason_code\":\"user_command\"}}"
    );
    let promote_payload = format!(
        "{{\"shape\":\"promote_receipt\",\"target_id_hash\":\"{hash}\",\
          \"from_tier\":\"private\",\"to_tier\":\"team\",\"receipt_id\":\"rcpt-1\"}}"
    );
    let policy_payload =
        r#"{"shape":"policy_delta","key":"sensors.x","from_code":"a","to_code":"b"}"#.to_owned();
    let decision_payload = r#"{"shape":"decision","subject_code":"share_link:abcd"}"#.to_owned();
    // (kind, subject, sensor_id, payload)
    let cases: &[(&str, &str, Option<&str>, String)] = &[
        (
            "sensor_enable",
            &snr_subject,
            Some(label),
            sensor_payload.clone(),
        ),
        ("sensor_disable", &snr_subject, Some(label), sensor_payload),
        ("policy_change", "s", None, policy_payload),
        ("remember_intent", hash, None, intent_payload.clone()),
        ("forget_intent", hash, None, intent_payload),
        ("grant", "s", None, decision_payload.clone()),
        ("revoke", "s", None, decision_payload),
        ("promote_receipt", hash, None, promote_payload),
    ];
    for (kind, subject, sensor_id, payload) in cases {
        conn.execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, decided_at_iso, actor, sensor_id, payload_json) \
             VALUES (?, ?, 'private', 'GRANT', 'hmn:t', 0, ?, '2026-04-28T12:00:00Z', \
                     'hmn:t', ?, ?)",
            params![format!("c-{kind}"), subject, kind, sensor_id, payload],
        )
        .unwrap_or_else(|e| panic!("kind {kind} should be accepted: {e}"));
    }
}

#[test]
fn consent_journal_event_requires_iso_timestamp() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, payload_json) \
             VALUES ('c-no-iso', 's', 'private', 'GRANT', 'hmn:t', 0, 'grant', \
                     'hmn:t', '{\"shape\":\"decision\",\"subject_code\":\"x\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("decided_at_iso"),
        "iso requirement should fire, got: {err}"
    );
}

#[test]
fn consent_journal_kind_not_null_enforced() {
    // Phase-B (#255, brief §14): pin the column-level NOT NULL constraint on
    // consent_journal.kind as the canonical Phase-B gate. Migration 0021
    // promotes `kind` to NOT NULL; this test isolates that constraint by
    // inserting a fully event-shape compliant row — every required field
    // (actor, decided_at_iso, well-formed payload_json, valid consent_id /
    // scope / subject) is populated EXCEPT `kind`. With the row otherwise
    // legal, every BEFORE INSERT trigger rebuilt by 0021 passes, leaving
    // the column NOT NULL on `kind` as the only remaining gate.
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               actor, decided_at_iso, payload_json) \
             VALUES ('c-no-kind', 'agent.x', 'private', 'GRANT', 'hmn:t', 0, \
                     'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"decision\",\"subject_code\":\"agent.x\"}')",
            [],
        )
        .unwrap_err();
    let msg = format!("{err}");
    let msg_uc = msg.to_uppercase();
    assert!(
        msg_uc.contains("NOT NULL") && msg.contains("kind"),
        "column NOT NULL on kind should fire, got: {err}"
    );
}

#[test]
fn forget_intent_payload_must_be_body_free() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c2', 'hash:11111111111111111111111111111111', \
                     'private', 'GRANT', 'hmn:t', 0, 'forget_intent', \
                     'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"intent_receipt\",\
                       \"target_id_hash\":\"hash:11111111111111111111111111111111\",\
                       \"scope_tier\":\"private\",\"reason_code\":\"user_command\",\
                       \"body\":\"leak\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("body-free"),
        "forget receipt body guard should fire, got: {err}"
    );
}

#[test]
fn forget_intent_payload_rejects_extended_banned_keys() {
    // Brought into the trigger after Codex round 1 — earlier list missed
    // these three, leaving a direct-SQL leak path. Test each individually.
    let conn = open_in_memory().expect("open");
    for banned in ["message", "payload_text", "user_input"] {
        let hash = "hash:11111111111111111111111111111111";
        let payload = format!(
            "{{\"shape\":\"intent_receipt\",\"target_id_hash\":\"{hash}\",\
              \"scope_tier\":\"private\",\"reason_code\":\"user_command\",\
              \"{banned}\":\"leak\"}}"
        );
        let err = conn
            .execute(
                "INSERT INTO consent_journal \
                  (consent_id, subject, scope, decision, granted_by, decided_at, \
                   kind, actor, decided_at_iso, payload_json) \
                 VALUES (?, ?, 'private', 'GRANT', 'hmn:t', 0, 'forget_intent', \
                         'hmn:t', '2026-04-28T12:00:00Z', ?)",
                params![format!("c-leak-{banned}"), hash, payload],
            )
            .unwrap_err();
        assert!(
            format!("{err}").contains("body-free"),
            "banned key {banned} must be rejected, got: {err}"
        );
    }
}

#[test]
fn forget_intent_payload_rejects_malformed_json() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-bad-json', 'hash:11111111111111111111111111111111', \
                     'private', 'GRANT', 'hmn:t', 0, 'forget_intent', \
                     'hmn:t', \
                     '2026-04-28T12:00:00Z', 'not json at all')",
            [],
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("body-free") || msg.contains("valid JSON payload"),
        "malformed JSON must be rejected, got: {msg}"
    );
}

#[test]
fn non_forget_payload_also_body_free() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-promote-leak', 'hash:11111111111111111111111111111111', \
                     'team:p', 'GRANT', 'hmn:t', 0, \
                     'promote_receipt', 'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"promote_receipt\",\
                       \"target_id_hash\":\"hash:11111111111111111111111111111111\",\
                       \"from_tier\":\"private\",\"to_tier\":\"team\",\
                       \"receipt_id\":\"rcpt-1\",\
                       \"body\":\"x\"}')",
            [],
        )
        .unwrap_err();
    assert!(format!("{err}").contains("body-free"));
}

#[test]
fn forget_intent_payload_accepts_hash_only() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, actor, decided_at_iso, payload_json) \
         VALUES ('c3', 'hash:11111111111111111111111111111111', \
                 'private', 'GRANT', 'hmn:t', 0, 'forget_intent', \
                 'hmn:t', '2026-04-28T12:00:00Z', \
                 '{\"shape\":\"intent_receipt\",\
                   \"target_id_hash\":\"hash:11111111111111111111111111111111\",\
                   \"scope_tier\":\"private\",\"reason_code\":\"user_command\"}')",
        [],
    )
    .expect("hash-only payload should be accepted");
}

#[test]
fn consent_journal_queryable_by_op_actor_sensor_scope() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, decided_at_iso, op_id, actor, sensor_id, payload_json) \
         VALUES ('c4', 'snr:local:screen:host:v1', 'global', 'GRANT', 'hmn:t', 0, \
                 'sensor_enable', '2026-04-28T12:00:00Z', \
                 'op-1', 'hmn:tafeng', 'local:screen:host:v1', \
                 '{\"shape\":\"sensor_toggle\",\"sensor_label\":\"local:screen:host:v1\",\
                   \"reason_code\":\"first_run_prompt\"}')",
        [],
    )
    .expect("seed sensor_enable row");

    // queryable by operation
    let by_op: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE op_id = 'op-1'",
            [],
            |r| r.get(0),
        )
        .expect("by op");
    assert_eq!(by_op, 1);

    // queryable by identity (actor)
    let by_actor: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE actor = 'hmn:tafeng'",
            [],
            |r| r.get(0),
        )
        .expect("by actor");
    assert_eq!(by_actor, 1);

    // queryable by sensor
    let by_sensor: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE sensor_id = 'local:screen:host:v1'",
            [],
            |r| r.get(0),
        )
        .expect("by sensor");
    assert_eq!(by_sensor, 1);

    // queryable by scope (already covered by the 0005 index, asserted here
    // for completeness against the issue AC).
    let by_scope: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE scope = 'global'",
            [],
            |r| r.get(0),
        )
        .expect("by scope");
    assert_eq!(by_scope, 1);
}

#[test]
fn consent_journal_payload_missing_shape_is_rejected() {
    // Round 4 hardening: an empty object `{}` with no `shape` key bypassed
    // the original trigger because `json_extract` returned NULL and a NULL
    // WHEN clause never fires. Now the trigger guards on `json_type` of
    // `$.shape` returning the literal `'text'`. We use `policy_change` to
    // isolate this assertion from the round-5 hash-payload trigger.
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-noshape', 'sensors.x', \
                     'global', 'GRANT', 'hmn:t', 0, 'policy_change', \
                     'hmn:t', '2026-04-28T12:00:00Z', '{}')",
            [],
        )
        .unwrap_err();
    let msg = format!("{err}");
    // SQLite trigger fire order is undefined; either the shape or the
    // required-fields trigger wins. Both are valid violations.
    assert!(
        msg.contains("payload shape must match kind") || msg.contains("required field"),
        "missing-shape payload must be rejected, got: {msg}"
    );
}

#[test]
fn consent_journal_sensor_kind_requires_sensor_id() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-sensor-no-id', 'snr:local:hook:host:v1', 'global', 'GRANT', \
                     'hmn:t', 0, 'sensor_enable', 'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"sensor_toggle\",\"sensor_label\":\"local:hook:host:v1\",\"reason_code\":\"first_run_prompt\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("sensor kinds require sensor_id"),
        "sensor row without sensor_id must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_sensor_id_must_match_payload_label() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, sensor_id, payload_json) \
             VALUES ('c-sensor-mismatch', 'snr:local:a:host:v1', 'global', 'GRANT', \
                     'hmn:t', 0, 'sensor_enable', 'hmn:t', '2026-04-28T12:00:00Z', \
                     'local:a:host:v1', \
                     '{\"shape\":\"sensor_toggle\",\"sensor_label\":\"local:b:host:v1\",\
                       \"reason_code\":\"first_run_prompt\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("sensor_id must equal payload.sensor_label"),
        "sensor_id != payload.sensor_label must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_non_sensor_kind_forbids_sensor_id() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, sensor_id, payload_json) \
             VALUES ('c-policy-with-sensor', 'sensors.x', 'global', 'GRANT', \
                     'hmn:t', 0, 'policy_change', 'hmn:t', '2026-04-28T12:00:00Z', \
                     'local:hook:host:v1', \
                     '{\"shape\":\"policy_delta\",\"key\":\"sensors.x\",\
                       \"from_code\":\"a\",\"to_code\":\"b\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("non-sensor kinds must not carry sensor_id"),
        "non-sensor kind with sensor_id must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_sensor_subject_must_match_sensor_id() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, sensor_id, payload_json) \
             VALUES ('c-sensor-bad-subject', 'snr:local:WRONG:host:v1', 'global', 'GRANT', \
                     'hmn:t', 0, 'sensor_enable', 'hmn:t', '2026-04-28T12:00:00Z', \
                     'local:hook:host:v1', \
                     '{\"shape\":\"sensor_toggle\",\"sensor_label\":\"local:hook:host:v1\",\"reason_code\":\"first_run_prompt\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("subject must be `snr:` + sensor_id"),
        "sensor row with subject != snr:+sensor_id must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_hash_kind_subject_shape_enforced() {
    let conn = open_in_memory().expect("open");
    // Raw text, no `hash:` / `sha256:` prefix.
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-bad-subject', 'TOPSECRETBODY', 'private', 'GRANT', \
                     'hmn:t', 0, 'forget_intent', 'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"intent_receipt\",\
                       \"target_id_hash\":\"hash:11111111111111111111111111111111\",\
                       \"scope_tier\":\"private\",\"reason_code\":\"user_command\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("subject must be sha256:64hex or hash:32..128hex"),
        "raw subject on forget_intent must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_hash_kind_target_id_hash_shape_enforced() {
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-bad-target', 'hash:11111111111111111111111111111111', \
                     'private', 'GRANT', 'hmn:t', 0, 'forget_intent', \
                     'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"intent_receipt\",\"target_id_hash\":\"plainstring\",\
                       \"scope_tier\":\"private\",\"reason_code\":\"user_command\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("target_id_hash must be sha256:64hex or hash:32..128hex"),
        "raw target_id_hash must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_sensor_payload_requires_sensor_label() {
    // Round 5 hardening: the previous trigger only fired when
    // `sensor_label` was a text mismatch, letting payloads without
    // `sensor_label` through. Serde would then fail to decode the
    // append-only row at mirror time. Now the trigger fires on missing /
    // non-text values too.
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, sensor_id, payload_json) \
             VALUES ('c-no-label', 'snr:local:hook:host:v1', 'global', 'GRANT', \
                     'hmn:t', 0, 'sensor_enable', 'hmn:t', '2026-04-28T12:00:00Z', \
                     'local:hook:host:v1', \
                     '{\"shape\":\"sensor_toggle\",\"reason_code\":\"first_run_prompt\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("sensor_id must equal payload.sensor_label"),
        "missing sensor_label must be rejected, got: {err}"
    );

    // Numeric (non-text) sensor_label is also rejected.
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, sensor_id, payload_json) \
             VALUES ('c-num-label', 'snr:local:hook:host:v1', 'global', 'GRANT', \
                     'hmn:t', 0, 'sensor_enable', 'hmn:t', '2026-04-28T12:00:00Z', \
                     'local:hook:host:v1', \
                     '{\"shape\":\"sensor_toggle\",\"sensor_label\":42,\
                       \"reason_code\":\"first_run_prompt\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("sensor_id must equal payload.sensor_label"),
        "non-text sensor_label must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_hash_kind_requires_target_id_hash_text() {
    // Round 5 hardening: payloads missing or non-text `target_id_hash`
    // were previously accepted because the trigger only ran when
    // `json_type = 'text'`. Now missing / null / numeric all fail.
    let conn = open_in_memory().expect("open");
    let hash = "hash:11111111111111111111111111111111";
    let suffix = "\"scope_tier\":\"private\",\"reason_code\":\"user_command\"}";
    for (label, payload) in &[
        (
            "missing",
            format!("{{\"shape\":\"intent_receipt\",{suffix}"),
        ),
        (
            "null",
            format!("{{\"shape\":\"intent_receipt\",\"target_id_hash\":null,{suffix}"),
        ),
        (
            "number",
            format!("{{\"shape\":\"intent_receipt\",\"target_id_hash\":7,{suffix}"),
        ),
    ] {
        let err = conn
            .execute(
                "INSERT INTO consent_journal \
                  (consent_id, subject, scope, decision, granted_by, decided_at, \
                   kind, actor, decided_at_iso, payload_json) \
                 VALUES (?, ?, 'private', 'GRANT', 'hmn:t', 0, 'forget_intent', \
                         'hmn:t', '2026-04-28T12:00:00Z', ?)",
                params![format!("c-{label}"), hash, payload],
            )
            .unwrap_err();
        assert!(
            format!("{err}").contains("target_id_hash"),
            "{label} target_id_hash must be rejected, got: {err}"
        );
    }
}

type RequiredFieldCase = (
    &'static str,         // description
    &'static str,         // kind
    String,               // subject
    Option<&'static str>, // sensor_id
    String,               // payload
    &'static str,         // expected fragment
);

#[test]
fn consent_journal_payload_required_fields_enforced() {
    // Round 6 hardening: every serde-required payload field per shape
    // must be present and JSON-text. Without these guards, a direct-SQL
    // writer could pass earlier triggers but produce an undecodable
    // append-only row that bricks the mirror.
    let conn = open_in_memory().expect("open");
    let hash = "hash:11111111111111111111111111111111";
    let label = "local:hook:host:v1";
    let snr = format!("snr:{label}");
    let cases: &[RequiredFieldCase] = &[
        (
            "sensor_toggle missing reason_code",
            "sensor_enable",
            snr,
            Some(label),
            format!("{{\"shape\":\"sensor_toggle\",\"sensor_label\":\"{label}\"}}"),
            "required field",
        ),
        (
            "policy_delta missing from_code",
            "policy_change",
            "sensors.x".to_owned(),
            None,
            r#"{"shape":"policy_delta","key":"sensors.x","to_code":"b"}"#.to_owned(),
            "required field",
        ),
        (
            "intent_receipt missing scope_tier",
            "forget_intent",
            hash.to_owned(),
            None,
            format!(
                "{{\"shape\":\"intent_receipt\",\"target_id_hash\":\"{hash}\",\
                  \"reason_code\":\"user_command\"}}"
            ),
            "required field",
        ),
        (
            "decision missing subject_code",
            "grant",
            "share_link:a".to_owned(),
            None,
            r#"{"shape":"decision"}"#.to_owned(),
            "required field",
        ),
        (
            "promote_receipt missing receipt_id",
            "promote_receipt",
            hash.to_owned(),
            None,
            format!(
                "{{\"shape\":\"promote_receipt\",\"target_id_hash\":\"{hash}\",\
                  \"from_tier\":\"private\",\"to_tier\":\"team\"}}"
            ),
            "required field",
        ),
    ];
    for (desc, kind, subject, sensor_id, payload, frag) in cases {
        let cid: String = desc
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let err = conn
            .execute(
                "INSERT INTO consent_journal \
                  (consent_id, subject, scope, decision, granted_by, decided_at, \
                   kind, actor, decided_at_iso, sensor_id, payload_json) \
                 VALUES (?, ?, 'private', 'GRANT', 'hmn:t', 0, ?, 'hmn:t', \
                         '2026-04-28T12:00:00Z', ?, ?)",
                params![format!("c-{cid}"), subject, kind, sensor_id, payload],
            )
            .unwrap_err();
        assert!(
            format!("{err}").contains(frag),
            "{desc} must be rejected with `{frag}`, got: {err}"
        );
    }
}

#[test]
fn consent_journal_payload_rejects_invalid_visibility_tier() {
    // Round 7 hardening: scope_tier / from_tier / to_tier are
    // `MemoryVisibility` in serde — not just any text. A direct insert
    // with a valid shape but a bogus tier value passes the earlier
    // text-type guards and would brick `serde_json::from_str` at
    // mirror time.
    let conn = open_in_memory().expect("open");
    let hash = "hash:11111111111111111111111111111111";
    let payload = format!(
        "{{\"shape\":\"intent_receipt\",\"target_id_hash\":\"{hash}\",\
          \"scope_tier\":\"bogus\",\"reason_code\":\"user_command\"}}"
    );
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-bad-tier', ?, 'private', 'GRANT', 'hmn:t', 0, \
                     'forget_intent', 'hmn:t', '2026-04-28T12:00:00Z', ?)",
            params![hash, payload],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("required field"),
        "bogus scope_tier must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_payload_rejects_unknown_top_level_key() {
    // Round 7 hardening: `ConsentPayload` is `deny_unknown_fields` in
    // serde. A direct insert with a valid shape but an unknown extra
    // key would brick the mirror decoder.
    let conn = open_in_memory().expect("open");
    let hash = "hash:11111111111111111111111111111111";
    let payload = format!(
        "{{\"shape\":\"intent_receipt\",\"target_id_hash\":\"{hash}\",\
          \"scope_tier\":\"private\",\"reason_code\":\"user_command\",\
          \"sneaky_extra\":\"x\"}}"
    );
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-unknown-key', ?, 'private', 'GRANT', 'hmn:t', 0, \
                     'forget_intent', 'hmn:t', '2026-04-28T12:00:00Z', ?)",
            params![hash, payload],
        )
        .unwrap_err();
    let msg = format!("{err}");
    // Either unknown_top_level_keys or keys_match_shape trigger fires
    // first (SQLite trigger order is undefined). Both reject the row.
    assert!(
        msg.contains("unknown top-level key") || msg.contains("not allowed for its shape"),
        "unknown payload key must be rejected, got: {msg}"
    );
}

#[test]
fn consent_journal_decision_policy_code_must_be_text_or_null() {
    // Round 7 hardening: `policy_code` is `Option<String>` in serde —
    // null and absent are both fine, but any other JSON type fails to
    // decode.
    let conn = open_in_memory().expect("open");
    let payload = r#"{"shape":"decision","subject_code":"share_link:abcd","policy_code":7}"#;
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-pol-num', 'share_link:abcd', 'private', 'GRANT', \
                     'hmn:t', 0, 'grant', 'hmn:t', '2026-04-28T12:00:00Z', ?)",
            params![payload],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("required field"),
        "non-text policy_code must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_event_rejects_nonpositive_rowid() {
    // Round 8 hardening: SQLite normally auto-assigns positive rowids,
    // but a direct-SQL writer can set them explicitly. The mirror cursor
    // model reads `rowid > cursor` starting at 0, so rowid 0 or negative
    // would be a permanent audit gap.
    let conn = open_in_memory().expect("open");
    let hash = "hash:11111111111111111111111111111111";
    let payload = format!(
        "{{\"shape\":\"intent_receipt\",\"target_id_hash\":\"{hash}\",\
          \"scope_tier\":\"private\",\"reason_code\":\"user_command\"}}"
    );
    for bad in [0i64, -1] {
        let err = conn
            .execute(
                "INSERT INTO consent_journal \
                  (rowid, consent_id, subject, scope, decision, granted_by, decided_at, \
                   kind, actor, decided_at_iso, payload_json) \
                 VALUES (?, ?, ?, 'private', 'GRANT', 'hmn:t', 0, \
                         'forget_intent', 'hmn:t', '2026-04-28T12:00:00Z', ?)",
                params![bad, format!("c-rowid-{bad}"), hash, payload],
            )
            .unwrap_err();
        assert!(
            format!("{err}").contains("require positive rowid"),
            "rowid={bad} must be rejected, got: {err}"
        );
    }
}

#[test]
fn consent_journal_payload_rejects_cross_variant_key() {
    // Round 8 hardening: even though `receipt_id` is allowed for
    // promote_receipt, it is NOT allowed for intent_receipt. The earlier
    // union allowlist let it through; the per-shape trigger rejects it.
    let conn = open_in_memory().expect("open");
    let hash = "hash:11111111111111111111111111111111";
    let payload = format!(
        "{{\"shape\":\"intent_receipt\",\"target_id_hash\":\"{hash}\",\
          \"scope_tier\":\"private\",\"reason_code\":\"user_command\",\
          \"receipt_id\":\"rcpt-xx\"}}"
    );
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-cross-key', ?, 'private', 'GRANT', 'hmn:t', 0, \
                     'forget_intent', 'hmn:t', '2026-04-28T12:00:00Z', ?)",
            params![hash, payload],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("not allowed for its shape"),
        "cross-variant key must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_payload_rejects_smuggled_reason_code() {
    // Round 8 hardening: reason_code must be a closed lower-snake class,
    // not arbitrary user text. `please forget secret token` would slip
    // through into consent.log otherwise.
    let conn = open_in_memory().expect("open");
    let hash = "hash:11111111111111111111111111111111";
    let payload = format!(
        "{{\"shape\":\"intent_receipt\",\"target_id_hash\":\"{hash}\",\
          \"scope_tier\":\"private\",\
          \"reason_code\":\"please forget secret token ABC123\"}}"
    );
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-bad-reason', ?, 'private', 'GRANT', 'hmn:t', 0, \
                     'forget_intent', 'hmn:t', '2026-04-28T12:00:00Z', ?)",
            params![hash, payload],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("scalar out of domain class"),
        "free-text reason_code must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_payload_rejects_duplicate_top_level_keys() {
    // Round 9 hardening: SQLite `json_extract` returns the first
    // matching value, but serde rejects duplicate fields. A direct-SQL
    // payload with duplicate `reason_code` would brick the mirror.
    let conn = open_in_memory().expect("open");
    let hash = "hash:11111111111111111111111111111111";
    let payload = format!(
        "{{\"shape\":\"intent_receipt\",\"target_id_hash\":\"{hash}\",\
          \"scope_tier\":\"private\",\"reason_code\":\"user_command\",\
          \"reason_code\":\"another_one\"}}"
    );
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-dup', ?, 'private', 'GRANT', 'hmn:t', 0, \
                     'forget_intent', 'hmn:t', '2026-04-28T12:00:00Z', ?)",
            params![hash, payload],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("duplicate top-level keys"),
        "duplicate keys must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_subject_domain_enforced_for_non_hash_kinds() {
    // Round 9 hardening: top-level subject for policy_change / grant /
    // revoke must match the same closed character class the Rust
    // validator enforces. Without this, raw user text could ride
    // through `subject` into consent.log.
    let conn = open_in_memory().expect("open");
    // grant subject with spaces / uppercase / leak.
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-grant-leak', 'please share secret token ABC123', \
                     'private', 'GRANT', 'hmn:t', 0, 'grant', \
                     'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"decision\",\"subject_code\":\"share_link:abcd\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("subject out of domain class"),
        "free-text grant subject must be rejected, got: {err}"
    );
    // policy_change subject empty.
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-policy-empty', '', \
                     'global', 'GRANT', 'hmn:t', 0, 'policy_change', \
                     'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"policy_delta\",\"key\":\"sensors.x\",\
                       \"from_code\":\"a\",\"to_code\":\"b\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("subject out of domain class"),
        "empty policy_change subject must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_event_metadata_domain_enforced() {
    // Round 10 hardening: top-level `consent_id`, `scope`, and optional
    // `op_id` must match the closed character classes the Rust
    // `ConsentEvent::validate` enforces. Without this, raw user text
    // could ride through these audit columns into consent.log.
    let conn = open_in_memory().expect("open");

    // consent_id with spaces and free text — must be rejected.
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('please share secret token ABC123', 'sensor.x', \
                     'private', 'GRANT', 'hmn:t', 0, 'policy_change', \
                     'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"policy_delta\",\"key\":\"sensor.x\",\
                       \"from_code\":\"a\",\"to_code\":\"b\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("event metadata out of domain class"),
        "free-text consent_id must be rejected, got: {err}"
    );

    // scope with uppercase / spaces — must be rejected.
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-scope-bad', 'sensor.x', \
                     'Private Project Scope', 'GRANT', 'hmn:t', 0, \
                     'policy_change', 'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"policy_delta\",\"key\":\"sensor.x\",\
                       \"from_code\":\"a\",\"to_code\":\"b\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("event metadata out of domain class"),
        "free-text scope must be rejected, got: {err}"
    );

    // op_id with spaces — must be rejected.
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json, op_id) \
             VALUES ('c-op-bad', 'sensor.x', \
                     'private', 'GRANT', 'hmn:t', 0, 'policy_change', \
                     'hmn:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"policy_delta\",\"key\":\"sensor.x\",\
                       \"from_code\":\"a\",\"to_code\":\"b\"}', \
                     'op id with spaces')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("event metadata out of domain class"),
        "free-text op_id must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_sensor_id_domain_enforced() {
    // Round 10 hardening: `sensor_id` must match the `SensorLabel`
    // character class. The earlier sensor triggers only enforced
    // equality between sensor_id, payload.sensor_label, and subject —
    // a direct-SQL writer with consistent free text in all three would
    // pass equality but brick the mirror at SensorLabel::parse.
    let conn = open_in_memory().expect("open");

    // sensor_id with spaces — equality holds across columns but the
    // domain trigger must reject the row.
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, sensor_id, actor, decided_at_iso, payload_json) \
             VALUES ('c-sensor-spaces', 'snr:local hook host v1', \
                     'private', 'GRANT', 'hmn:t', 0, 'sensor_enable', \
                     'local hook host v1', 'hmn:t', \
                     '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"sensor_toggle\",\
                       \"sensor_label\":\"local hook host v1\",\
                       \"reason_code\":\"user_grant\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("sensor_id out of domain class"),
        "sensor_id with spaces must be rejected, got: {err}"
    );

    // sensor_id overlong (> 128 chars).
    let long = "a".repeat(129);
    let stmt = format!(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, sensor_id, actor, decided_at_iso, payload_json) \
         VALUES ('c-sensor-long', 'snr:{long}', \
                 'private', 'GRANT', 'hmn:t', 0, 'sensor_enable', \
                 '{long}', 'hmn:t', '2026-04-28T12:00:00Z', \
                 '{{\"shape\":\"sensor_toggle\",\
                    \"sensor_label\":\"{long}\",\
                    \"reason_code\":\"user_grant\"}}')",
    );
    let err = conn.execute(&stmt, []).unwrap_err();
    assert!(
        format!("{err}").contains("sensor_id out of domain class"),
        "overlong sensor_id must be rejected, got: {err}"
    );
}

#[test]
fn consent_journal_remains_append_only_under_0007() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, actor, decided_at_iso, payload_json) \
         VALUES ('c5', 's', 'private', 'GRANT', 'hmn:t', 0, 'grant', \
                 'hmn:t', '2026-04-28T12:00:00Z', \
                 '{\"shape\":\"decision\",\"subject_code\":\"x\"}')",
        [],
    )
    .expect("insert");

    let upd = conn
        .execute(
            "UPDATE consent_journal SET payload_json = '{}' WHERE consent_id = 'c5'",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{upd}").contains("immutable"),
        "UPDATE should still be blocked: {upd}"
    );

    let del = conn
        .execute("DELETE FROM consent_journal WHERE consent_id = 'c5'", [])
        .unwrap_err();
    assert!(
        format!("{del}").contains("append-only"),
        "DELETE should still be blocked: {del}"
    );
}

fn open_at_version(version: usize) -> rusqlite::Connection {
    let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory");
    migrations()
        .to_version(&mut conn, version)
        .expect("apply migrations to version");
    conn
}

#[test]
fn consent_journal_rebuild_preserves_rowid_and_backfills_revoke() {
    // Phase-B (#255, brief §14): the cairn-workflows consent.log materializer
    // tails consent_journal by rowid. Migration 0021 rebuilds the table to
    // promote `kind` to NOT NULL; the rebuild must preserve rowid 1:1 (so
    // existing mirror cursors are not silently invalidated) and must
    // backfill every event-shape field for legacy `kind IS NULL` rows so
    // that post-0021 readers can decode them fail-closed.
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('legacy-revoke', 'sub', 'private', 'REVOKE', 'hmn:t', 0)",
        [],
    )
    .expect("legacy insert");
    let rowid_before: i64 = conn
        .query_row(
            "SELECT rowid FROM consent_journal WHERE consent_id = 'legacy-revoke'",
            [],
            |r| r.get(0),
        )
        .expect("rowid before");

    migrations().to_version(&mut conn, 21).expect("apply 0021");

    let (rowid_after, kind_after, actor_after, payload_after, iso_after): (
        i64,
        String,
        String,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT rowid, kind, actor, payload_json, decided_at_iso \
             FROM consent_journal WHERE consent_id = 'legacy-revoke'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("row after");
    assert_eq!(
        rowid_before, rowid_after,
        "rebuild must preserve rowid 1:1 to keep mirror cursors valid"
    );
    assert_eq!(
        kind_after, "revoke",
        "REVOKE decision must backfill kind = 'revoke'"
    );
    assert_eq!(
        actor_after, "hmn:legacy",
        "legacy rows must get the fixed `hmn:legacy` sentinel actor — \
         pre-0009 `granted_by` had no domain check and is NOT trusted"
    );
    assert_eq!(
        payload_after, "{\"shape\":\"decision\",\"subject_code\":\"legacy\"}",
        "payload_json must be synthesized to body-free decision sentinel"
    );
    assert_eq!(
        iso_after, "1970-01-01T00:00:00Z",
        "decided_at_iso must be synthesized from decided_at unix-millis"
    );
}

#[test]
fn consent_journal_rebuild_backfills_grant_kind() {
    // Phase-B (#255, brief §14): symmetric coverage of the GRANT arm of the
    // 0021 backfill CASE/COALESCE plus the surrounding event-field
    // synthesis. Pinned separately so a future refactor that drops the
    // GRANT branch or weakens synthesis surfaces immediately.
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('legacy-grant', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("legacy insert");

    migrations().to_version(&mut conn, 21).expect("apply 0021");

    let (kind_after, actor_after, payload_after, iso_after): (String, String, String, String) =
        conn.query_row(
            "SELECT kind, actor, payload_json, decided_at_iso \
             FROM consent_journal WHERE consent_id = 'legacy-grant'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("row after");
    assert_eq!(
        kind_after, "grant",
        "GRANT decision must backfill kind = 'grant'"
    );
    assert_eq!(
        actor_after, "hmn:legacy",
        "legacy rows must get the fixed `hmn:legacy` sentinel actor — \
         pre-0009 `granted_by` had no domain check and is NOT trusted"
    );
    assert_eq!(
        payload_after, "{\"shape\":\"decision\",\"subject_code\":\"legacy\"}",
        "payload_json must be synthesized to body-free decision sentinel"
    );
    assert_eq!(
        iso_after, "1970-01-01T00:00:00Z",
        "decided_at_iso must be synthesized from decided_at unix-millis"
    );
}

#[test]
fn consent_journal_rebuild_synthesizes_legacy_event_fields_for_decode() {
    // Phase-B (#255, brief §14): the 0021 rebuild promotes legacy null-kind
    // rows into fully event-shaped rows so post-0021 readers can fail
    // closed on decode without needing a structural NULL filter that would
    // also hide future malformed rows. This test pins the end-to-end:
    // a legacy row inserted at v20 must, after 0021, be visible to the
    // mirror cursor AND decode cleanly into a `ConsentEvent` whose actor,
    // payload, and decided_at match the synthesis rules.
    use cairn_core::domain::{ConsentKind, ConsentPayload};
    use cairn_store_sqlite::consent::read_since_rowid;

    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, op_id) \
         VALUES ('legacy-grant', 'sub', 'private', 'GRANT', 'hmn:t', 0, 'op-legacy')",
        [],
    )
    .expect("legacy insert");

    migrations().to_version(&mut conn, 21).expect("apply 0021");

    let events = read_since_rowid(&conn, 0).expect("read_since_rowid");
    assert_eq!(
        events.len(),
        1,
        "synthesized legacy row must be visible to mirror cursor: {events:?}"
    );
    let (_rowid, event) = &events[0];
    assert_eq!(event.consent_id, "legacy-grant");
    assert_eq!(event.kind, ConsentKind::Grant);
    assert_eq!(
        event.actor.as_str(),
        "hmn:legacy",
        "decoded actor must match the fixed `hmn:legacy` sentinel — \
         legacy rows do NOT trust pre-0009 `granted_by` values"
    );
    assert_eq!(
        event.decided_at.as_str(),
        "1970-01-01T00:00:00Z",
        "decoded decided_at must match synthesized RFC3339 from unix-millis"
    );
    match &event.payload {
        ConsentPayload::Decision {
            subject_code,
            policy_code,
        } => {
            assert_eq!(subject_code, "legacy", "synthesized decision subject_code");
            assert!(
                policy_code.is_none(),
                "synthesized decision payload must omit policy_code"
            );
        }
        other => panic!("expected synthesized decision payload, got {other:?}"),
    }
}

#[test]
fn consent_journal_rebuild_aborts_on_nonpositive_rowid() {
    // Phase-B (#255, brief §14): round-4 High finding. An earlier draft
    // of 0021 silently renumbered legacy `rowid <= 0` rows by passing
    // `NULL` as the rowid in the rebuild SELECT, letting SQLite assign a
    // fresh rowid above MAX. That moved the offending row AFTER newer
    // events in the mirror's `WHERE rowid > cursor ORDER BY rowid` tail
    // — i.e. silently reordered history. The fix: a pre-rebuild SQL
    // assert ABORTs the migration so the operator can clean the row
    // manually before re-running. The migration transaction rolls back,
    // leaving the original schema intact.
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (rowid, consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES (0, 'rowid-zero', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("legacy insert at rowid=0");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on legacy rowid <= 0");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("rowid <= 0"),
        "abort message must mention `rowid <= 0` so operators can grep \
         the migration source for the cause; got: {msg}"
    );

    // Original row still present — rollback intact, no partial rebuild.
    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'rowid-zero'",
            [],
            |r| r.get(0),
        )
        .expect("legacy row must survive aborted migration");
    assert_eq!(consent_id, "rowid-zero");
}

#[test]
fn consent_journal_rebuild_aborts_on_unrenderable_decided_at() {
    // Phase-B (#255, brief §14): round-4 Medium finding. SQLite's
    // `strftime('%Y-%m-%dT%H:%M:%SZ', n, 'unixepoch')` returns NULL for
    // out-of-range UNIX seconds (e.g. UNIX millis past year 9999). An
    // earlier draft of 0021 used that strftime to synthesize
    // `decided_at_iso` for legacy rows; a NULL result would silently
    // produce a row with `kind != NULL` AND `decided_at_iso = NULL`,
    // which the event readers would surface (gating only on
    // `kind IS NOT NULL`) but decode would then fail on the missing iso
    // field, bricking the consent mirror. The fix: a pre-rebuild SQL
    // assert ABORTs the migration. Operator must clean the row first.
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('out-of-range', 'sub', 'private', 'GRANT', 'hmn:t', \
                 253402300800000000)",
        [],
    )
    .expect("legacy insert with out-of-range decided_at");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on unrenderable decided_at");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("cannot be rendered as RFC3339") || msg.contains("out-of-range UNIX millis"),
        "abort message must mention RFC3339 / out-of-range UNIX millis; got: {msg}"
    );

    // Original row still present — rollback intact, no partial rebuild.
    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'out-of-range'",
            [],
            |r| r.get(0),
        )
        .expect("legacy row must survive aborted migration");
    assert_eq!(consent_id, "out-of-range");
}

#[test]
fn consent_journal_rebuild_aborts_on_invalid_legacy_subject() {
    // Phase-B (#255, brief §14): round-6 High finding. Pre-0011 schema did
    // not enforce closed character classes on `subject`, so legacy rows can
    // carry free-form text. After the round-5 mirror cursor reset, the
    // mirror replays from rowid 0, so unsanitized values would reach the
    // on-disk consent.log via decode. Migration 0021 now FAILS CLOSED on
    // legacy rows whose subject violates the 0011 grant/revoke domain
    // (length 1..=128, [a-z0-9._:-], first char [a-z]).
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('bad-subject', 'Has Spaces And Caps', 'private', 'GRANT', \
                 'hmn:t', 0)",
        [],
    )
    .expect("legacy insert with out-of-domain subject");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on invalid legacy subject");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("0011 domain classes"),
        "abort message must cite the 0011 domain classes; got: {msg}"
    );

    // Original row still present — rollback intact, no partial rebuild.
    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'bad-subject'",
            [],
            |r| r.get(0),
        )
        .expect("legacy row must survive aborted migration");
    assert_eq!(consent_id, "bad-subject");
}

#[test]
fn consent_journal_rebuild_aborts_on_invalid_legacy_consent_id() {
    // Phase-B (#255, brief §14): round-6 High finding. Pre-0011 schema did
    // not enforce closed character classes on `consent_id`. The 0011 domain
    // is length 1..=64, [A-Za-z0-9._:-]. Free-form values like
    // `'has@badchars!'` must abort the migration.
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('has@badchars!', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("legacy insert with out-of-domain consent_id");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on invalid legacy consent_id");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("0011 domain classes"),
        "abort message must cite the 0011 domain classes; got: {msg}"
    );

    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal \
             WHERE consent_id = 'has@badchars!'",
            [],
            |r| r.get(0),
        )
        .expect("legacy row must survive aborted migration");
    assert_eq!(consent_id, "has@badchars!");
}

#[test]
fn consent_journal_rebuild_aborts_on_invalid_legacy_scope() {
    // Phase-B (#255, brief §14): round-6 High finding. Pre-0011 schema did
    // not enforce closed character classes on `scope`. The 0011 domain is
    // length 1..=256, [a-z0-9._:=,-]. Uppercase scope must abort.
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('bad-scope', 'sub', 'UPPERCASE', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("legacy insert with out-of-domain scope");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on invalid legacy scope");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("0011 domain classes"),
        "abort message must cite the 0011 domain classes; got: {msg}"
    );

    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'bad-scope'",
            [],
            |r| r.get(0),
        )
        .expect("legacy row must survive aborted migration");
    assert_eq!(consent_id, "bad-scope");
}

#[test]
fn consent_journal_rebuild_aborts_on_invalid_legacy_op_id() {
    // Phase-B (#255, brief §14): round-6 High finding. Pre-0011 schema did
    // not enforce closed character classes on `op_id`. The 0011 domain is
    // length 1..=128, [A-Za-z0-9._:-]. NULL op_id remains allowed (other
    // legacy tests already exercise NULL op_id paths); a non-NULL op_id
    // with spaces must abort.
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, op_id) \
         VALUES ('bad-op-id', 'sub', 'private', 'GRANT', 'hmn:t', 0, \
                 'has spaces')",
        [],
    )
    .expect("legacy insert with out-of-domain op_id");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on invalid legacy op_id");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("0011 domain classes"),
        "abort message must cite the 0011 domain classes; got: {msg}"
    );

    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'bad-op-id'",
            [],
            |r| r.get(0),
        )
        .expect("legacy row must survive aborted migration");
    assert_eq!(consent_id, "bad-op-id");
}

#[test]
fn consent_journal_rebuild_overrides_free_form_legacy_granted_by() {
    // Phase-B (#255, brief §14): regression for the round-3 High finding.
    // Pre-0009 `granted_by` had no domain CHECK — a historical row could
    // carry a free-form value like `'tafeng'` (no `hmn:` / `agt:` / `snr:`
    // prefix). An earlier version of 0021 used `COALESCE(actor,
    // granted_by)` which would have promoted that free-form value into
    // `actor`, then bricked decode at `Identity::parse(actor)` time.
    // The fix: legacy rows (kind IS NULL) get the fixed `'hmn:legacy'`
    // sentinel UNCONDITIONALLY, regardless of `granted_by`.
    use cairn_store_sqlite::consent::read_since_rowid;

    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('legacy-freeform', 'sub', 'private', 'GRANT', \
                 'tafeng', 0)",
        [],
    )
    .expect("legacy insert with free-form granted_by");

    migrations().to_version(&mut conn, 21).expect("apply 0021");

    let actor: String = conn
        .query_row(
            "SELECT actor FROM consent_journal WHERE consent_id = 'legacy-freeform'",
            [],
            |r| r.get(0),
        )
        .expect("actor after");
    assert_eq!(
        actor, "hmn:legacy",
        "legacy actor must be the fixed sentinel — the unsafe pre-0009 \
         `granted_by` value MUST NOT be promoted into `actor`"
    );

    // End-to-end: decode must succeed (Identity::parse accepts hmn:legacy).
    let events = read_since_rowid(&conn, 0).expect("read_since_rowid");
    let event = events
        .iter()
        .find(|(_, e)| e.consent_id == "legacy-freeform")
        .map(|(_, e)| e)
        .expect("legacy-freeform must decode");
    assert_eq!(event.actor.as_str(), "hmn:legacy");
}

#[test]
fn consent_journal_rebuild_aborts_on_event_row_missing_actor() {
    // Phase-B (#255, brief §14): round-7 High finding. Pre-0011 schema did
    // not require `actor` on event-kind rows — that NOT-NULL gate landed
    // in 0011 as a BEFORE INSERT trigger. A vault that wrote event-kind
    // rows under 0009/0010 could carry rows where `kind IS NOT NULL` but
    // `actor IS NULL`. Steps 0a/0b/0c only check `kind IS NULL` legacy
    // rows; without step 0d those rows would survive the rebuild and
    // brick `decode_event_inner`'s post-round-6 validate(). The fix:
    // step 0d FAILS CLOSED on any event-kind row missing actor/payload/
    // iso or with out-of-domain metadata. Drop the 0011 trigger first to
    // simulate the historical reality (v9: no trigger, row exists; v11:
    // trigger added, blocks new inserts but pre-existing row survives).
    let mut conn = open_at_version(20);
    conn.execute("DROP TRIGGER consent_journal_event_requires_actor", [])
        .expect("drop pre-0021 actor trigger to simulate v9-era write");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, actor, payload_json, decided_at_iso) \
         VALUES ('event-no-actor', 'sub', 'private', 'GRANT', 'hmn:t', 0, \
                 'grant', NULL, '{\"shape\":\"decision\",\"subject_code\":\"x\"}', \
                 '1970-01-01T00:00:00Z')",
        [],
    )
    .expect("legacy event-kind insert with NULL actor");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on event-kind row missing actor");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("event-kind row") || msg.contains("pre-0011 schema"),
        "abort message must cite event-kind / pre-0011 schema; got: {msg}"
    );

    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'event-no-actor'",
            [],
            |r| r.get(0),
        )
        .expect("event-kind row must survive aborted migration");
    assert_eq!(consent_id, "event-no-actor");
}

#[test]
fn consent_journal_rebuild_aborts_on_event_row_missing_payload() {
    // Phase-B (#255, brief §14): round-7 High finding. Same shape as the
    // missing-actor test, but for `payload_json`. The 0011 trigger
    // `consent_journal_event_requires_payload` blocks NULL/invalid
    // payload going forward — pre-0011 rows escape that gate.
    let mut conn = open_at_version(20);
    conn.execute("DROP TRIGGER consent_journal_event_requires_payload", [])
        .expect("drop pre-0021 payload trigger to simulate v9-era write");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, actor, payload_json, decided_at_iso) \
         VALUES ('event-no-payload', 'sub', 'private', 'GRANT', 'hmn:t', 0, \
                 'grant', 'hmn:t', NULL, '1970-01-01T00:00:00Z')",
        [],
    )
    .expect("legacy event-kind insert with NULL payload");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on event-kind row missing payload");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("event-kind row") || msg.contains("pre-0011 schema"),
        "abort message must cite event-kind / pre-0011 schema; got: {msg}"
    );

    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'event-no-payload'",
            [],
            |r| r.get(0),
        )
        .expect("event-kind row must survive aborted migration");
    assert_eq!(consent_id, "event-no-payload");
}

#[test]
fn consent_journal_rebuild_aborts_on_event_row_invalid_metadata() {
    // Phase-B (#255, brief §14): round-7 High finding. Pre-0011 schema
    // did not enforce closed character classes on consent_id / scope /
    // op_id for event-kind rows either; the 0011 trigger
    // `consent_journal_event_metadata_domains` blocks them going forward
    // but pre-existing rows survive. A `consent_id` containing `@` and
    // `!` violates the [A-Za-z0-9._:-] domain.
    let mut conn = open_at_version(20);
    conn.execute("DROP TRIGGER consent_journal_event_metadata_domains", [])
        .expect("drop pre-0021 metadata trigger to simulate v9-era write");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, actor, payload_json, decided_at_iso) \
         VALUES ('has@bad!chars', 'sub', 'private', 'GRANT', 'hmn:t', 0, \
                 'grant', 'hmn:t', '{\"shape\":\"decision\",\"subject_code\":\"x\"}', \
                 '1970-01-01T00:00:00Z')",
        [],
    )
    .expect("legacy event-kind insert with invalid consent_id");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on event-kind row with invalid metadata");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("event-kind row") || msg.contains("pre-0011 schema"),
        "abort message must cite event-kind / pre-0011 schema; got: {msg}"
    );

    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'has@bad!chars'",
            [],
            |r| r.get(0),
        )
        .expect("event-kind row must survive aborted migration");
    assert_eq!(consent_id, "has@bad!chars");
}

#[test]
fn consent_journal_rebuild_aborts_on_event_row_invalid_grant_subject() {
    // Phase-B (#255, brief §14): round-8 High finding. Pre-0011 schema did
    // not enforce the subject domain class on grant/revoke rows — the
    // `consent_journal_subject_domain_for_non_hash_kinds` trigger landed
    // in 0011. A pre-0011 row could carry a subject with uppercase /
    // spaces / other out-of-domain chars. Step 0d must FAIL CLOSED so
    // `ConsentEvent::validate()` does not subsequently fail at decode.
    let mut conn = open_at_version(20);
    conn.execute(
        "DROP TRIGGER consent_journal_subject_domain_for_non_hash_kinds",
        [],
    )
    .expect("drop pre-0021 subject-domain trigger to simulate v9-era write");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, actor, payload_json, decided_at_iso) \
         VALUES ('grant-bad-subject', 'Has Caps', 'private', 'GRANT', 'hmn:t', 0, \
                 'grant', 'hmn:t', '{\"shape\":\"decision\",\"subject_code\":\"x\"}', \
                 '1970-01-01T00:00:00Z')",
        [],
    )
    .expect("legacy event-kind insert with out-of-domain subject");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on event-kind row with bad grant subject");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("event-kind row")
            || msg.contains("subject/sensor_id")
            || msg.contains("pre-0011 schema"),
        "abort message must cite event-kind / subject/sensor_id / pre-0011 schema; got: {msg}"
    );

    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'grant-bad-subject'",
            [],
            |r| r.get(0),
        )
        .expect("event-kind row must survive aborted migration");
    assert_eq!(consent_id, "grant-bad-subject");
}

#[test]
fn consent_journal_rebuild_aborts_on_event_row_sensor_kind_missing_sensor_id() {
    // Phase-B (#255, brief §14): round-8 High finding. The 0011 trigger
    // `consent_journal_sensor_kind_requires_sensor_id` enforces that
    // sensor_enable/sensor_disable rows carry a non-NULL `sensor_id`.
    // Pre-0011 schema did not enforce this; step 0d must FAIL CLOSED.
    let mut conn = open_at_version(20);
    conn.execute(
        "DROP TRIGGER consent_journal_sensor_kind_requires_sensor_id",
        [],
    )
    .expect("drop pre-0021 sensor-requires-sensor_id trigger to simulate v9-era write");
    // Also drop the matching-payload + matching-subject triggers since the
    // payload here doesn't reference a sensor_id and the subject is a
    // hand-rolled string — those triggers would otherwise block the seed.
    conn.execute("DROP TRIGGER consent_journal_sensor_id_matches_payload", [])
        .ok();
    conn.execute(
        "DROP TRIGGER consent_journal_sensor_subject_matches_sensor_id",
        [],
    )
    .ok();
    conn.execute(
        "DROP TRIGGER consent_journal_payload_shape_matches_kind",
        [],
    )
    .ok();
    conn.execute("DROP TRIGGER consent_journal_payload_required_fields", [])
        .ok();
    conn.execute(
        "DROP TRIGGER consent_journal_subject_domain_for_non_hash_kinds",
        [],
    )
    .ok();
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, actor, payload_json, decided_at_iso, sensor_id) \
         VALUES ('sensor-no-id', 'snr:foo', 'private', 'GRANT', 'hmn:t', 0, \
                 'sensor_enable', 'hmn:t', \
                 '{\"shape\":\"sensor_toggle\",\"sensor_label\":\"foo\"}', \
                 '1970-01-01T00:00:00Z', NULL)",
        [],
    )
    .expect("legacy sensor-kind insert with NULL sensor_id");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on sensor row missing sensor_id");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("event-kind row")
            || msg.contains("subject/sensor_id")
            || msg.contains("pre-0011 schema"),
        "abort message must cite event-kind / subject/sensor_id / pre-0011 schema; got: {msg}"
    );

    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'sensor-no-id'",
            [],
            |r| r.get(0),
        )
        .expect("event-kind row must survive aborted migration");
    assert_eq!(consent_id, "sensor-no-id");
}

#[test]
fn consent_journal_rebuild_aborts_on_event_row_non_sensor_kind_with_sensor_id() {
    // Phase-B (#255, brief §14): round-8 High finding. The 0011 trigger
    // `consent_journal_non_sensor_kind_forbids_sensor_id` rejects any
    // non-sensor row that carries a `sensor_id`. Pre-0011 schema did not
    // enforce this; step 0d must FAIL CLOSED.
    let mut conn = open_at_version(20);
    conn.execute(
        "DROP TRIGGER consent_journal_non_sensor_kind_forbids_sensor_id",
        [],
    )
    .expect("drop pre-0021 forbids-sensor_id trigger to simulate v9-era write");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, actor, payload_json, decided_at_iso, sensor_id) \
         VALUES ('grant-with-sensor-id', 'sub', 'private', 'GRANT', 'hmn:t', 0, \
                 'grant', 'hmn:t', '{\"shape\":\"decision\",\"subject_code\":\"x\"}', \
                 '1970-01-01T00:00:00Z', 'snr:bar')",
        [],
    )
    .expect("legacy grant-kind insert with stray sensor_id");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on non-sensor row with sensor_id");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("event-kind row")
            || msg.contains("subject/sensor_id")
            || msg.contains("pre-0011 schema"),
        "abort message must cite event-kind / subject/sensor_id / pre-0011 schema; got: {msg}"
    );

    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'grant-with-sensor-id'",
            [],
            |r| r.get(0),
        )
        .expect("event-kind row must survive aborted migration");
    assert_eq!(consent_id, "grant-with-sensor-id");
}

#[test]
fn consent_journal_rebuild_aborts_on_event_row_invalid_hash_subject() {
    // Phase-B (#255, brief §14): round-8 High finding. Hash-kind rows
    // (forget_intent/remember_intent/promote_receipt) must carry a
    // subject of shape `sha256:<64hex>` or `hash:<32..=128 hex>`. The
    // 0011 trigger `consent_journal_hash_kind_subject_shape` enforces
    // this going forward; pre-0011 schema did not. Step 0d must FAIL
    // CLOSED on a hash-kind row whose subject does not match.
    let mut conn = open_at_version(20);
    conn.execute("DROP TRIGGER consent_journal_hash_kind_subject_shape", [])
        .expect("drop pre-0021 hash-subject trigger to simulate v9-era write");
    // The hash-kind payloads have their own shape; drop the related
    // payload-shape and subject-domain triggers so the seed insert lands.
    conn.execute(
        "DROP TRIGGER consent_journal_payload_shape_matches_kind",
        [],
    )
    .ok();
    conn.execute("DROP TRIGGER consent_journal_payload_required_fields", [])
        .ok();
    conn.execute(
        "DROP TRIGGER consent_journal_subject_domain_for_non_hash_kinds",
        [],
    )
    .ok();
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, actor, payload_json, decided_at_iso) \
         VALUES ('forget-bad-subj', 'not-a-hash', 'private', 'GRANT', 'hmn:t', 0, \
                 'forget_intent', 'hmn:t', \
                 '{\"shape\":\"intent_receipt\",\"target_id_hash\":\"sha256:0000000000000000000000000000000000000000000000000000000000000000\"}', \
                 '1970-01-01T00:00:00Z')",
        [],
    )
    .expect("legacy hash-kind insert with malformed subject");

    let err = migrations()
        .to_version(&mut conn, 21)
        .expect_err("migration 0021 must abort on hash-kind row with malformed subject");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("event-kind row")
            || msg.contains("subject/sensor_id")
            || msg.contains("pre-0011 schema"),
        "abort message must cite event-kind / subject/sensor_id / pre-0011 schema; got: {msg}"
    );

    let consent_id: String = conn
        .query_row(
            "SELECT consent_id FROM consent_journal WHERE consent_id = 'forget-bad-subj'",
            [],
            |r| r.get(0),
        )
        .expect("event-kind row must survive aborted migration");
    assert_eq!(consent_id, "forget-bad-subj");
}

#[test]
fn wal_ops_terminal_immutable() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO wal_ops (operation_id, issued_seq, kind, state, envelope, issuer, \
          target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
         VALUES ('op1', 1, 'upsert', 'ISSUED', '{}', 'i', 'h', '{}', 0, 'sig', 0, 0)",
        [],
    )
    .expect("insert wal_ops");
    conn.execute(
        "UPDATE wal_ops SET state = 'PREPARED' WHERE operation_id = 'op1'",
        [],
    )
    .expect("ISSUED -> PREPARED");
    conn.execute(
        "UPDATE wal_ops SET state = 'COMMITTED' WHERE operation_id = 'op1'",
        [],
    )
    .expect("PREPARED -> COMMITTED");
    let err = conn
        .execute(
            "UPDATE wal_ops SET reason = 'x' WHERE operation_id = 'op1'",
            [],
        )
        .unwrap_err();
    assert!(format!("{err}").contains("terminal-state"));
}
