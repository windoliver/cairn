//! Shared helpers for building synthesized records emitted by workflow
//! handlers (dream / evaluation / consolidation share the same trust
//! boundary: no author key, deterministic `target_id`, flush-mutated
//! sentinel signature).
//!
//! Consolidation pre-dates this helper and inlines its own copies; new
//! workflows that need to upsert a system-generated record should use
//! [`build_synthetic_record`].

use cairn_core::domain::{
    ActorChainEntry, ChainRole, EvidenceVector, Identity, Provenance, Rfc3339Timestamp, ScopeTuple,
    TargetId,
    record::{Ed25519Signature, MemoryRecord, RecordId},
    taxonomy::{MemoryClass, MemoryKind, MemoryVisibility},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Inputs to [`build_synthetic_record`]. The caller supplies the
/// content (`kind`, `class`, `body`, `target_key`, `extras`) and the
/// helper handles everything that is invariant across system-generated
/// records: a fresh record id, deterministic `target_id`, current
/// timestamp, the flush-mutated sentinel signature, and the actor +
/// provenance shape.
pub struct SyntheticRecordSpec<'a> {
    /// `MemoryKind` for the new record.
    pub kind: MemoryKind,
    /// `MemoryClass` for the new record.
    pub class: MemoryClass,
    /// Final scope on the record (workflows fold the bound scope's
    /// tenant/workspace dims into this and add `session_id` when
    /// session-scoped).
    pub scope: ScopeTuple,
    /// Markdown body — what the LLM/check produced.
    pub body: String,
    /// Logical key used to derive the deterministic `target_id`. Two
    /// invocations with the same key produce the same `target_id`,
    /// which the store's body-hash dedupe pairs with to make replay
    /// idempotent.
    pub target_key: &'a str,
    /// JSON-shaped extra frontmatter — `extra_frontmatter` on the
    /// emitted record.
    pub extras: BTreeMap<String, serde_json::Value>,
    /// Stable agent identity used as the author and the originating
    /// agent in `Provenance`. Workflows pass their per-workflow
    /// `agt:cairn-workflows:<name>-handler:v1` literal.
    pub agent_id: &'a str,
    /// Sensor identity used in `Provenance.source_sensor`. Same shape
    /// as `agent_id` but with the `snr:` prefix.
    pub sensor_id: &'a str,
    /// `consent_ref` for system-generated records.
    pub consent_ref: &'a str,
    /// Optional ULID generator hook for deterministic tests. Production
    /// callers pass `None` and the helper generates one via
    /// [`ulid::Ulid::new`].
    pub record_id_override: Option<String>,
}

/// Build a synthesized record per `spec`. The returned record passes
/// `MemoryRecord::validate` for the workflow trust boundary.
///
/// # Errors
/// Returns the wrapped domain error if any embedded identity or id
/// fails to parse — every literal call site passes static, well-formed
/// inputs, so the runtime path never trips this in production.
pub fn build_synthetic_record(
    spec: SyntheticRecordSpec<'_>,
) -> Result<MemoryRecord, Box<dyn std::error::Error + Send + Sync>> {
    let record_id_str = spec
        .record_id_override
        .unwrap_or_else(|| ulid::Ulid::new().to_string());
    let id = RecordId::parse(&record_id_str)?;

    let target_id = stable_target_id(spec.target_key)?;

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    #[allow(
        clippy::cast_possible_wrap,
        reason = "unix epoch seconds fit in i64 for ~292 billion years"
    )]
    let now_ts = Rfc3339Timestamp::from_unix_secs(now_secs as i64)?;

    let agent = Identity::parse(spec.agent_id)?;
    let sensor = Identity::parse(spec.sensor_id)?;

    let source_hash = sha256_hex(spec.body.as_bytes());
    let self_source = cairn_core::domain::SourceId::parse(target_id.as_str().to_owned())?;
    let provenance = Provenance {
        source_sensor: sensor,
        created_at: now_ts.clone(),
        originating_agent_id: agent.clone(),
        source_hash: format!("sha256:{source_hash}"),
        consent_ref: spec.consent_ref.to_owned(),
        llm_id_if_any: None,
        source_ids: vec![self_source],
        source_refs: Vec::new(),
    };

    let actor_chain = vec![ActorChainEntry {
        role: ChainRole::Author,
        identity: agent,
        at: now_ts.clone(),
    }];

    Ok(MemoryRecord {
        id,
        target_id,
        kind: spec.kind,
        class: spec.class,
        visibility: MemoryVisibility::Private,
        scope: spec.scope,
        body: spec.body,
        provenance,
        updated_at: now_ts,
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain,
        signature: Ed25519Signature::flush_mutated_sentinel(),
        tags: Vec::new(),
        source_ids: Vec::new(),
        extra_frontmatter: spec.extras,
        consent_model: None,
    })
}

/// Derive a deterministic, ULID-shaped [`TargetId`] from a logical key.
///
/// Matches the algorithm used in
/// `consolidation::handler::stable_target_id`: SHA-256, take the first
/// 16 bytes as a 128-bit value, clear the top 3 bits so the leading
/// Crockford symbol stays `<= 7`, encode as 26-char Crockford base32.
///
/// # Errors
/// Returns the wrapped domain error if the encoded string fails
/// `TargetId::parse` — unreachable for any 26-char Crockford-base32
/// string produced by [`encode_crockford_base32_128`].
pub fn stable_target_id(
    target_key: &str,
) -> Result<TargetId, Box<dyn std::error::Error + Send + Sync>> {
    let hash = sha256_hex(target_key.as_bytes());
    let hi = u64::from_str_radix(&hash[..16], 16)?;
    let lo = u64::from_str_radix(&hash[16..32], 16)?;
    let hi_masked = hi & 0x1FFF_FFFF_FFFF_FFFF_u64;
    let ulid_str = encode_crockford_base32_128(hi_masked, lo);
    Ok(TargetId::parse(ulid_str)?)
}

/// SHA-256 of `bytes`, returned as lowercase hex.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

const CROCKFORD_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Encode a 128-bit value `(hi, lo)` as a 26-character Crockford base32
/// string (matches the ULID alphabet — no `I L O U`).
#[must_use]
pub fn encode_crockford_base32_128(hi: u64, lo: u64) -> String {
    // 128 bits → 26 base32 symbols (5 bits each); the leading symbol uses
    // only 3 bits (the top 3 bits of `hi` must already be zero).
    let mut out = [0_u8; 26];
    let mut value: u128 = (u128::from(hi) << 64) | u128::from(lo);
    for slot in out.iter_mut().rev() {
        let idx = (value & 0x1f) as usize;
        *slot = CROCKFORD_ALPHABET[idx];
        value >>= 5;
    }
    String::from_utf8(out.to_vec()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_target_id_is_deterministic() {
        let a = stable_target_id("workflow:test:key").expect("encode");
        let b = stable_target_id("workflow:test:key").expect("encode");
        assert_eq!(a.as_str(), b.as_str());
    }

    #[test]
    fn stable_target_id_distinguishes_keys() {
        let a = stable_target_id("dream:s1:0").expect("encode");
        let b = stable_target_id("dream:s1:1").expect("encode");
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn encode_crockford_uses_ulid_alphabet() {
        let s = encode_crockford_base32_128(0, 0);
        assert_eq!(s.len(), 26);
        assert!(s.bytes().all(|b| CROCKFORD_ALPHABET.contains(&b)));
    }
}
