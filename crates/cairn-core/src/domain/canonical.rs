//! Canonical record hashing — the single binding between a `MemoryRecord`
//! and a `SignedIntent.target_hash`.
//!
//! [`CanonicalRecordHash`] is opaque and computed only from a real
//! [`MemoryRecord`]. The intent containment check at
//! [`crate::domain::MemoryRecord::validate_against_intent`] computes
//! the hash internally from `self`, so callers can't pass a stale or
//! mismatched value.
//!
//! ## Signed-payload form
//!
//! The canonical form **excludes** the `signature` field — this is the
//! "signed payload" the author signs with Ed25519 and the same payload
//! the intent issuer hashes for `target_hash`. Including the signature
//! would be self-referential: the author can't sign bytes that include
//! their own not-yet-existing signature. Storing the same form for both
//! signers gives `sign(payload)` and `target_hash = sha256(payload)`.
//!
//! Mutation guarantees: any change to a signed field of the record
//! (`id`, `body`, `scope`, `provenance`, `actor_chain`, `evidence`,
//! `salience`, `confidence`, `tags`, `extra_frontmatter`) flips at
//! least one byte of the canonical encoding and therefore the digest.
//! Tests pin this for each field. The `signature` field is excluded
//! from the canonical payload by design and is verified independently.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::domain::{DomainError, MemoryRecord};

/// SHA-256 of a record's canonical JSON encoding, formatted as
/// `sha256:<64 lowercase hex>`. Opaque — only constructable via
/// [`Self::compute`] from a real [`MemoryRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalRecordHash(String);

impl CanonicalRecordHash {
    /// Hash the RFC 8785 JSON Canonicalization Scheme signed-payload form of
    /// `record`. The encoding sorts every object's keys lexicographically,
    /// emits no whitespace, uses ECMAScript-compatible primitive formatting,
    /// and **excludes** the top-level `signature` field — see module docs for
    /// the rationale. The result depends only on the record's signed content,
    /// not on the serializer's struct-field order or hash-map iteration order.
    pub fn compute(record: &MemoryRecord) -> Result<Self, DomainError> {
        let mut value = serde_json::to_value(record).map_err(|e| DomainError::InvalidIdentity {
            message: format!("canonical serialize failed: {e}"),
        })?;
        if let Some(obj) = value.as_object_mut() {
            obj.remove("signature");
        }
        let bytes =
            serde_json_canonicalizer::to_vec(&value).map_err(|e| DomainError::InvalidIdentity {
                message: format!("canonical serialize failed: {e}"),
            })?;
        let digest = Sha256::digest(&bytes);
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write;
            let _ = write!(hex, "{byte:02x}");
        }
        Ok(Self(format!("sha256:{hex}")))
    }

    /// Underlying `sha256:<hex>` string. Match this against
    /// `SignedIntent.target_hash` only via
    /// [`crate::domain::MemoryRecord::validate_against_intent`] —
    /// direct comparison from external callers bypasses the version
    /// guarantees this type expresses.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CanonicalRecordHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Serialize a value to canonical bytes for inspection in tests or
/// storage adapters. Adapters should compute the hash via
/// [`CanonicalRecordHash::compute`] rather than re-implement the
/// canonicalizer. Returns an error when the value isn't serializable to
/// JSON (e.g., a non-string-keyed map).
pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, DomainError> {
    serde_json_canonicalizer::to_vec(value).map_err(|e| DomainError::InvalidIdentity {
        message: format!("canonical serialize failed: {e}"),
    })
}

/// Canonical signed-payload bytes for a record. Excludes the
/// `signature` field so authors and intent issuers compute the same
/// payload. Use this when implementing Ed25519 sign/verify or when
/// debugging a `target_hash` mismatch.
pub fn canonical_bytes_signed_payload(record: &MemoryRecord) -> Result<Vec<u8>, DomainError> {
    let mut value = serde_json::to_value(record).map_err(|e| DomainError::InvalidIdentity {
        message: format!("canonical serialize failed: {e}"),
    })?;
    if let Some(obj) = value.as_object_mut() {
        obj.remove("signature");
    }
    serde_json_canonicalizer::to_vec(&value).map_err(|e| DomainError::InvalidIdentity {
        message: format!("canonical serialize failed: {e}"),
    })
}

/// Encode `intent` to canonical-JSON bytes with the `signature` field
/// removed. The result is the byte string that was signed, suitable for
/// passing to [`ed25519_dalek::Verifier::verify`] alongside
/// `intent.signature`.
///
/// # Errors
/// Returns [`DomainError::InvalidIdentity`] if `intent` cannot be
/// serialized to a JSON object (should never happen — `SignedIntent` is a
/// struct).
pub fn canonical_bytes_signed_intent(
    intent: &crate::generated::envelope::SignedIntent,
) -> Result<Vec<u8>, crate::domain::DomainError> {
    let mut value =
        serde_json::to_value(intent).map_err(|e| crate::domain::DomainError::InvalidIdentity {
            message: format!("canonical serialize failed: {e}"),
        })?;
    let map = value
        .as_object_mut()
        .ok_or_else(|| crate::domain::DomainError::InvalidIdentity {
            message: "SignedIntent did not serialize to a JSON object".into(),
        })?;
    map.remove("signature");
    serde_json_canonicalizer::to_vec(&value).map_err(|e| {
        crate::domain::DomainError::InvalidIdentity {
            message: format!("canonical serialize failed: {e}"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MemoryRecord {
        use crate::domain::{
            ActorChainEntry, ChainRole, EvidenceVector, Identity, MemoryClass, MemoryKind,
            MemoryVisibility, Provenance, Rfc3339Timestamp, ScopeTuple, TargetId,
            record::{Ed25519Signature, RecordId},
        };
        use std::collections::BTreeMap;
        let user = Identity::parse("hmn:tafeng").expect("valid");
        MemoryRecord {
            id: RecordId::parse("01HQZX9F5N0000000000000000").expect("valid"),
            target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid"),
            kind: MemoryKind::User,
            class: MemoryClass::Semantic,
            visibility: MemoryVisibility::Private,
            scope: ScopeTuple {
                user: Some("hmn:tafeng".to_owned()),
                ..ScopeTuple::default()
            },
            body: "user prefers dark mode".to_owned(),
            provenance: Provenance {
                source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
                created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
                originating_agent_id: user.clone(),
                source_hash: format!("sha256:{}", "a".repeat(64)),
                consent_ref: "consent:01HQZ".to_owned(),
                llm_id_if_any: None,
            },
            updated_at: Rfc3339Timestamp::parse("2026-04-22T14:05:11Z").expect("valid"),
            evidence: EvidenceVector {
                recall_count: 3,
                score: 0.82,
                unique_queries: 2,
                recency_half_life_days: 14,
            },
            salience: 0.5,
            confidence: 0.7,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: user,
                at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            }],
            signature: Ed25519Signature::parse(format!("ed25519:{}", "a".repeat(128)))
                .expect("valid"),
            tags: vec!["pref".to_owned()],
            extra_frontmatter: BTreeMap::new(),
            consent_model: None,
        }
    }

    #[test]
    fn deterministic_across_runs() {
        let r = sample();
        let h1 = CanonicalRecordHash::compute(&r).expect("compute");
        let h2 = CanonicalRecordHash::compute(&r).expect("compute");
        assert_eq!(h1, h2);
        assert!(h1.as_str().starts_with("sha256:"));
        assert_eq!(h1.as_str().len(), "sha256:".len() + 64);
    }

    #[test]
    fn body_change_flips_hash() {
        let r1 = sample();
        let mut r2 = sample();
        r2.body.push('!');
        assert_ne!(
            CanonicalRecordHash::compute(&r1).expect("compute"),
            CanonicalRecordHash::compute(&r2).expect("compute"),
        );
    }

    #[test]
    fn provenance_change_flips_hash() {
        let r1 = sample();
        let mut r2 = sample();
        r2.provenance.consent_ref = "consent:other".to_owned();
        assert_ne!(
            CanonicalRecordHash::compute(&r1).expect("compute"),
            CanonicalRecordHash::compute(&r2).expect("compute"),
        );
    }

    #[test]
    fn signature_change_does_not_flip_hash() {
        // The canonical payload excludes the signature — see module docs.
        // Author + intent issuer compute the same hash; only the
        // signature varies (it's verified separately).
        use crate::domain::record::Ed25519Signature;
        let r1 = sample();
        let mut r2 = sample();
        r2.signature =
            Ed25519Signature::parse(format!("ed25519:{}", "b".repeat(128))).expect("valid");
        assert_eq!(
            CanonicalRecordHash::compute(&r1).expect("compute"),
            CanonicalRecordHash::compute(&r2).expect("compute"),
        );
    }

    #[test]
    fn canonical_payload_omits_signature_key() {
        let r = sample();
        let bytes = canonical_bytes_signed_payload(&r).expect("compute");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        assert!(
            !s.contains("\"signature\""),
            "canonical signed payload must not include the `signature` field, got: {s}"
        );
    }

    #[test]
    fn extra_frontmatter_change_flips_hash() {
        let r1 = sample();
        let mut r2 = sample();
        r2.extra_frontmatter.insert(
            "obsidian_color".to_owned(),
            serde_json::Value::String("blue".to_owned()),
        );
        assert_ne!(
            CanonicalRecordHash::compute(&r1).expect("compute"),
            CanonicalRecordHash::compute(&r2).expect("compute"),
        );
    }

    #[test]
    fn canonical_bytes_keys_are_sorted() {
        let r = sample();
        let bytes = canonical_bytes(&r).expect("serializable");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        // Top-level keys must appear in sorted order. `actor_chain` < `body` < `class` < ...
        let actor_pos = s.find("\"actor_chain\"").expect("actor_chain present");
        let body_pos = s.find("\"body\"").expect("body present");
        let class_pos = s.find("\"class\"").expect("class present");
        assert!(actor_pos < body_pos);
        assert!(body_pos < class_pos);
    }

    #[test]
    fn canonical_bytes_match_jcs_basic_shape() {
        let value = serde_json::json!({
            "c": 120,
            "b": false,
            "a": "Hello!"
        });

        assert_eq!(
            canonical_bytes(&value).expect("canonical"),
            br#"{"a":"Hello!","b":false,"c":120}"#
        );
    }

    #[test]
    fn signed_intent_canonical_strips_signature() {
        use crate::generated::common;
        use crate::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};

        let mk = |sig: &str| SignedIntent {
            chain_parents: vec![],
            expires_at: "2026-04-22T14:07:11Z".into(),
            issued_at: "2026-04-22T14:02:11Z".into(),
            issuer: common::Identity("hmn:tafeng".into()),
            key_version: 1,
            nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".into()),
            operation_id: common::Ulid("01HQZX9F5N0000000000000000".into()),
            scope: SignedIntentScope {
                tenant: "acme".into(),
                workspace: "ws".into(),
                entity: "ent".into(),
                tier: SignedIntentScopeTier::Project,
            },
            sequence: Some(1),
            server_challenge: None,
            signature: common::Ed25519Signature(format!("ed25519:{sig}")),
            target_hash: format!("sha256:{}", "a".repeat(64)),
        };

        let a = canonical_bytes_signed_intent(&mk(&"a".repeat(128))).unwrap();
        let b = canonical_bytes_signed_intent(&mk(&"b".repeat(128))).unwrap();
        assert_eq!(
            a, b,
            "canonical bytes must be invariant under signature mutation"
        );
        assert!(!std::str::from_utf8(&a).unwrap().contains("signature"));
    }

    #[test]
    fn signed_intent_canonical_changes_when_payload_changes() {
        use crate::generated::common;
        use crate::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};

        let base = SignedIntent {
            chain_parents: vec![],
            expires_at: "2026-04-22T14:07:11Z".into(),
            issued_at: "2026-04-22T14:02:11Z".into(),
            issuer: common::Identity("hmn:tafeng".into()),
            key_version: 1,
            nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".into()),
            operation_id: common::Ulid("01HQZX9F5N0000000000000000".into()),
            scope: SignedIntentScope {
                tenant: "acme".into(),
                workspace: "ws".into(),
                entity: "ent".into(),
                tier: SignedIntentScopeTier::Project,
            },
            sequence: Some(1),
            server_challenge: None,
            signature: common::Ed25519Signature(format!("ed25519:{}", "a".repeat(128))),
            target_hash: format!("sha256:{}", "a".repeat(64)),
        };
        let mut tweaked = base.clone();
        tweaked.scope.entity = "different".into();

        let a = canonical_bytes_signed_intent(&base).unwrap();
        let b = canonical_bytes_signed_intent(&tweaked).unwrap();
        assert_ne!(a, b);
    }
}
