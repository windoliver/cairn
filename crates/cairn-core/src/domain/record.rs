//! [`MemoryRecord`] — the typed durable record (brief §3, §6, §6.5, §4.2).
//!
//! A `MemoryRecord` is the core domain type Cairn writes, retrieves, and
//! reasons about. It is serialized three ways without re-derivation:
//!
//! - **API envelopes** — `serde_json` wire form.
//! - **`SQLite` row JSON columns** — same `serde_json` representation.
//! - **Markdown frontmatter** — YAML; the markdown projector splits the
//!   record into a `body`-less header + the body content.
//!
//! Construction does not enforce invariants on its own — call
//! [`MemoryRecord::validate`] before any [`crate::contract::MemoryStore`]
//! write so the typed errors in [`crate::domain::DomainError`] surface
//! before the WAL is touched.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{
    ActorChainEntry, ChainRole, DomainError, EvidenceVector, IdentityKind, Provenance,
    Rfc3339Timestamp, ScopeTuple,
    actor_chain::validate_chain,
    taxonomy::{MemoryClass, MemoryKind, MemoryVisibility},
};

/// Ed25519 signature in `ed25519:<128 lowercase hex>` form. Mirrors the
/// schema in `crates/cairn-idl/schema/common/primitives.json` so domain
/// signatures parse and serialize identically to wire signatures.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Ed25519Signature(String);

impl Ed25519Signature {
    /// Parse an `ed25519:<128 hex>` signature.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        let Some(tail) = raw.strip_prefix("ed25519:") else {
            return Err(DomainError::MissingSignature {
                message: "signature must start with `ed25519:`".to_owned(),
            });
        };
        if tail.len() != 128 {
            return Err(DomainError::MissingSignature {
                message: format!(
                    "signature must be `ed25519:` + exactly 128 hex chars (got {} hex chars)",
                    tail.len()
                ),
            });
        }
        if !tail.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(DomainError::MissingSignature {
                message: "signature hex tail must be lowercase 0-9 a-f".to_owned(),
            });
        }
        Ok(Self(raw))
    }

    /// Wire-form signature string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Ed25519Signature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// ULID-typed record id. 26 chars, Crockford base32, uppercase, no `I L O U`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct RecordId(String);

impl RecordId {
    /// Parse a wire-form ULID.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.len() != 26 {
            return Err(DomainError::EmptyField { field: "record_id" });
        }
        if !raw.bytes().all(|b| {
            matches!(b,
                b'0'..=b'9'
                | b'A'..=b'H'
                | b'J'
                | b'K'
                | b'M'
                | b'N'
                | b'P'..=b'T'
                | b'V'..=b'Z')
        }) {
            return Err(DomainError::EmptyField { field: "record_id" });
        }
        Ok(Self(raw))
    }

    /// Underlying ULID string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RecordId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// The typed durable memory record.
///
/// Field ordering of this struct *is the wire ordering* — `serde` emits
/// fields in declaration order, which means JSON / YAML / `SQLite` rows all
/// agree on canonical key order. Adapters should call [`Self::validate`]
/// before any persistence side effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecord {
    /// ULID — the stable record identifier.
    pub id: RecordId,
    /// Memory kind (§6.1).
    pub kind: MemoryKind,
    /// Memory class (§6.2).
    pub class: MemoryClass,
    /// Visibility tier (§6.3). Default for new records is `private` or
    /// `session` per kind config — domain validation does not enforce that
    /// default.
    pub visibility: MemoryVisibility,
    /// Scope tuple (§6, §4.2). At least one dimension must be set.
    pub scope: ScopeTuple,
    /// Markdown body. Required and non-empty.
    pub body: String,
    /// Mandatory provenance frontmatter (§6.5).
    pub provenance: Provenance,
    /// Wall-clock instant of the most recent durable update.
    pub updated_at: Rfc3339Timestamp,
    /// Evidence vector (§6.4).
    pub evidence: EvidenceVector,
    /// Salience scalar in `[0.0, 1.0]`.
    pub salience: f32,
    /// Confidence scalar in `[0.0, 1.0]`. Banding lives in
    /// [`crate::domain::ConfidenceBand::from_scalar`].
    pub confidence: f32,
    /// Actor chain (§4.2). At minimum: one `author` entry.
    pub actor_chain: Vec<ActorChainEntry>,
    /// Author signature over the canonical record bytes.
    pub signature: Ed25519Signature,
    /// Tags (free-form). Empty by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Extra YAML/JSON frontmatter the ingest call carried (§ schema
    /// `verbs/ingest.json`). Stored verbatim; ordered for deterministic
    /// re-emission via `BTreeMap`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_frontmatter: BTreeMap<String, serde_json::Value>,
}

impl MemoryRecord {
    /// Validate every domain invariant. Returns the first violation found.
    ///
    /// This is **shape validation only** — it confirms the record is
    /// well-formed (provenance present, identity refs parse, scope is
    /// non-empty, visibility/kind/class are recognized, evidence and
    /// scalar ranges hold, signature has the right wire form). It does
    /// **not** verify the cryptographic signature against the author's
    /// key material; that check belongs to the store boundary where
    /// keychain-resident keys are available (brief §4.2 "Signature-first
    /// rejection"). A successful return from `validate` means the record
    /// is *eligible* for crypto verification, not that it has been
    /// verified.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.body.is_empty() {
            return Err(DomainError::EmptyField { field: "body" });
        }
        if self.id.as_str().is_empty() {
            return Err(DomainError::EmptyField { field: "id" });
        }
        self.scope.validate()?;
        self.provenance.validate()?;
        self.evidence.validate()?;
        if !(0.0..=1.0).contains(&self.salience) || self.salience.is_nan() {
            return Err(DomainError::OutOfRange {
                field: "salience",
                message: format!("must be in [0.0, 1.0], was {}", self.salience),
            });
        }
        if !(0.0..=1.0).contains(&self.confidence) || self.confidence.is_nan() {
            return Err(DomainError::OutOfRange {
                field: "confidence",
                message: format!("must be in [0.0, 1.0], was {}", self.confidence),
            });
        }
        validate_chain(&self.actor_chain)?;
        self.validate_sensor_consistency()?;
        self.validate_actor_scope_consistency()?;
        self.validate_temporal_invariants()?;
        for tag in &self.tags {
            if tag.is_empty() {
                return Err(DomainError::EmptyField { field: "tag" });
            }
        }
        Ok(())
    }

    /// Cross-field check: the sensor that captured the source bytes
    /// (`provenance.source_sensor`) must agree with what the actor chain
    /// claims about sensor involvement. Otherwise downstream policy may
    /// trust the provenance sensor while the signature only proves a
    /// different actor authored the record.
    ///
    /// Rules:
    /// - `MemoryKind::SensorObservation` records must be attributable to
    ///   `provenance.source_sensor` via the chain — either the author *is*
    ///   that sensor, or an explicit `Sensor` role entry naming that
    ///   sensor is present. Otherwise the signature does not prove the
    ///   sensor in `provenance.source_sensor` had any role in the write.
    /// - When the chain author is a `Sensor`, that sensor identity must
    ///   equal `provenance.source_sensor`.
    /// - When the chain has explicit `Sensor` role entries (any kind of
    ///   record), `provenance.source_sensor` must match one of them.
    fn validate_sensor_consistency(&self) -> Result<(), DomainError> {
        let author = self
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author);
        if let Some(author) = author
            && author.identity.kind() == IdentityKind::Sensor
            && author.identity != self.provenance.source_sensor
        {
            return Err(DomainError::InvalidIdentity {
                message: format!(
                    "sensor-authored record: chain author `{}` does not match provenance.source_sensor `{}`",
                    author.identity.as_str(),
                    self.provenance.source_sensor.as_str()
                ),
            });
        }

        // Every `Sensor` chain entry must equal `provenance.source_sensor`.
        // Provenance is single-source (one `source_sensor`, one
        // `source_hash`) so any extra sensor identity in the chain would
        // be unattributed and potentially treated as a co-capturer by
        // downstream policy.
        for entry in self
            .actor_chain
            .iter()
            .filter(|e| e.role == ChainRole::Sensor)
        {
            if entry.identity != self.provenance.source_sensor {
                return Err(DomainError::InvalidIdentity {
                    message: format!(
                        "actor_chain sensor entry `{}` does not equal provenance.source_sensor `{}` (provenance is single-source until multi-sensor records are modeled)",
                        entry.identity.as_str(),
                        self.provenance.source_sensor.as_str()
                    ),
                });
            }
        }
        // Bidirectional sensor-author invariant:
        //   - `SensorObservation` records *must* have a sensor author equal
        //     to `provenance.source_sensor` (otherwise the signature does
        //     not prove sensor participation; unsigned `Sensor` chain
        //     entries are claims, not proof, until P2 countersignatures).
        //   - Sensor authors are *only* legal for `SensorObservation`. A
        //     sensor key has narrow trust (raw event capture); allowing it
        //     to author derived kinds like `Rule`, `Fact`, or `Reasoning`
        //     would let a low-trust signer mint high-trust memories.
        let author_is_sensor =
            matches!(author, Some(a) if a.identity.kind() == IdentityKind::Sensor);
        match self.kind {
            MemoryKind::SensorObservation => {
                let author_is_source =
                    matches!(author, Some(a) if a.identity == self.provenance.source_sensor);
                if !author_is_source {
                    return Err(DomainError::InvalidIdentity {
                        message: format!(
                            "sensor_observation record must have author == provenance.source_sensor `{}` (unsigned `sensor` chain entries do not prove sensor participation until P2 countersignatures land)",
                            self.provenance.source_sensor.as_str()
                        ),
                    });
                }
            }
            other if author_is_sensor => {
                return Err(DomainError::InvalidIdentity {
                    message: format!(
                        "sensor identities may only author `sensor_observation` records, not `{}` (derived kinds need a human or agent author)",
                        other.as_str()
                    ),
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Cross-field check binding scope and originator to the signed author.
    ///
    /// At P0 the record carries a single author signature; principal,
    /// delegator, and sensor chain entries are unsigned attestations until
    /// per-entry countersignatures arrive at P2 (brief §4.2). To avoid
    /// scope-attribution forgery (an agent author claiming
    /// `scope.user = victim` via an unsigned `principal: usr:victim`
    /// entry), the only chain identity allowed to satisfy
    /// `scope.user`/`scope.agent` and `provenance.originating_agent_id`
    /// is the author itself.
    ///
    /// When countersignatures land at P2, this check should grow to
    /// accept any identity in the chain whose countersignature has been
    /// verified.
    fn validate_actor_scope_consistency(&self) -> Result<(), DomainError> {
        let Some(author) = self
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
        else {
            // Caught earlier by `validate_chain`; reachable only if
            // someone bypassed that step.
            return Ok(());
        };
        let author_user_body = author.identity.as_str().strip_prefix("usr:");
        let author_agent_body = author.identity.as_str().strip_prefix("agt:");

        if let Some(user) = self.scope.user.as_deref()
            && Some(user) != author_user_body
        {
            return Err(DomainError::MalformedScope {
                message: format!(
                    "scope.user `{user}` does not match the signing author `{}` (P0: only the author is signed; principal/delegator entries are unsigned at P0)",
                    author.identity.as_str()
                ),
            });
        }
        if let Some(agent) = self.scope.agent.as_deref()
            && Some(agent) != author_agent_body
        {
            return Err(DomainError::MalformedScope {
                message: format!(
                    "scope.agent `{agent}` does not match the signing author `{}` (P0: only the author is signed)",
                    author.identity.as_str()
                ),
            });
        }
        if self.provenance.originating_agent_id != author.identity {
            return Err(DomainError::InvalidIdentity {
                message: format!(
                    "provenance.originating_agent_id `{}` does not match the signing author `{}` (P0: delegation requires P2 countersignatures)",
                    self.provenance.originating_agent_id.as_str(),
                    author.identity.as_str()
                ),
            });
        }
        Ok(())
    }

    fn validate_temporal_invariants(&self) -> Result<(), DomainError> {
        let created = epoch_with_nanos(self.provenance.created_at.as_str())?;
        let updated = epoch_with_nanos(self.updated_at.as_str())?;
        if created > updated {
            return Err(DomainError::InvalidTimestamp {
                message: format!(
                    "provenance.created_at `{}` is after updated_at `{}`",
                    self.provenance.created_at.as_str(),
                    self.updated_at.as_str()
                ),
            });
        }
        for entry in &self.actor_chain {
            let at = epoch_with_nanos(entry.at.as_str())?;
            if at > updated {
                return Err(DomainError::InvalidTimestamp {
                    message: format!(
                        "actor_chain entry `at` ({}) is after updated_at ({})",
                        entry.at.as_str(),
                        self.updated_at.as_str()
                    ),
                });
            }
        }
        Ok(())
    }
}

/// Convert a validated RFC3339 timestamp string to UTC `(epoch_seconds,
/// nanos)` for ordering with subsecond precision.
///
/// Cheap parser used only for ordering inside [`MemoryRecord::validate`];
/// the input has already passed [`Rfc3339Timestamp::parse`] so range checks
/// here are belt-and-braces. We avoid `chrono`/`time` to keep `cairn-core`
/// dep-free.
fn epoch_with_nanos(raw: &str) -> Result<(i64, u32), DomainError> {
    let bytes = raw.as_bytes();
    let invalid = || DomainError::InvalidTimestamp {
        message: format!("`{raw}`: cannot parse for ordering"),
    };

    if bytes.len() < 20 {
        return Err(invalid());
    }
    let year: i64 = parse_int(&bytes[..4]).ok_or_else(invalid)?;
    let month: i64 = parse_int(&bytes[5..7]).ok_or_else(invalid)?;
    let day: i64 = parse_int(&bytes[8..10]).ok_or_else(invalid)?;
    let hour: i64 = parse_int(&bytes[11..13]).ok_or_else(invalid)?;
    let minute: i64 = parse_int(&bytes[14..16]).ok_or_else(invalid)?;
    let second: i64 = parse_int(&bytes[17..19]).ok_or_else(invalid)?;

    let mut idx = 19;
    let mut nanos: u32 = 0;
    if idx < bytes.len() && bytes[idx] == b'.' {
        idx += 1;
        let frac_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        // Pad / truncate to 9 digits for nanoseconds.
        let mut acc: u64 = 0;
        let mut count = 0;
        for &b in &bytes[frac_start..idx] {
            if count >= 9 {
                break;
            }
            acc = acc * 10 + u64::from(b - b'0');
            count += 1;
        }
        while count < 9 {
            acc *= 10;
            count += 1;
        }
        nanos = u32::try_from(acc).map_err(|_| invalid())?;
    }
    let offset_seconds: i64 = match bytes.get(idx) {
        Some(b'Z' | b'z') => 0,
        Some(b'+' | b'-') => {
            let sign: i64 = if bytes[idx] == b'-' { -1 } else { 1 };
            let oh: i64 = parse_int(&bytes[idx + 1..idx + 3]).ok_or_else(invalid)?;
            let om: i64 = parse_int(&bytes[idx + 4..idx + 6]).ok_or_else(invalid)?;
            sign * (oh * 3600 + om * 60)
        }
        _ => return Err(invalid()),
    };

    let days = days_from_civil(year, month, day);
    let local = days * 86_400 + hour * 3600 + minute * 60 + second;
    Ok((local - offset_seconds, nanos))
}

fn parse_int(bytes: &[u8]) -> Option<i64> {
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut acc: i64 = 0;
    for b in bytes {
        acc = acc * 10 + i64::from(b - b'0');
    }
    Some(acc)
}

/// Days since 1970-01-01 for a (proleptic Gregorian) civil date. Algorithm:
/// Howard Hinnant, *date.h* — `days_from_civil`.
const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ActorChainEntry, ChainRole, Identity};

    pub(crate) fn sample_record() -> MemoryRecord {
        // Single human author at P0: scope.user, originating_agent_id, and
        // chain author all bind to `usr:tafeng`. Delegation chains arrive
        // with P2 countersignatures.
        let user_id = Identity::parse("usr:tafeng").expect("valid");
        MemoryRecord {
            id: RecordId::parse("01HQZX9F5N0000000000000000").expect("valid"),
            kind: MemoryKind::User,
            class: MemoryClass::Semantic,
            visibility: MemoryVisibility::Private,
            scope: ScopeTuple {
                user: Some("tafeng".to_owned()),
                ..ScopeTuple::default()
            },
            body: "user prefers dark mode".to_owned(),
            provenance: Provenance {
                source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
                created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
                originating_agent_id: user_id.clone(),
                source_hash: format!("sha256:{}", "a".repeat(64)),
                consent_ref: "consent:01HQZ".to_owned(),
                llm_id_if_any: None,
            },
            updated_at: Rfc3339Timestamp::parse("2026-04-22T14:05:11Z").expect("valid"),
            evidence: EvidenceVector::default(),
            salience: 0.5,
            confidence: 0.7,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: user_id,
                at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            }],
            signature: Ed25519Signature::parse(format!("ed25519:{}", "a".repeat(128)))
                .expect("valid"),
            tags: vec!["pref".to_owned()],
            extra_frontmatter: BTreeMap::new(),
        }
    }

    #[test]
    fn valid_record_passes_validation() {
        sample_record().validate().expect("valid");
    }

    #[test]
    fn empty_body_rejected() {
        let mut r = sample_record();
        r.body.clear();
        let err = r.validate().unwrap_err();
        assert_eq!(err, DomainError::EmptyField { field: "body" });
    }

    #[test]
    fn empty_scope_rejected() {
        let mut r = sample_record();
        r.scope = ScopeTuple::default();
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn out_of_range_confidence_rejected() {
        let mut r = sample_record();
        r.confidence = 1.5;
        let err = r.validate().unwrap_err();
        assert!(matches!(
            err,
            DomainError::OutOfRange {
                field: "confidence",
                ..
            }
        ));
    }

    #[test]
    fn out_of_range_salience_rejected() {
        let mut r = sample_record();
        r.salience = -0.1;
        let err = r.validate().unwrap_err();
        assert!(matches!(
            err,
            DomainError::OutOfRange {
                field: "salience",
                ..
            }
        ));
    }

    #[test]
    fn missing_author_rejected() {
        let mut r = sample_record();
        r.actor_chain.clear();
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn bad_signature_rejected_at_parse() {
        let err = Ed25519Signature::parse("notasig").unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn json_round_trip_preserves_all_fields() {
        let r = sample_record();
        let s = serde_json::to_string(&r).expect("ser");
        let back: MemoryRecord = serde_json::from_str(&s).expect("de");
        assert_eq!(r, back);
    }

    #[test]
    fn sensor_authored_record_must_match_provenance() {
        let mut r = sample_record();
        // Sensor authors are only valid for SensorObservation.
        r.kind = MemoryKind::SensorObservation;
        let sensor =
            Identity::parse("snr:local:hook:cc-session:v1").expect("valid sensor identity");
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        r.provenance.source_sensor = sensor.clone();
        r.provenance.originating_agent_id = sensor.clone();
        r.scope = ScopeTuple {
            entity: Some("camera-4".to_owned()),
            ..ScopeTuple::default()
        };
        r.validate().expect("matched sensor author + provenance");

        // Now flip provenance to a different sensor.
        r.provenance.source_sensor =
            Identity::parse("snr:local:hook:other:v1").expect("valid sensor identity");
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
    }

    #[test]
    fn sensor_observation_requires_sensor_author() {
        let mut r = sample_record();
        r.kind = MemoryKind::SensorObservation;
        // Default sample has agent author — invalid for SensorObservation.
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));

        // Adding a sensor chain entry naming source_sensor is NOT enough at
        // P0 — the sensor must be the actual author so the signature
        // proves sensor participation.
        r.actor_chain.push(ActorChainEntry {
            role: ChainRole::Sensor,
            identity: r.provenance.source_sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        });
        let err = r.validate().unwrap_err();
        assert!(
            matches!(err, DomainError::InvalidIdentity { .. }),
            "unsigned sensor entry must not be sufficient for sensor_observation"
        );

        // Make the sensor the author → valid (after aligning scope and
        // originating_agent_id with the sensor-only chain).
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: r.provenance.source_sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        r.provenance.originating_agent_id = r.provenance.source_sensor.clone();
        r.scope = ScopeTuple {
            entity: Some("camera-4".to_owned()),
            ..ScopeTuple::default()
        };
        r.validate()
            .expect("sensor-as-author is the only valid sensor_observation shape");
    }

    #[test]
    fn sensor_chain_entry_must_match_provenance() {
        let mut r = sample_record();
        let chain_sensor =
            Identity::parse("snr:local:hook:cc-session:v1").expect("valid sensor identity");
        let other_sensor =
            Identity::parse("snr:local:hook:other:v1").expect("valid sensor identity");
        r.actor_chain.push(ActorChainEntry {
            role: ChainRole::Sensor,
            identity: chain_sensor,
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        });
        r.provenance.source_sensor = other_sensor;
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
    }

    #[test]
    fn agent_author_cannot_forge_user_scope_via_unsigned_principal() {
        // P0 attack: agent signs a record but adds an unsigned `principal:
        // usr:victim` entry, claiming `scope.user = victim`. Validator must
        // reject because principal/delegator entries are unsigned at P0.
        let mut r = sample_record();
        r.actor_chain = vec![
            ActorChainEntry {
                role: ChainRole::Principal,
                identity: Identity::parse("usr:victim").expect("valid"),
                at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            },
            ActorChainEntry {
                role: ChainRole::Author,
                identity: Identity::parse("agt:attacker:v1").expect("valid"),
                at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            },
        ];
        r.scope = ScopeTuple {
            user: Some("victim".to_owned()),
            ..ScopeTuple::default()
        };
        r.provenance.originating_agent_id = Identity::parse("agt:attacker:v1").expect("valid");
        let err = r.validate().unwrap_err();
        assert!(
            matches!(err, DomainError::MalformedScope { .. }),
            "agent author cannot satisfy scope.user via unsigned principal entry"
        );
    }

    #[test]
    fn sensor_author_rejected_for_non_sensor_kinds() {
        let mut r = sample_record();
        r.kind = MemoryKind::Rule;
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: r.provenance.source_sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
    }

    #[test]
    fn created_after_updated_rejected() {
        let mut r = sample_record();
        r.provenance.created_at = Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid");
        r.updated_at = Rfc3339Timestamp::parse("2026-04-22T14:00:00Z").expect("valid");
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }

    #[test]
    fn chain_entry_after_updated_rejected() {
        let mut r = sample_record();
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("usr:tafeng").expect("valid"),
            at: Rfc3339Timestamp::parse("2026-04-22T16:00:00Z").expect("valid"),
        }];
        r.updated_at = Rfc3339Timestamp::parse("2026-04-22T14:00:00Z").expect("valid");
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }

    #[test]
    fn temporal_check_handles_offsets() {
        // 14:00 +02:00 == 12:00 UTC, which is BEFORE 13:00 Z, so the
        // ordering must be chronological, not lexical.
        let mut r = sample_record();
        r.provenance.created_at =
            Rfc3339Timestamp::parse("2026-04-22T14:00:00+02:00").expect("valid");
        r.updated_at = Rfc3339Timestamp::parse("2026-04-22T13:00:00Z").expect("valid");
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("usr:tafeng").expect("valid"),
            at: Rfc3339Timestamp::parse("2026-04-22T14:00:00+02:00").expect("valid"),
        }];
        r.validate()
            .expect("created_at 14:00+02:00 (= 12:00Z) is before updated_at 13:00Z");
    }

    #[test]
    fn deserialize_rejects_unknown_fields() {
        let mut value = serde_json::to_value(sample_record()).expect("ser");
        value
            .as_object_mut()
            .expect("object")
            .insert("zzz".to_owned(), serde_json::json!("bad"));
        let res: Result<MemoryRecord, _> = serde_json::from_value(value);
        assert!(res.is_err(), "unknown field should reject");
    }
}
