//! Provenance — mandatory on every record (brief §6.5).
//!
//! `{source_sensor, created_at, llm_id_if_any, originating_agent_id,
//! source_hash, consent_ref}` — five required components plus one optional
//! (`llm_id_if_any`). Every record must answer *who wrote me, when, under
//! what consent, and from what evidence*.

use serde::{Deserialize, Serialize};

use crate::domain::{DomainError, Identity, Rfc3339Timestamp};

/// Mandatory provenance frontmatter on every [`crate::domain::MemoryRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// Sensor identity that captured the source bytes.
    pub source_sensor: Identity,
    /// Wall-clock instant the record was first written.
    pub created_at: Rfc3339Timestamp,
    /// Agent or human who initiated the write. Distinct from the chain
    /// author when an upstream principal delegated.
    pub originating_agent_id: Identity,
    /// SHA-256 (or similar) hash of the source bytes the record was
    /// derived from. Format `<algo>:<hex>`. Validated for non-empty here;
    /// algo strength is the store layer's concern.
    pub source_hash: String,
    /// `consent.log` row id this write was authorized under.
    pub consent_ref: String,
    /// LLM identifier (model + revision) when the record was produced or
    /// summarized by a model. Optional — only present when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_id_if_any: Option<String>,
}

impl Provenance {
    /// Validate that every required component is present and non-empty.
    /// Wire-form validation already runs at deserialize time for `Identity`
    /// and `Rfc3339Timestamp`; this catches the string fields and surfaces
    /// uniform [`DomainError::MissingProvenance`] errors callers can match
    /// on regardless of construction path.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.source_hash.is_empty() {
            return Err(DomainError::MissingProvenance {
                field: "source_hash",
            });
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Provenance {
        Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            originating_agent_id: Identity::parse("agt:claude-code:opus-4-7:main:v1")
                .expect("valid"),
            source_hash: "sha256:abc123".to_owned(),
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
}
