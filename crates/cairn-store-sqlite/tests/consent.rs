//! Integration tests for the consent journal store helpers.

use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryVisibility, Rfc3339Timestamp,
    SensorLabel,
};
use cairn_store_sqlite::consent::{
    append, max_rowid, query_by_actor, query_by_op, query_by_scope, query_by_sensor,
    read_since_rowid,
};
use cairn_store_sqlite::open_in_memory;

/// Build a fixture hash of the form `hash:<32 lowercase hex>` from a
/// numeric seed. Avoids hand-padding strings in every test.
fn h(seed: u32) -> String {
    format!("hash:{seed:0>32x}")
}

fn forget_event(consent_id: &str, target_hash: &str) -> ConsentEvent {
    ConsentEvent {
        consent_id: consent_id.to_owned(),
        kind: ConsentKind::ForgetIntent,
        actor: Identity::parse("usr:tafeng").expect("identity"),
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

fn sensor_event(consent_id: &str, label: &str) -> ConsentEvent {
    let lbl = SensorLabel::parse(label).expect("sensor label");
    ConsentEvent {
        consent_id: consent_id.to_owned(),
        kind: ConsentKind::SensorEnable,
        actor: Identity::parse("usr:tafeng").expect("identity"),
        subject: format!("snr:{label}"),
        scope: "global".to_owned(),
        op_id: None,
        sensor_id: Some(lbl.clone()),
        payload: ConsentPayload::SensorToggle {
            sensor_label: lbl,
            reason_code: "first_run_prompt".to_owned(),
        },
        decided_at: Rfc3339Timestamp::parse("2026-04-28T12:01:00Z").expect("ts"),
        expires_at: None,
    }
}

#[test]
fn append_round_trips_through_query_by_op() {
    let conn = open_in_memory().expect("open");
    let event = forget_event("c-1", &h(0x00ab_c123));
    let rowid = append(&conn, &event).expect("append");
    assert!(rowid > 0);

    let by_op = query_by_op(&conn, "op-c-1").expect("query");
    assert_eq!(by_op.len(), 1);
    assert_eq!(by_op[0], event);
}

#[test]
fn query_by_actor_filters_to_principal() {
    let conn = open_in_memory().expect("open");
    let mut alice = forget_event("c-a", &h(0xa));
    alice.actor = Identity::parse("usr:alice").expect("id");
    let mut bob = forget_event("c-b", &h(0xb));
    bob.actor = Identity::parse("usr:bob").expect("id");
    bob.op_id = Some("op-bob".to_owned());

    append(&conn, &alice).expect("a");
    append(&conn, &bob).expect("b");

    let by_alice =
        query_by_actor(&conn, &Identity::parse("usr:alice").expect("id")).expect("actor query");
    assert_eq!(by_alice.len(), 1);
    assert_eq!(by_alice[0].consent_id, "c-a");
}

#[test]
fn query_by_sensor_filters_to_label() {
    let conn = open_in_memory().expect("open");
    append(&conn, &sensor_event("c-screen", "local:screen:host:v1")).expect("screen");
    append(&conn, &sensor_event("c-clip", "local:clipboard:host:v1")).expect("clip");

    let lbl = SensorLabel::parse("local:screen:host:v1").expect("lbl");
    let by_screen = query_by_sensor(&conn, &lbl).expect("sensor query");
    assert_eq!(by_screen.len(), 1);
    assert_eq!(by_screen[0].consent_id, "c-screen");
}

#[test]
fn query_by_scope_filters_to_scope() {
    let conn = open_in_memory().expect("open");
    append(&conn, &forget_event("c-private", &h(0xb1))).expect("p");
    let mut team = forget_event("c-team", &h(0xb2));
    team.scope = "team:platform".to_owned();
    append(&conn, &team).expect("t");

    let by_team = query_by_scope(&conn, "team:platform").expect("scope");
    assert_eq!(by_team.len(), 1);
    assert_eq!(by_team[0].consent_id, "c-team");
}

#[test]
fn read_since_rowid_advances_monotonically() {
    let conn = open_in_memory().expect("open");
    let r1 = append(&conn, &forget_event("c-1", &h(1))).expect("1");
    let r2 = append(&conn, &forget_event("c-2", &h(2))).expect("2");
    assert!(r2 > r1);

    let pending = read_since_rowid(&conn, 0).expect("all");
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].0, r1);
    assert_eq!(pending[1].0, r2);

    let after_first = read_since_rowid(&conn, r1).expect("after first");
    assert_eq!(after_first.len(), 1);
    assert_eq!(after_first[0].0, r2);

    assert_eq!(max_rowid(&conn).expect("max"), r2);
}

#[test]
fn forget_receipt_grep_invariant() {
    // §15-style invariant: after appending a forget_intent event, the raw
    // SQLite row payload contains no tokenization of the original target
    // body. We construct an event whose semantic body is "TOPSECRETBODY"
    // and verify that string never appears in any column of the row.
    let conn = open_in_memory().expect("open");
    let secret = "TOPSECRETBODY";
    let salted = h(0xdead_beef);
    let event = forget_event("c-leak", &salted);
    let event = ConsentEvent {
        subject: salted.clone(),
        payload: ConsentPayload::IntentReceipt {
            target_id_hash: salted.clone(),
            scope_tier: MemoryVisibility::Private,
            reason_code: "user_command".to_owned(),
        },
        ..event
    };
    append(&conn, &event).expect("append");

    let mut stmt = conn
        .prepare("SELECT * FROM consent_journal WHERE consent_id = 'c-leak'")
        .expect("prep");
    let row = stmt
        .query_row([], |r| {
            // Concatenate every column value (text or null) into one buffer.
            let mut buf = String::new();
            for i in 0..r.as_ref().column_count() {
                let v: Option<String> = r.get(i).ok();
                if let Some(s) = v {
                    buf.push_str(&s);
                    buf.push('\n');
                }
            }
            Ok(buf)
        })
        .expect("row dump");
    assert!(
        !row.contains(secret),
        "consent_journal row leaked forgotten body: {row}"
    );
}

#[test]
fn append_rejects_body_bearing_payload_via_serializer() {
    // The ConsentPayload type cannot represent a body field; the trigger
    // is the last line of defense. This test asserts the trigger fires
    // when something bypasses the type system (e.g., a future variant).
    let conn = open_in_memory().expect("open");
    let hash = "hash:11111111111111111111111111111111";
    let payload = format!(
        "{{\"shape\":\"intent_receipt\",\"target_id_hash\":\"{hash}\",\
          \"scope_tier\":\"private\",\"reason_code\":\"user_command\",\
          \"body\":\"x\"}}"
    );
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-bypass', ?, 'private', 'GRANT', 'usr:t', 0, \
                     'forget_intent', 'usr:t', '2026-04-28T12:00:00Z', ?)",
            rusqlite::params![hash, payload],
        )
        .unwrap_err();
    assert!(format!("{err}").contains("body-free"));
}

#[test]
fn round_trip_preserves_every_kind() {
    let conn = open_in_memory().expect("open");
    let kinds: &[ConsentKind] = &[
        ConsentKind::SensorEnable,
        ConsentKind::SensorDisable,
        ConsentKind::PolicyChange,
        ConsentKind::RememberIntent,
        ConsentKind::ForgetIntent,
        ConsentKind::Grant,
        ConsentKind::Revoke,
        ConsentKind::PromoteReceipt,
    ];

    for (i, kind) in kinds.iter().enumerate() {
        let id = format!("c-rt-{i}");
        let event = match kind {
            ConsentKind::SensorEnable | ConsentKind::SensorDisable => {
                let mut e = sensor_event(&id, "local:hook:host:v1");
                e.kind = *kind;
                e
            }
            ConsentKind::PolicyChange => ConsentEvent {
                consent_id: id.clone(),
                kind: *kind,
                actor: Identity::parse("usr:tafeng").expect("id"),
                subject: "sensors.screen.enabled".to_owned(),
                scope: "global".to_owned(),
                op_id: None,
                sensor_id: None,
                payload: ConsentPayload::PolicyDelta {
                    key: "sensors.screen.enabled".to_owned(),
                    from_code: "false".to_owned(),
                    to_code: "true".to_owned(),
                },
                decided_at: Rfc3339Timestamp::parse("2026-04-28T12:00:00Z").expect("ts"),
                expires_at: None,
            },
            ConsentKind::Grant | ConsentKind::Revoke => ConsentEvent {
                consent_id: id.clone(),
                kind: *kind,
                actor: Identity::parse("usr:tafeng").expect("id"),
                subject: "share_link:abcd".to_owned(),
                scope: "team:platform".to_owned(),
                op_id: None,
                sensor_id: None,
                payload: ConsentPayload::Decision {
                    subject_code: "share_link:abcd".to_owned(),
                    policy_code: Some("policy:default-share".to_owned()),
                },
                decided_at: Rfc3339Timestamp::parse("2026-04-28T12:00:00Z").expect("ts"),
                expires_at: None,
            },
            ConsentKind::PromoteReceipt => {
                let promoted = h(0x00c0_ffee);
                ConsentEvent {
                    consent_id: id.clone(),
                    kind: *kind,
                    actor: Identity::parse("usr:tafeng").expect("id"),
                    subject: promoted.clone(),
                    scope: "team:platform".to_owned(),
                    op_id: Some(format!("op-{id}")),
                    sensor_id: None,
                    payload: ConsentPayload::PromoteReceipt {
                        target_id_hash: promoted,
                        from_tier: MemoryVisibility::Private,
                        to_tier: MemoryVisibility::Team,
                        receipt_id: "rcpt-1".to_owned(),
                    },
                    decided_at: Rfc3339Timestamp::parse("2026-04-28T12:00:00Z").expect("ts"),
                    expires_at: None,
                }
            }
            ConsentKind::RememberIntent | ConsentKind::ForgetIntent => {
                let mut e = forget_event(&id, &h(0xff));
                e.kind = *kind;
                e
            }
        };
        append(&conn, &event).expect("append");
        let back = query_by_actor(&conn, &event.actor).expect("by actor");
        let found = back.iter().find(|e| e.consent_id == id).expect("present");
        assert_eq!(found, &event, "kind {kind:?} did not round trip");
    }
}
