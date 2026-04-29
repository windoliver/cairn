//! Consent journal event types (brief §14, issue #94).
//!
//! These are the body-free events the `SQLite` `consent_journal` row carries
//! and the async `.cairn/consent.log` materializer mirrors. They describe
//! sensor enablement, policy changes, explicit remember/forget intent, and
//! grant/revoke decisions — never the underlying user content. The payload
//! shape is constrained per `ConsentKind` so a forget receipt cannot
//! accidentally retain the body of the record it forgot.
//!
//! This module is **pure data** — no I/O, no async. The store crate maps
//! these onto `consent_journal` rows; the workflow crate's materializer
//! tails the table onto the on-disk log.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{Identity, MemoryVisibility, Rfc3339Timestamp, SensorLabel};

/// Allowed kinds of consent journal event. Mirrors the `kind` CHECK
/// trigger in `crates/cairn-store-sqlite/src/migrations/sql/0007_consent_event.sql`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentKind {
    /// Sensor was enabled (first run prompt accepted, or operator toggle).
    SensorEnable,
    /// Sensor was disabled.
    SensorDisable,
    /// `.cairn/config.yaml` policy field changed (visibility default,
    /// retention, redaction policy …).
    PolicyChange,
    /// Explicit user "remember this" intent attached to a write.
    RememberIntent,
    /// Explicit user "forget this" intent — produces a body-free receipt.
    ForgetIntent,
    /// Generic GRANT decision (0005-style consent grant).
    Grant,
    /// Generic REVOKE decision (0005-style consent revoke).
    Revoke,
    /// Promotion across visibility tiers (P2 surface, journal row P0).
    PromoteReceipt,
}

/// Body-free metadata payload. Each variant is constrained so the union
/// of all serialized field names stays inside the §14 audit allowlist —
/// `body`, `text`, `content`, `raw`, `snippet`, `command`, `url`, `title`,
/// `file_path`, `input` are never emitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConsentPayload {
    /// Sensor toggle — names the sensor and the user-visible reason code.
    SensorToggle {
        /// Sensor identifier; matches the `sensor_id` column.
        sensor_label: SensorLabel,
        /// Short reason code (`"first_run_prompt"`, `"operator_off"`, …).
        /// Never raw user input.
        reason_code: String,
    },
    /// Policy field change — names the config key and previous/next codes.
    PolicyDelta {
        /// Dotted config key (`"sensors.screen.enabled"`, …).
        key: String,
        /// Previous value, rendered as a short symbolic code, not raw text.
        from_code: String,
        /// New value, rendered as a short symbolic code.
        to_code: String,
    },
    /// `remember`/`forget` user intent at a target id.
    IntentReceipt {
        /// Salted hash of the target id — never the target id itself when
        /// the kind is `forget_intent`.
        target_id_hash: String,
        /// Visibility tier the write was scoped under.
        scope_tier: MemoryVisibility,
        /// Short reason code (`"user_command"`, `"workflow_expire"`, …).
        reason_code: String,
    },
    /// GRANT / REVOKE decision attached to a subject.
    Decision {
        /// Symbolic decision target (e.g. `"share_link:abcd"`).
        subject_code: String,
        /// Optional short policy reference, never the policy text itself.
        policy_code: Option<String>,
    },
    /// Promotion receipt — links the receipt id, never the promoted record.
    PromoteReceipt {
        /// Hash of the target id being promoted.
        target_id_hash: String,
        /// Visibility tier the record is moving from.
        from_tier: MemoryVisibility,
        /// Visibility tier the record is moving to.
        to_tier: MemoryVisibility,
        /// Cryptographic receipt id (signature ref). Verified upstream.
        receipt_id: String,
    },
}

/// One row of the consent journal — what the store persists, and what the
/// async materializer mirrors line-by-line into `.cairn/consent.log`.
///
/// **Body-free by construction.** The struct exposes only metadata,
/// hashes, and short reason codes. A forget receipt that tried to carry
/// the body of the forgotten record would fail to deserialize: every
/// payload variant is `deny_unknown_fields`, and the test
/// `forbids_body_bearing_field_names_anywhere` enumerates the field set
/// the json form may emit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentEvent {
    /// Stable id (ULID-shaped). Primary key in `consent_journal`.
    pub consent_id: String,
    /// What kind of event this row records.
    pub kind: ConsentKind,
    /// Principal that authored the event (`usr:…` / `agt:…`). The store
    /// indexes this as the `actor` column for identity-keyed queries.
    pub actor: Identity,
    /// Subject of the event — meaning is `kind`-specific (sensor label,
    /// config key, target id hash, share link id …). Indexed as
    /// `consent_journal.subject`.
    pub subject: String,
    /// Scope tuple in canonical wire form. Used by `query_by_scope`.
    pub scope: String,
    /// `wal_ops.operation_id` this event was committed under (when the
    /// event corresponds to a WAL transition). `None` for ambient events
    /// like first-run sensor enablement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_id: Option<String>,
    /// Sensor identifier when `kind ∈ {sensor_enable, sensor_disable}`.
    /// `None` for non-sensor events. Indexed for sensor-keyed queries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor_id: Option<SensorLabel>,
    /// Body-free, kind-specific payload.
    pub payload: ConsentPayload,
    /// When the event was decided.
    pub decided_at: Rfc3339Timestamp,
    /// Optional TTL (e.g., for short-lived grants).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Rfc3339Timestamp>,
}

impl ConsentEvent {
    /// JSON field names the wire form is permitted to emit. Used by the
    /// banned-field test below to keep the body-free invariant explicit.
    pub const ALLOWED_TOP_LEVEL_FIELDS: &'static [&'static str] = &[
        "consent_id",
        "kind",
        "actor",
        "subject",
        "scope",
        "op_id",
        "sensor_id",
        "payload",
        "decided_at",
        "expires_at",
    ];

    /// JSON field names any nested payload variant is permitted to emit.
    pub const ALLOWED_PAYLOAD_FIELDS: &'static [&'static str] = &[
        "shape",
        "sensor_label",
        "reason_code",
        "key",
        "from_code",
        "to_code",
        "target_id_hash",
        "scope_tier",
        "subject_code",
        "policy_code",
        "from_tier",
        "to_tier",
        "receipt_id",
    ];

    /// Field names that must never appear anywhere in the wire form —
    /// they would imply user content is being persisted into the journal.
    pub const BANNED_FIELDS: &'static [&'static str] = &[
        "body",
        "text",
        "content",
        "raw",
        "snippet",
        "command",
        "url",
        "title",
        "file_path",
        "input",
        "payload_text",
        "user_input",
        "message",
    ];
}

/// Walk a `serde_json::Value` and return every key name encountered.
#[must_use]
pub fn collect_keys(value: &serde_json::Value) -> BTreeMap<String, u32> {
    let mut acc = BTreeMap::new();
    walk(value, &mut acc);
    acc
}

fn walk(value: &serde_json::Value, acc: &mut BTreeMap<String, u32>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                *acc.entry(k.clone()).or_insert(0) += 1;
                walk(v, acc);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                walk(v, acc);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_forget() -> ConsentEvent {
        ConsentEvent {
            consent_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            kind: ConsentKind::ForgetIntent,
            actor: Identity::parse("usr:tafeng").expect("valid identity"),
            subject: "hash:abc123".to_owned(),
            scope: "private:agent=agt:claude-code".to_owned(),
            op_id: Some("op-01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()),
            sensor_id: None,
            payload: ConsentPayload::IntentReceipt {
                target_id_hash: "hash:abc123".to_owned(),
                scope_tier: MemoryVisibility::Private,
                reason_code: "user_command".to_owned(),
            },
            decided_at: Rfc3339Timestamp::parse("2026-04-28T12:00:00Z").expect("valid ts"),
            expires_at: None,
        }
    }

    fn sample_sensor_enable() -> ConsentEvent {
        ConsentEvent {
            consent_id: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned(),
            kind: ConsentKind::SensorEnable,
            actor: Identity::parse("usr:tafeng").expect("valid identity"),
            subject: "snr:local:screen:host:v1".to_owned(),
            scope: "global".to_owned(),
            op_id: None,
            sensor_id: Some(SensorLabel::parse("local:screen:host:v1").expect("valid sensor")),
            payload: ConsentPayload::SensorToggle {
                sensor_label: SensorLabel::parse("local:screen:host:v1").expect("valid sensor"),
                reason_code: "first_run_prompt".to_owned(),
            },
            decided_at: Rfc3339Timestamp::parse("2026-04-28T12:01:00Z").expect("valid ts"),
            expires_at: None,
        }
    }

    fn sample_promote() -> ConsentEvent {
        ConsentEvent {
            consent_id: "01ARZ3NDEKTSV4RRFFQ69G5FAX".to_owned(),
            kind: ConsentKind::PromoteReceipt,
            actor: Identity::parse("usr:tafeng").expect("valid identity"),
            subject: "hash:def456".to_owned(),
            scope: "team:platform".to_owned(),
            op_id: Some("op-01ARZ3NDEKTSV4RRFFQ69G5FAX".to_owned()),
            sensor_id: None,
            payload: ConsentPayload::PromoteReceipt {
                target_id_hash: "hash:def456".to_owned(),
                from_tier: MemoryVisibility::Private,
                to_tier: MemoryVisibility::Team,
                receipt_id: "rcpt-01ARZ3NDEKTSV4RRFFQ69G5FAX".to_owned(),
            },
            decided_at: Rfc3339Timestamp::parse("2026-04-28T12:02:00Z").expect("valid ts"),
            expires_at: None,
        }
    }

    #[test]
    fn round_trips_forget_through_json() {
        let event = sample_forget();
        let s = serde_json::to_string(&event).expect("ser");
        let back: ConsentEvent = serde_json::from_str(&s).expect("de");
        assert_eq!(back, event);
    }

    #[test]
    fn round_trips_sensor_enable_through_json() {
        let event = sample_sensor_enable();
        let s = serde_json::to_string(&event).expect("ser");
        let back: ConsentEvent = serde_json::from_str(&s).expect("de");
        assert_eq!(back, event);
    }

    #[test]
    fn round_trips_promote_through_json() {
        let event = sample_promote();
        let s = serde_json::to_string(&event).expect("ser");
        let back: ConsentEvent = serde_json::from_str(&s).expect("de");
        assert_eq!(back, event);
    }

    #[test]
    fn forbids_body_bearing_field_names_anywhere() {
        for sample in [sample_forget(), sample_sensor_enable(), sample_promote()] {
            let v = serde_json::to_value(&sample).expect("ser");
            let keys = collect_keys(&v);
            for banned in ConsentEvent::BANNED_FIELDS {
                assert!(
                    !keys.contains_key(*banned),
                    "banned body-bearing field {banned:?} found in {keys:?}",
                );
            }
            for key in keys.keys() {
                let allowed = ConsentEvent::ALLOWED_TOP_LEVEL_FIELDS.contains(&key.as_str())
                    || ConsentEvent::ALLOWED_PAYLOAD_FIELDS.contains(&key.as_str());
                assert!(allowed, "unexpected field {key:?}");
            }
        }
    }

    #[test]
    fn deny_unknown_top_level_field() {
        let bad = r#"{
            "consent_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "kind":"forget_intent",
            "actor":"usr:tafeng",
            "subject":"hash:abc",
            "scope":"private",
            "payload":{"shape":"intent_receipt","target_id_hash":"hash:abc",
                       "scope_tier":"private","reason_code":"user_command"},
            "decided_at":"2026-04-28T12:00:00Z",
            "body":"this should not be accepted"
        }"#;
        assert!(serde_json::from_str::<ConsentEvent>(bad).is_err());
    }

    #[test]
    fn deny_unknown_payload_field() {
        let bad = r#"{
            "consent_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "kind":"forget_intent",
            "actor":"usr:tafeng",
            "subject":"hash:abc",
            "scope":"private",
            "payload":{"shape":"intent_receipt","target_id_hash":"hash:abc",
                       "scope_tier":"private","reason_code":"user_command",
                       "raw":"forbidden"},
            "decided_at":"2026-04-28T12:00:00Z"
        }"#;
        assert!(serde_json::from_str::<ConsentEvent>(bad).is_err());
    }

    #[test]
    fn forget_intent_payload_is_hash_only() {
        // Construction-time guarantee: the payload's field set, by type,
        // cannot include the original target id text. The hash is the
        // only way to reference the forgotten record.
        let event = sample_forget();
        let v = serde_json::to_value(&event).expect("ser");
        let keys = collect_keys(&v);
        assert!(keys.contains_key("target_id_hash"));
        assert!(!keys.contains_key("target_id"));
    }

    #[test]
    fn sensor_event_carries_sensor_id() {
        let event = sample_sensor_enable();
        assert!(event.sensor_id.is_some());
        let v = serde_json::to_value(&event).expect("ser");
        let keys = collect_keys(&v);
        assert!(keys.contains_key("sensor_id"));
    }

    #[test]
    fn op_id_omitted_when_none() {
        let mut event = sample_sensor_enable();
        event.op_id = None;
        let s = serde_json::to_string(&event).expect("ser");
        assert!(!s.contains("\"op_id\""), "{s}");
    }
}
