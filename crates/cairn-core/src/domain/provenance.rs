//! Provenance — mandatory on every record (brief §6.5).
//!
//! `{source_sensor, created_at, llm_id_if_any, originating_agent_id,
//! source_hash, consent_ref}` — five required components plus one optional
//! (`llm_id_if_any`). Every record must answer *who wrote me, when, under
//! what consent, and from what evidence*.

use serde::{Deserialize, Serialize};

use crate::domain::{DomainError, Identity, IdentityKind, Rfc3339Timestamp};

/// Mandatory provenance frontmatter on every [`crate::domain::MemoryRecord`].
///
/// `llm_id_if_any` is structurally required — the key must be present in
/// the wire form (`null` for "no LLM") so a missing/truncated record can
/// be detected. The custom `Deserialize` below uses an `Option<Option<…>>`
/// pattern to distinguish "key absent" from "key present, value null".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// Sensor identity that captured the source bytes.
    pub source_sensor: Identity,
    /// Wall-clock instant the record was first written.
    pub created_at: Rfc3339Timestamp,
    /// Agent or human who initiated the write. Distinct from the chain
    /// author when an upstream principal delegated.
    pub originating_agent_id: Identity,
    /// Cryptographic hash of the source bytes the record was derived from.
    /// Format `<algo>:<hex>` where `algo ∈ {sha256, sha512, blake3}` and
    /// the hex tail length matches the algorithm's digest size.
    pub source_hash: String,
    /// `consent.log` row id this write was authorized under.
    pub consent_ref: String,
    /// LLM identifier (model + revision) when the record was produced or
    /// summarized by a model. Optional in *value* (`null` when not
    /// applicable) but **structurally required** — the field is always
    /// serialized so consumers can distinguish "explicit no-LLM
    /// provenance" from a missing/truncated record.
    pub llm_id_if_any: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvenanceWire {
    source_sensor: Identity,
    created_at: Rfc3339Timestamp,
    originating_agent_id: Identity,
    source_hash: String,
    consent_ref: String,
    /// Three-state on the wire: outer `None` = key absent (truncated record),
    /// `Some(None)` = explicit null (no-LLM provenance), `Some(Some(_))` =
    /// model id. Without the custom `deserialize_with`, serde collapses
    /// `null` into outer `None`, conflating "missing" with "explicit null".
    #[serde(default, deserialize_with = "deserialize_explicit_optional")]
    #[allow(
        clippy::option_option,
        reason = "load-bearing: distinguishes missing key from explicit null"
    )]
    llm_id_if_any: Option<Option<String>>,
}

#[allow(
    clippy::option_option,
    reason = "load-bearing: distinguishes missing key from explicit null"
)]
fn deserialize_explicit_optional<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

impl<'de> Deserialize<'de> for Provenance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = ProvenanceWire::deserialize(deserializer)?;
        let llm_id_if_any = raw.llm_id_if_any.ok_or_else(|| {
            serde::de::Error::custom(
                "provenance.llm_id_if_any is structurally required (use null when no LLM was used)",
            )
        })?;
        Ok(Self {
            source_sensor: raw.source_sensor,
            created_at: raw.created_at,
            originating_agent_id: raw.originating_agent_id,
            source_hash: raw.source_hash,
            consent_ref: raw.consent_ref,
            llm_id_if_any,
        })
    }
}

impl Provenance {
    /// Validate that every required component is present and non-empty.
    /// Wire-form validation already runs at deserialize time for `Identity`
    /// and `Rfc3339Timestamp`; this catches the string fields and surfaces
    /// uniform [`DomainError::MissingProvenance`] errors callers can match
    /// on regardless of construction path.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.source_sensor.kind() != IdentityKind::Sensor {
            return Err(DomainError::InvalidIdentity {
                message: format!(
                    "provenance.source_sensor must be a `snr:` identity, got `{}`",
                    self.source_sensor.as_str()
                ),
            });
        }
        // `originating_agent_id` is named for the agent case but the
        // field also bears human-initiated and sensor-initiated writes
        // (sensor-authored raw events). The actual cross-field check —
        // "originator must appear in the chain" — happens in
        // `MemoryRecord::validate_actor_scope_consistency`.
        validate_source_hash(&self.source_hash)?;
        if self.consent_ref.is_empty() {
            return Err(DomainError::MissingProvenance {
                field: "consent_ref",
            });
        }
        if let Some(v) = &self.llm_id_if_any
            && v.is_empty()
        {
            return Err(DomainError::MissingProvenance {
                field: "llm_id_if_any",
            });
        }
        Ok(())
    }
}

fn validate_source_hash(raw: &str) -> Result<(), DomainError> {
    if raw.is_empty() {
        return Err(DomainError::MissingProvenance {
            field: "source_hash",
        });
    }
    let Some((algo, hex)) = raw.split_once(':') else {
        return Err(DomainError::MissingProvenance {
            field: "source_hash",
        });
    };
    let expected_len = match algo {
        "sha256" | "blake3" => 64,
        "sha512" => 128,
        _ => {
            return Err(DomainError::MissingProvenance {
                field: "source_hash",
            });
        }
    };
    if hex.len() != expected_len {
        return Err(DomainError::MissingProvenance {
            field: "source_hash",
        });
    }
    if !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(DomainError::MissingProvenance {
            field: "source_hash",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Provenance {
        Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            originating_agent_id: Identity::parse("agt:claude-code:opus-4-7:main:v1")
                .expect("valid"),
            source_hash: format!("sha256:{}", "a".repeat(64)),
            consent_ref: "consent:01HQZ".to_owned(),
            llm_id_if_any: None,
        }
    }

    #[test]
    fn valid_round_trips() {
        let p = sample();
        p.validate().expect("valid");
        let s = serde_json::to_string(&p).expect("ser");
        let back: Provenance = serde_json::from_str(&s).expect("de");
        assert_eq!(p, back);
    }

    #[test]
    fn rejects_empty_source_hash() {
        let mut p = sample();
        p.source_hash.clear();
        let err = p.validate().unwrap_err();
        assert!(matches!(
            err,
            DomainError::MissingProvenance {
                field: "source_hash"
            }
        ));
    }

    #[test]
    fn rejects_empty_consent_ref() {
        let mut p = sample();
        p.consent_ref.clear();
        let err = p.validate().unwrap_err();
        assert!(matches!(
            err,
            DomainError::MissingProvenance {
                field: "consent_ref"
            }
        ));
    }

    #[test]
    fn rejects_empty_llm_id_when_present() {
        let mut p = sample();
        p.llm_id_if_any = Some(String::new());
        let err = p.validate().unwrap_err();
        assert!(matches!(
            err,
            DomainError::MissingProvenance {
                field: "llm_id_if_any"
            }
        ));
    }

    #[test]
    fn deserialize_rejects_missing_llm_id_key() {
        // Key entirely absent — must reject. A bare `Option<String>` would
        // default to `None` and silently accept this; the custom deserializer
        // distinguishes "missing" from "explicit null".
        let json = r#"{
            "source_sensor": "snr:local:hook:cc-session:v1",
            "created_at": "2026-04-22T14:02:11Z",
            "originating_agent_id": "agt:claude-code:opus-4-7:main:v1",
            "source_hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "consent_ref": "consent:01HQZ"
        }"#;
        let res: Result<Provenance, _> = serde_json::from_str(json);
        let err = res.expect_err("missing llm_id_if_any key must fail");
        assert!(
            err.to_string().contains("llm_id_if_any"),
            "error mentions field, got: {err}"
        );
    }

    #[test]
    fn deserialize_accepts_explicit_null_llm_id() {
        let json = r#"{
            "source_sensor": "snr:local:hook:cc-session:v1",
            "created_at": "2026-04-22T14:02:11Z",
            "originating_agent_id": "agt:claude-code:opus-4-7:main:v1",
            "source_hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "consent_ref": "consent:01HQZ",
            "llm_id_if_any": null
        }"#;
        let p: Provenance = serde_json::from_str(json).expect("explicit null is valid");
        assert!(p.llm_id_if_any.is_none());
    }

    #[test]
    fn deserialize_rejects_missing_fields() {
        let json = r#"{
            "source_sensor": "snr:local:hook:cc-session:v1",
            "created_at": "2026-04-22T14:02:11Z",
            "originating_agent_id": "agt:claude-code:opus-4-7:main:v1",
            "consent_ref": "consent:01HQZ"
        }"#;
        let res: Result<Provenance, _> = serde_json::from_str(json);
        assert!(res.is_err(), "missing source_hash should fail");
    }

    #[test]
    fn rejects_source_sensor_wrong_kind() {
        let mut p = sample();
        p.source_sensor = Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid");
        let err = p.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
    }

    #[test]
    fn allows_originating_agent_as_sensor() {
        // Sensor-authored raw events have a sensor originator — record-level
        // validation enforces the chain-membership cross-field check.
        let mut p = sample();
        p.originating_agent_id = Identity::parse("snr:local:hook:cc-session:v1").expect("valid");
        p.validate()
            .expect("sensor originator allowed at provenance level");
    }

    #[test]
    fn allows_originating_agent_as_human() {
        let mut p = sample();
        p.originating_agent_id = Identity::parse("usr:tafeng").expect("valid");
        p.validate().expect("human originator allowed");
    }

    #[test]
    fn rejects_unstructured_source_hash() {
        let mut p = sample();
        p.source_hash = "not-a-hash".to_owned();
        let err = p.validate().unwrap_err();
        assert!(matches!(
            err,
            DomainError::MissingProvenance {
                field: "source_hash"
            }
        ));
    }

    #[test]
    fn rejects_unknown_hash_algo() {
        let mut p = sample();
        p.source_hash = format!("md5:{}", "a".repeat(32));
        let err = p.validate().unwrap_err();
        assert!(matches!(
            err,
            DomainError::MissingProvenance {
                field: "source_hash"
            }
        ));
    }

    #[test]
    fn rejects_wrong_hash_length() {
        let mut p = sample();
        p.source_hash = format!("sha256:{}", "a".repeat(63));
        let err = p.validate().unwrap_err();
        assert!(matches!(
            err,
            DomainError::MissingProvenance {
                field: "source_hash"
            }
        ));
    }

    #[test]
    fn accepts_sha512_and_blake3() {
        let mut p = sample();
        p.source_hash = format!("sha512:{}", "a".repeat(128));
        p.validate().expect("sha512 valid");
        p.source_hash = format!("blake3:{}", "a".repeat(64));
        p.validate().expect("blake3 valid");
    }
}
