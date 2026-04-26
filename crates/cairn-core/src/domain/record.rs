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
    ActorChainEntry, DomainError, EvidenceVector, Provenance, Rfc3339Timestamp, ScopeTuple,
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
        for tag in &self.tags {
            if tag.is_empty() {
                return Err(DomainError::EmptyField { field: "tag" });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ActorChainEntry, ChainRole, Identity};

    pub(crate) fn sample_record() -> MemoryRecord {
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
                originating_agent_id: Identity::parse("agt:claude-code:opus-4-7:main:v1")
                    .expect("valid"),
                source_hash: "sha256:abc123".to_owned(),
                consent_ref: "consent:01HQZ".to_owned(),
                llm_id_if_any: None,
            },
            updated_at: Rfc3339Timestamp::parse("2026-04-22T14:05:11Z").expect("valid"),
            evidence: EvidenceVector::default(),
            salience: 0.5,
            confidence: 0.7,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid"),
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
