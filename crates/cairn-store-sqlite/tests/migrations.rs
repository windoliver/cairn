//! Integration tests for the migration set.

use cairn_store_sqlite::{migrations::migrations, open, open_in_memory};
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
    assert_eq!(head, 8);
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
    assert_eq!(head, 8);
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
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c1', 's1', 'private', 'GRANT', 'usr:t', 0, \
                     'not_a_kind', 'usr:t', '2026-04-28T12:00:00Z', \
                     '{\"shape\":\"decision\",\"subject_code\":\"x\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("kind not in §14 domain"),
        "kind CHECK should fire, got: {err}"
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
             VALUES (?, ?, 'private', 'GRANT', 'usr:t', 0, ?, '2026-04-28T12:00:00Z', \
                     'usr:t', ?, ?)",
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
             VALUES ('c-no-iso', 's', 'private', 'GRANT', 'usr:t', 0, 'grant', \
                     'usr:t', '{\"shape\":\"decision\",\"subject_code\":\"x\"}')",
            [],
        )
        .unwrap_err();
    assert!(
        format!("{err}").contains("decided_at_iso"),
        "iso requirement should fire, got: {err}"
    );
}

#[test]
fn consent_journal_kind_null_back_compat() {
    // Rows written before 0007 have kind = NULL. Inserting one should
    // still succeed — the trigger only fires when kind IS NOT NULL.
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('legacy', 's', 'private', 'GRANT', 'usr:t', 0)",
        [],
    )
    .expect("legacy insert with NULL kind");
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
                     'private', 'GRANT', 'usr:t', 0, 'forget_intent', \
                     'usr:t', '2026-04-28T12:00:00Z', \
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
                 VALUES (?, ?, 'private', 'GRANT', 'usr:t', 0, 'forget_intent', \
                         'usr:t', '2026-04-28T12:00:00Z', ?)",
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
                     'private', 'GRANT', 'usr:t', 0, 'forget_intent', \
                     'usr:t', \
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
                     'team:p', 'GRANT', 'usr:t', 0, \
                     'promote_receipt', 'usr:t', '2026-04-28T12:00:00Z', \
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
                 'private', 'GRANT', 'usr:t', 0, 'forget_intent', \
                 'usr:t', '2026-04-28T12:00:00Z', \
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
         VALUES ('c4', 'snr:local:screen:host:v1', 'global', 'GRANT', 'usr:t', 0, \
                 'sensor_enable', '2026-04-28T12:00:00Z', \
                 'op-1', 'usr:tafeng', 'local:screen:host:v1', \
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
            "SELECT COUNT(*) FROM consent_journal WHERE actor = 'usr:tafeng'",
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
                     'global', 'GRANT', 'usr:t', 0, 'policy_change', \
                     'usr:t', '2026-04-28T12:00:00Z', '{}')",
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
                     'usr:t', 0, 'sensor_enable', 'usr:t', '2026-04-28T12:00:00Z', \
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
                     'usr:t', 0, 'sensor_enable', 'usr:t', '2026-04-28T12:00:00Z', \
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
                     'usr:t', 0, 'policy_change', 'usr:t', '2026-04-28T12:00:00Z', \
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
                     'usr:t', 0, 'sensor_enable', 'usr:t', '2026-04-28T12:00:00Z', \
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
                     'usr:t', 0, 'forget_intent', 'usr:t', '2026-04-28T12:00:00Z', \
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
                     'private', 'GRANT', 'usr:t', 0, 'forget_intent', \
                     'usr:t', '2026-04-28T12:00:00Z', \
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
                     'usr:t', 0, 'sensor_enable', 'usr:t', '2026-04-28T12:00:00Z', \
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
                     'usr:t', 0, 'sensor_enable', 'usr:t', '2026-04-28T12:00:00Z', \
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
                 VALUES (?, ?, 'private', 'GRANT', 'usr:t', 0, 'forget_intent', \
                         'usr:t', '2026-04-28T12:00:00Z', ?)",
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
        let err = conn
            .execute(
                "INSERT INTO consent_journal \
                  (consent_id, subject, scope, decision, granted_by, decided_at, \
                   kind, actor, decided_at_iso, sensor_id, payload_json) \
                 VALUES (?, ?, 'private', 'GRANT', 'usr:t', 0, ?, 'usr:t', \
                         '2026-04-28T12:00:00Z', ?, ?)",
                params![format!("c-{desc}"), subject, kind, sensor_id, payload],
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
             VALUES ('c-bad-tier', ?, 'private', 'GRANT', 'usr:t', 0, \
                     'forget_intent', 'usr:t', '2026-04-28T12:00:00Z', ?)",
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
             VALUES ('c-unknown-key', ?, 'private', 'GRANT', 'usr:t', 0, \
                     'forget_intent', 'usr:t', '2026-04-28T12:00:00Z', ?)",
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
                     'usr:t', 0, 'grant', 'usr:t', '2026-04-28T12:00:00Z', ?)",
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
                 VALUES (?, ?, ?, 'private', 'GRANT', 'usr:t', 0, \
                         'forget_intent', 'usr:t', '2026-04-28T12:00:00Z', ?)",
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
             VALUES ('c-cross-key', ?, 'private', 'GRANT', 'usr:t', 0, \
                     'forget_intent', 'usr:t', '2026-04-28T12:00:00Z', ?)",
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
             VALUES ('c-bad-reason', ?, 'private', 'GRANT', 'usr:t', 0, \
                     'forget_intent', 'usr:t', '2026-04-28T12:00:00Z', ?)",
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
             VALUES ('c-dup', ?, 'private', 'GRANT', 'usr:t', 0, \
                     'forget_intent', 'usr:t', '2026-04-28T12:00:00Z', ?)",
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
                     'private', 'GRANT', 'usr:t', 0, 'grant', \
                     'usr:t', '2026-04-28T12:00:00Z', \
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
                     'global', 'GRANT', 'usr:t', 0, 'policy_change', \
                     'usr:t', '2026-04-28T12:00:00Z', \
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
fn consent_journal_remains_append_only_under_0007() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, \
           kind, actor, decided_at_iso, payload_json) \
         VALUES ('c5', 's', 'private', 'GRANT', 'usr:t', 0, 'grant', \
                 'usr:t', '2026-04-28T12:00:00Z', \
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
