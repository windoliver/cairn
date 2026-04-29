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
use thiserror::Error;

use crate::domain::{Identity, MemoryVisibility, Rfc3339Timestamp, SensorLabel};

/// Validation failures for [`ConsentEvent::validate`]. The store helpers
/// promote these into `StoreError` so a misconstructed event cannot reach
/// the journal.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConsentEventError {
    /// `payload` does not match the variant required by `kind`.
    #[error("kind {kind:?} requires {expected} payload, got {actual}")]
    KindPayloadMismatch {
        /// Event kind.
        kind: ConsentKind,
        /// Variant the kind required.
        expected: &'static str,
        /// Variant the caller supplied.
        actual: &'static str,
    },
    /// A field that must hold a salted hash carries an unrecognized shape.
    #[error("hash field {field} has malformed value: {message}")]
    InvalidHash {
        /// Which field failed (`subject`, `target_id_hash`, …).
        field: &'static str,
        /// Detail about why it failed.
        message: String,
    },
    /// A reason / policy / config code is out of class.
    #[error("code field {field} has malformed value: {message}")]
    InvalidCode {
        /// Which field failed (`reason_code`, `key`, …).
        field: &'static str,
        /// Detail about why it failed.
        message: String,
    },
    /// A receipt-only field is missing or wrongly shaped.
    #[error("receipt field {field} has malformed value: {message}")]
    InvalidReceipt {
        /// Which field failed (`receipt_id`, …).
        field: &'static str,
        /// Detail about why it failed.
        message: String,
    },
}

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
    /// Mirrored by the `SQLite` trigger
    /// `consent_journal_forget_receipt_body_free` in migration 0007.
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

    /// Validate kind/payload pairing, hash shapes, and reason codes.
    /// Called by `cairn_store_sqlite::consent::append` before persisting,
    /// so a misconstructed event cannot reach `consent_journal` or the
    /// `.cairn/consent.log` mirror — even though the struct fields are
    /// public. Pure function; safe to call from any context.
    ///
    /// # Errors
    /// Returns [`ConsentEventError`] when the kind/payload pair is wrong,
    /// or when a hash / code / receipt field is malformed.
    pub fn validate(&self) -> Result<(), ConsentEventError> {
        validate_consent_id(&self.consent_id)?;
        validate_scope(&self.scope)?;
        if let Some(op_id) = &self.op_id {
            validate_op_id(op_id)?;
        }
        validate_sensor_id_presence(self.kind, self.sensor_id.as_ref())?;
        validate_payload_for_kind(self)?;
        Ok(())
    }
}

const fn expected_payload_variant(kind: ConsentKind) -> &'static str {
    match kind {
        ConsentKind::SensorEnable | ConsentKind::SensorDisable => "sensor_toggle",
        ConsentKind::PolicyChange => "policy_delta",
        ConsentKind::RememberIntent | ConsentKind::ForgetIntent => "intent_receipt",
        ConsentKind::Grant | ConsentKind::Revoke => "decision",
        ConsentKind::PromoteReceipt => "promote_receipt",
    }
}

const fn payload_variant_name(payload: &ConsentPayload) -> &'static str {
    match payload {
        ConsentPayload::SensorToggle { .. } => "sensor_toggle",
        ConsentPayload::PolicyDelta { .. } => "policy_delta",
        ConsentPayload::IntentReceipt { .. } => "intent_receipt",
        ConsentPayload::Decision { .. } => "decision",
        ConsentPayload::PromoteReceipt { .. } => "promote_receipt",
    }
}

/// Sensor-id top-level field is required for sensor kinds and forbidden
/// for every other kind. Without this, sensor events could be missed by
/// `query_by_sensor` (`sensor_id` NULL) or non-sensor events could pollute
/// it (`sensor_id` `Some` on a `policy_change`).
fn validate_sensor_id_presence(
    kind: ConsentKind,
    sensor_id: Option<&SensorLabel>,
) -> Result<(), ConsentEventError> {
    let is_sensor_kind = matches!(kind, ConsentKind::SensorEnable | ConsentKind::SensorDisable);
    match (is_sensor_kind, sensor_id) {
        (true, Some(_)) | (false, None) => Ok(()),
        (true, None) => Err(ConsentEventError::InvalidCode {
            field: "sensor_id",
            message: "sensor kinds require sensor_id".to_owned(),
        }),
        (false, Some(_)) => Err(ConsentEventError::InvalidCode {
            field: "sensor_id",
            message: "non-sensor kinds must not carry sensor_id".to_owned(),
        }),
    }
}

fn validate_payload_for_kind(event: &ConsentEvent) -> Result<(), ConsentEventError> {
    match (event.kind, &event.payload) {
        (
            ConsentKind::SensorEnable | ConsentKind::SensorDisable,
            ConsentPayload::SensorToggle {
                reason_code,
                sensor_label,
            },
        ) => validate_sensor_payload(event, reason_code, sensor_label),
        (
            ConsentKind::PolicyChange,
            ConsentPayload::PolicyDelta {
                key,
                from_code,
                to_code,
            },
        ) => {
            validate_dotted_key("key", key)?;
            validate_code("from_code", from_code)?;
            validate_code("to_code", to_code)?;
            validate_dotted_key("subject", &event.subject)
        }
        (
            ConsentKind::RememberIntent | ConsentKind::ForgetIntent,
            ConsentPayload::IntentReceipt {
                target_id_hash,
                reason_code,
                ..
            },
        ) => {
            validate_hash("target_id_hash", target_id_hash)?;
            validate_code("reason_code", reason_code)?;
            validate_hash("subject", &event.subject)
        }
        (
            ConsentKind::Grant | ConsentKind::Revoke,
            ConsentPayload::Decision {
                subject_code,
                policy_code,
            },
        ) => {
            validate_subject_code("subject_code", subject_code)?;
            if let Some(pc) = policy_code {
                validate_subject_code("policy_code", pc)?;
            }
            validate_subject_code("subject", &event.subject)
        }
        (
            ConsentKind::PromoteReceipt,
            ConsentPayload::PromoteReceipt {
                target_id_hash,
                receipt_id,
                ..
            },
        ) => {
            validate_hash("target_id_hash", target_id_hash)?;
            validate_receipt_id(receipt_id)?;
            validate_hash("subject", &event.subject)
        }
        (kind, payload) => Err(ConsentEventError::KindPayloadMismatch {
            kind,
            expected: expected_payload_variant(kind),
            actual: payload_variant_name(payload),
        }),
    }
}

fn validate_sensor_payload(
    event: &ConsentEvent,
    reason_code: &str,
    sensor_label: &SensorLabel,
) -> Result<(), ConsentEventError> {
    validate_code("reason_code", reason_code)?;
    // The top-level `sensor_id` and the payload `sensor_label` must
    // agree: there's exactly one sensor this event refers to, and any
    // divergence is a programming bug.
    if let Some(top) = &event.sensor_id
        && top != sensor_label
    {
        return Err(ConsentEventError::InvalidCode {
            field: "sensor_id",
            message: "top-level sensor_id must equal payload.sensor_label".to_owned(),
        });
    }
    // Subject must hold the sensor identity (`snr:` prefix + closed
    // class). Reject raw text outright.
    validate_sensor_subject(&event.subject)?;
    if &event.subject[4..] != sensor_label.as_str() {
        return Err(ConsentEventError::InvalidCode {
            field: "subject",
            message: "subject body must equal payload.sensor_label".to_owned(),
        });
    }
    Ok(())
}

/// Hash slot — accepts only fixed cryptographic digest forms:
///
///   * `sha256:<64 lowercase hex>`  — canonical SHA-256.
///   * `hash:<32..=128 lowercase hex>` — salted/truncated digest.
///
/// Both forms guarantee the suffix is hex-only, so user content can
/// never ride through this field. `hash:TOPSECRETBODY`, raw tokens,
/// IDs, and similar alphanumeric secrets all fail because letters
/// outside `[0-9a-f]` are rejected.
fn validate_hash(field: &'static str, value: &str) -> Result<(), ConsentEventError> {
    if let Some(hex) = value.strip_prefix("sha256:") {
        if hex.len() != 64 || !is_lowercase_hex(hex) {
            return Err(ConsentEventError::InvalidHash {
                field,
                message: "sha256 must be `sha256:` + 64 lowercase hex".to_owned(),
            });
        }
        return Ok(());
    }
    let body = value
        .strip_prefix("hash:")
        .ok_or(ConsentEventError::InvalidHash {
            field,
            message: "must start with `sha256:` or `hash:`".to_owned(),
        })?;
    if !(32..=128).contains(&body.len()) {
        return Err(ConsentEventError::InvalidHash {
            field,
            message: "hash body must be 32..=128 chars".to_owned(),
        });
    }
    if !is_lowercase_hex(body) {
        return Err(ConsentEventError::InvalidHash {
            field,
            message: "hash body must be lowercase hex [0-9a-f]".to_owned(),
        });
    }
    Ok(())
}

fn is_lowercase_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Code slot — short, lowercase, `[a-z][a-z0-9_-]{0,63}`. Bounded length
/// + closed class makes it impossible to encode arbitrary user text.
fn validate_code(field: &'static str, value: &str) -> Result<(), ConsentEventError> {
    if value.is_empty() {
        return Err(ConsentEventError::InvalidCode {
            field,
            message: "empty".to_owned(),
        });
    }
    if value.len() > 64 {
        return Err(ConsentEventError::InvalidCode {
            field,
            message: "longer than 64 chars".to_owned(),
        });
    }
    let bytes = value.as_bytes();
    if !bytes[0].is_ascii_lowercase() {
        return Err(ConsentEventError::InvalidCode {
            field,
            message: "first char must be [a-z]".to_owned(),
        });
    }
    if !bytes
        .iter()
        .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
    {
        return Err(ConsentEventError::InvalidCode {
            field,
            message: "chars must be in [a-z0-9_-]".to_owned(),
        });
    }
    Ok(())
}

/// Dotted config key — e.g. `sensors.screen.enabled`. Same class as
/// `code` plus `.`, length bounded.
fn validate_dotted_key(field: &'static str, value: &str) -> Result<(), ConsentEventError> {
    if value.is_empty() {
        return Err(ConsentEventError::InvalidCode {
            field,
            message: "empty".to_owned(),
        });
    }
    if value.len() > 128 {
        return Err(ConsentEventError::InvalidCode {
            field,
            message: "longer than 128 chars".to_owned(),
        });
    }
    if !value
        .bytes()
        .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
    {
        return Err(ConsentEventError::InvalidCode {
            field,
            message: "chars must be in [a-z0-9_.-]".to_owned(),
        });
    }
    Ok(())
}

/// Subject / policy code slot — typed identifier shape
/// `[a-z][a-z0-9._:-]{0,127}`. Allows `:` for typed prefixes
/// (`share_link:abcd`) and `.` for dotted keys; still bounded length and
/// closed class so user content cannot ride through.
fn validate_subject_code(field: &'static str, value: &str) -> Result<(), ConsentEventError> {
    if value.is_empty() {
        return Err(ConsentEventError::InvalidCode {
            field,
            message: "empty".to_owned(),
        });
    }
    if value.len() > 128 {
        return Err(ConsentEventError::InvalidCode {
            field,
            message: "longer than 128 chars".to_owned(),
        });
    }
    let bytes = value.as_bytes();
    if !bytes[0].is_ascii_lowercase() {
        return Err(ConsentEventError::InvalidCode {
            field,
            message: "first char must be [a-z]".to_owned(),
        });
    }
    if !bytes
        .iter()
        .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'-'))
    {
        return Err(ConsentEventError::InvalidCode {
            field,
            message: "chars must be in [a-z0-9._:-]".to_owned(),
        });
    }
    Ok(())
}

/// `scope` slot — closed class `[a-z0-9._:=,-]{1,256}`. Permits canonical
/// scope tuple wire forms (`team:platform`, `private:agent=agt:foo:v1`,
/// comma-joined keys). Bounded length, no whitespace, no quotes — user
/// content cannot ride through.
fn validate_scope(value: &str) -> Result<(), ConsentEventError> {
    if value.is_empty() || value.len() > 256 {
        return Err(ConsentEventError::InvalidCode {
            field: "scope",
            message: "must be 1..=256 chars".to_owned(),
        });
    }
    if !value.bytes().all(|b| {
        matches!(
            b,
            b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'=' | b',' | b'-'
        )
    }) {
        return Err(ConsentEventError::InvalidCode {
            field: "scope",
            message: "chars must be in [a-z0-9._:=,-]".to_owned(),
        });
    }
    Ok(())
}

/// `consent_id` slot — typed identifier `[A-Za-z0-9._:-]{1,64}`.
/// ULIDs and typed prefixes (`c-1`, `consent:abcd`) both satisfy this.
fn validate_consent_id(value: &str) -> Result<(), ConsentEventError> {
    if value.is_empty() || value.len() > 64 {
        return Err(ConsentEventError::InvalidCode {
            field: "consent_id",
            message: "must be 1..=64 chars".to_owned(),
        });
    }
    if !value
        .bytes()
        .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'-'))
    {
        return Err(ConsentEventError::InvalidCode {
            field: "consent_id",
            message: "chars must be in [A-Za-z0-9._:-]".to_owned(),
        });
    }
    Ok(())
}

/// `op_id` slot — same shape as a receipt id (`[A-Za-z0-9._:-]{1,128}`).
fn validate_op_id(value: &str) -> Result<(), ConsentEventError> {
    if value.is_empty() || value.len() > 128 {
        return Err(ConsentEventError::InvalidCode {
            field: "op_id",
            message: "must be 1..=128 chars".to_owned(),
        });
    }
    if !value
        .bytes()
        .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'-'))
    {
        return Err(ConsentEventError::InvalidCode {
            field: "op_id",
            message: "chars must be in [A-Za-z0-9._:-]".to_owned(),
        });
    }
    Ok(())
}

/// Sensor `subject` slot — `snr:<sensor-label-body>` form. Reuses the
/// `SensorLabel` character class, with the `snr:` prefix mandatory because
/// the journal's `subject` column has no other indicator of what kind
/// of identity it is holding.
fn validate_sensor_subject(value: &str) -> Result<(), ConsentEventError> {
    let body = value
        .strip_prefix("snr:")
        .ok_or(ConsentEventError::InvalidCode {
            field: "subject",
            message: "sensor subject must start with `snr:`".to_owned(),
        })?;
    if body.is_empty() || body.len() > 128 {
        return Err(ConsentEventError::InvalidCode {
            field: "subject",
            message: "sensor body must be 1..=128 chars".to_owned(),
        });
    }
    if !body
        .bytes()
        .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'-'))
    {
        return Err(ConsentEventError::InvalidCode {
            field: "subject",
            message: "sensor body chars must be in [A-Za-z0-9._:-]".to_owned(),
        });
    }
    Ok(())
}

/// Receipt id slot — accepts `rcpt:` prefix + closed character class.
fn validate_receipt_id(value: &str) -> Result<(), ConsentEventError> {
    if value.is_empty() {
        return Err(ConsentEventError::InvalidReceipt {
            field: "receipt_id",
            message: "empty".to_owned(),
        });
    }
    if value.len() > 128 {
        return Err(ConsentEventError::InvalidReceipt {
            field: "receipt_id",
            message: "longer than 128 chars".to_owned(),
        });
    }
    if !value
        .bytes()
        .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'-'))
    {
        return Err(ConsentEventError::InvalidReceipt {
            field: "receipt_id",
            message: "chars must be in [A-Za-z0-9._:-]".to_owned(),
        });
    }
    Ok(())
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

    /// Build a fixture hash of the form `hash:<32 lowercase hex>`.
    fn fixture_hash(seed: u32) -> String {
        format!("hash:{seed:0>32x}")
    }

    fn sample_forget() -> ConsentEvent {
        let h = fixture_hash(0x00ab_c123);
        ConsentEvent {
            consent_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            kind: ConsentKind::ForgetIntent,
            actor: Identity::parse("usr:tafeng").expect("valid identity"),
            subject: h.clone(),
            scope: "private:agent=agt:claude-code".to_owned(),
            op_id: Some("op-01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()),
            sensor_id: None,
            payload: ConsentPayload::IntentReceipt {
                target_id_hash: h,
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
        let h = fixture_hash(0x00de_f456);
        ConsentEvent {
            consent_id: "01ARZ3NDEKTSV4RRFFQ69G5FAX".to_owned(),
            kind: ConsentKind::PromoteReceipt,
            actor: Identity::parse("usr:tafeng").expect("valid identity"),
            subject: h.clone(),
            scope: "team:platform".to_owned(),
            op_id: Some("op-01ARZ3NDEKTSV4RRFFQ69G5FAX".to_owned()),
            sensor_id: None,
            payload: ConsentPayload::PromoteReceipt {
                target_id_hash: h,
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
    fn validate_accepts_well_formed_samples() {
        sample_forget().validate().expect("forget valid");
        sample_sensor_enable().validate().expect("sensor valid");
        sample_promote().validate().expect("promote valid");
    }

    #[test]
    fn validate_rejects_kind_payload_mismatch() {
        let mut event = sample_forget();
        // Pair a forget kind with a Decision payload.
        event.payload = ConsentPayload::Decision {
            subject_code: "share_link".to_owned(),
            policy_code: None,
        };
        let err = event.validate().expect_err("must reject");
        assert!(matches!(err, ConsentEventError::KindPayloadMismatch { .. }));
    }

    #[test]
    fn validate_rejects_body_smuggled_in_target_id_hash() {
        // Even though `target_id_hash` is a `String`, the validator binds
        // it to the canonical hash shape — a forgotten body cannot ride
        // through this field.
        let bytes = "TOPSECRETBODY this is the actual user body content";
        let mut event = sample_forget();
        event.subject = bytes.to_owned();
        event.payload = ConsentPayload::IntentReceipt {
            target_id_hash: bytes.to_owned(),
            scope_tier: MemoryVisibility::Private,
            reason_code: "user_command".to_owned(),
        };
        let err = event.validate().expect_err("must reject");
        assert!(
            matches!(err, ConsentEventError::InvalidHash { .. }),
            "expected InvalidHash, got {err:?}",
        );
    }

    #[test]
    fn validate_rejects_long_reason_code() {
        let mut event = sample_forget();
        let h = fixture_hash(0xabc);
        event.subject = h.clone();
        event.payload = ConsentPayload::IntentReceipt {
            target_id_hash: h,
            scope_tier: MemoryVisibility::Private,
            reason_code: "x".repeat(65),
        };
        let err = event.validate().expect_err("too long");
        assert!(matches!(err, ConsentEventError::InvalidCode { .. }));
    }

    #[test]
    fn validate_rejects_uppercase_reason_code() {
        let mut event = sample_forget();
        let h = fixture_hash(0xabc);
        event.subject = h.clone();
        event.payload = ConsentPayload::IntentReceipt {
            target_id_hash: h,
            scope_tier: MemoryVisibility::Private,
            reason_code: "USER_COMMAND".to_owned(),
        };
        let err = event.validate().expect_err("must be lowercase");
        assert!(matches!(err, ConsentEventError::InvalidCode { .. }));
    }

    #[test]
    fn validate_rejects_forget_subject_without_hash_prefix() {
        let mut event = sample_forget();
        event.subject = "the original target id".to_owned();
        let err = event.validate().expect_err("subject must be hash-shaped");
        assert!(matches!(err, ConsentEventError::InvalidHash { .. }));
    }

    #[test]
    fn validate_rejects_invalid_sha256_hash() {
        let mut event = sample_forget();
        event.subject = "sha256:NOTHEX".to_owned();
        event.payload = ConsentPayload::IntentReceipt {
            target_id_hash: "sha256:NOTHEX".to_owned(),
            scope_tier: MemoryVisibility::Private,
            reason_code: "user_command".to_owned(),
        };
        let err = event.validate().expect_err("bad hex");
        assert!(matches!(err, ConsentEventError::InvalidHash { .. }));
    }

    #[test]
    fn validate_rejects_promote_with_bad_receipt_id() {
        let mut event = sample_promote();
        let h = fixture_hash(0xabc);
        event.subject = h.clone();
        event.payload = ConsentPayload::PromoteReceipt {
            target_id_hash: h,
            from_tier: MemoryVisibility::Private,
            to_tier: MemoryVisibility::Team,
            receipt_id: "rcpt has spaces".to_owned(),
        };
        let err = event.validate().expect_err("bad receipt");
        assert!(matches!(err, ConsentEventError::InvalidReceipt { .. }));
    }

    #[test]
    fn validate_accepts_sha256_canonical_form() {
        let mut event = sample_forget();
        let h = format!("sha256:{}", "0".repeat(64));
        event.subject = h.clone();
        event.payload = ConsentPayload::IntentReceipt {
            target_id_hash: h,
            scope_tier: MemoryVisibility::Private,
            reason_code: "user_command".to_owned(),
        };
        event.validate().expect("sha256 form valid");
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
