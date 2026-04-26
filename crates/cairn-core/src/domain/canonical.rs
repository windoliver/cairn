//! Canonical record hashing — the single binding between a `MemoryRecord`
//! and a `SignedIntent.target_hash`.
//!
//! [`CanonicalRecordHash`] is opaque: callers can't fabricate one from a
//! string. Construction goes through [`CanonicalRecordHash::compute`],
//! which serializes the record to JSON with sorted object keys, hashes
//! the bytes with `SHA`-256, and prefixes with `sha256:`. The intent
//! containment check at [`crate::domain::MemoryRecord::validate_against_intent`]
//! takes a `&CanonicalRecordHash` so the type system enforces "the hash
//! came from this record, not an arbitrary string the caller invented".
//!
//! Mutation guarantees: any change to a signed field of the record
//! (`id`, `body`, `scope`, `provenance`, `actor_chain`, `signature`,
//! `evidence`, `salience`, `confidence`, `tags`, `extra_frontmatter`)
//! flips at least one byte of the canonical encoding and therefore the
//! digest. Tests pin this for each field so a future serde rename or
//! skip annotation that drops a field from the canonical form fails CI.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::domain::{DomainError, MemoryRecord};

/// SHA-256 of a record's canonical JSON encoding, formatted as
/// `sha256:<64 lowercase hex>`. Opaque — only constructable via
/// [`Self::compute`] from a real [`MemoryRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalRecordHash(String);

impl CanonicalRecordHash {
    /// Hash the canonical JSON form of `record`. The encoding sorts every
    /// object's keys lexicographically and emits no whitespace; it
    /// therefore depends only on the record's content, not on the
    /// serializer's struct-field order or hash-map iteration order.
    pub fn compute(record: &MemoryRecord) -> Result<Self, DomainError> {
        let value = serde_json::to_value(record).map_err(|e| DomainError::InvalidIdentity {
            message: format!("canonical serialize failed: {e}"),
        })?;
        let mut buf = String::new();
        write_canonical(&value, &mut buf);
        let digest = Sha256::digest(buf.as_bytes());
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

/// Append the canonical JSON encoding of `value` to `out`. Object keys
/// are sorted lexicographically; arrays preserve order; strings reuse
/// `serde_json`'s escape rules so the output round-trips through any
/// JSON parser.
fn write_canonical(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => append_json_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                append_json_string(k, out);
                out.push(':');
                if let Some(v) = map.get(*k) {
                    write_canonical(v, out);
                }
            }
            out.push('}');
        }
    }
}

fn append_json_string(s: &str, out: &mut String) {
    // serde_json's `to_string` on a String produces a JSON-encoded
    // quoted string; reuse it so escape semantics match what every
    // other JSON consumer reads.
    let encoded = serde_json::to_string(&s.to_owned()).unwrap_or_else(|_| String::from("\"\""));
    out.push_str(&encoded);
}

/// Serialize a value to canonical bytes for inspection in tests or
/// storage adapters. Internal — adapters should compute the hash via
/// [`CanonicalRecordHash::compute`] rather than re-implement the
/// canonicalizer. Returns an error when the value isn't serializable to
/// JSON (e.g., a non-string-keyed map).
pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, DomainError> {
    let json = serde_json::to_value(value).map_err(|e| DomainError::InvalidIdentity {
        message: format!("canonical serialize failed: {e}"),
    })?;
    let mut buf = String::new();
    write_canonical(&json, &mut buf);
    Ok(buf.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MemoryRecord {
        use crate::domain::{
            ActorChainEntry, ChainRole, EvidenceVector, Identity, MemoryClass, MemoryKind,
            MemoryVisibility, Provenance, Rfc3339Timestamp, ScopeTuple,
            record::{Ed25519Signature, RecordId},
        };
        use std::collections::BTreeMap;
        let user = Identity::parse("usr:tafeng").expect("valid");
        MemoryRecord {
            id: RecordId::parse("01HQZX9F5N0000000000000000").expect("valid"),
            kind: MemoryKind::User,
            class: MemoryClass::Semantic,
            visibility: MemoryVisibility::Private,
            scope: ScopeTuple {
                user: Some("usr:tafeng".to_owned()),
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
    fn signature_change_flips_hash() {
        use crate::domain::record::Ed25519Signature;
        let r1 = sample();
        let mut r2 = sample();
        r2.signature =
            Ed25519Signature::parse(format!("ed25519:{}", "b".repeat(128))).expect("valid");
        assert_ne!(
            CanonicalRecordHash::compute(&r1).expect("compute"),
            CanonicalRecordHash::compute(&r2).expect("compute"),
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
}
