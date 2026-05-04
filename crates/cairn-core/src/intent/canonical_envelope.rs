//! RFC 8785 (JCS) canonical-JSON serializer for the signed payload.
//!
//! Input: a parsed [`crate::generated::envelope::SignedIntent`].
//! Output: the deterministic byte sequence that
//! [`crate::verifier::verify_signed_intent`] passes to Ed25519 verify.
//!
//! Excludes the `signature` field — the signed payload covers every
//! *other* envelope field (brief §4.2: "signature is over the canonical
//! JSON of ALL fields above").

use serde_json::{Value, json};

use crate::generated::envelope::SignedIntent;
use crate::intent::VerifyError;

/// Build the canonical JSON byte representation of `intent` minus
/// `signature`. Used as the input to Ed25519 verify and as the input to
/// any future signer in `cairn-core`.
///
/// # Errors
/// Returns [`VerifyError::Malformed`] with `field = "envelope"` if any
/// intermediate `serde_json::to_value` call (for `chain_parents` or
/// `scope`) or the final `serde_jcs::to_vec` call fails. All three
/// paths are unreachable for structurally valid `SignedIntent` values
/// — guarded purely as defense in depth.
pub fn canonicalize_signed_payload(intent: &SignedIntent) -> Result<Vec<u8>, VerifyError> {
    let mut map = serde_json::Map::new();
    map.insert("chain_parents".into(), serde_json::to_value(&intent.chain_parents).map_err(envelope_err)?);
    map.insert("expires_at".into(), Value::String(intent.expires_at.clone()));
    map.insert("issued_at".into(), Value::String(intent.issued_at.clone()));
    map.insert("issuer".into(), Value::String(intent.issuer.0.clone()));
    map.insert("key_version".into(), json!(intent.key_version));
    map.insert("nonce".into(), Value::String(intent.nonce.0.clone()));
    map.insert("operation_id".into(), Value::String(intent.operation_id.0.clone()));
    map.insert("scope".into(), serde_json::to_value(&intent.scope).map_err(envelope_err)?);
    if let Some(seq) = intent.sequence {
        map.insert("sequence".into(), json!(seq));
    }
    if let Some(c) = &intent.server_challenge {
        map.insert("server_challenge".into(), Value::String(c.0.clone()));
    }
    map.insert("target_hash".into(), Value::String(intent.target_hash.clone()));
    let value = Value::Object(map);
    serde_jcs::to_vec(&value).map_err(envelope_err)
}

fn envelope_err<E: std::fmt::Display>(e: E) -> VerifyError {
    VerifyError::Malformed { field: "envelope", reason: e.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::common;
    use crate::generated::envelope::{SignedIntentScope, SignedIntentScopeTier};

    fn good_intent() -> SignedIntent {
        SignedIntent {
            chain_parents: vec![],
            expires_at: "2026-04-22T14:07:11Z".to_owned(),
            issued_at: "2026-04-22T14:02:11Z".to_owned(),
            issuer: common::Identity("hmn:tafeng".to_owned()),
            key_version: 1,
            nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".to_owned()),
            operation_id: common::Ulid("01HQZX9F5N0000000000000000".to_owned()),
            scope: SignedIntentScope {
                tenant: "acme".to_owned(),
                workspace: "ws".to_owned(),
                entity: "ent".to_owned(),
                tier: SignedIntentScopeTier::Project,
            },
            sequence: Some(1),
            server_challenge: None,
            signature: common::Ed25519Signature(format!("ed25519:{}", "a".repeat(128))),
            target_hash: format!("sha256:{}", "a".repeat(64)),
        }
    }

    #[test]
    fn canonical_output_excludes_signature() {
        let bytes = canonicalize_signed_payload(&good_intent()).expect("ok");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        assert!(!s.contains("signature"), "canonical payload must not contain signature; got: {s}");
        assert!(!s.contains("ed25519:"));
    }

    #[test]
    fn canonical_output_is_deterministic_across_calls() {
        let a = canonicalize_signed_payload(&good_intent()).expect("a");
        let b = canonicalize_signed_payload(&good_intent()).expect("b");
        assert_eq!(a, b);
    }

    #[test]
    fn canonical_output_keys_are_sorted() {
        let bytes = canonicalize_signed_payload(&good_intent()).expect("ok");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        let chain_idx = s.find("\"chain_parents\"").expect("chain_parents present");
        let expires_idx = s.find("\"expires_at\"").expect("expires_at present");
        let target_idx = s.find("\"target_hash\"").expect("target_hash present");
        assert!(chain_idx < expires_idx, "chain_parents before expires_at");
        assert!(expires_idx < target_idx, "expires_at before target_hash");
    }

    #[test]
    fn omits_optional_fields_when_none() {
        let mut i = good_intent();
        i.sequence = None;
        i.server_challenge = Some(common::Nonce16Base64("BBBBBBBBBBBBBBBBBBBBBA==".to_owned()));
        let bytes = canonicalize_signed_payload(&i).expect("ok");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        assert!(!s.contains("\"sequence\""), "sequence absent");
        assert!(s.contains("\"server_challenge\""), "server_challenge present");
    }

    #[test]
    fn flipping_a_field_changes_canonical_bytes() {
        let baseline = canonicalize_signed_payload(&good_intent()).expect("baseline");
        let mut mutated = good_intent();
        mutated.target_hash = format!("sha256:{}", "b".repeat(64));
        let after = canonicalize_signed_payload(&mutated).expect("mutated");
        assert_ne!(baseline, after, "canonical bytes must change when target_hash changes");
    }
}
