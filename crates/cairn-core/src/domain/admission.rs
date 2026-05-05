//! [`SignedAdmission`] — typed proof that a verified envelope's
//! `target_hash` covers the actual `(kind, plan_ref, payload)` triple
//! the caller is about to stage in the WAL ledger.
//!
//! # Spec note: `target_hash` is domain-separated by `(kind, plan_ref)`
//!
//! Brief §4.2 originally describes `SignedIntent.target_hash` as
//! `sha256(record / plan / receipt)` — payload-only. Issue #52
//! tightens that contract: replay admission requires `target_hash`
//! to be `sha256(canonical_bytes(kind, plan_ref, payload))`, so the
//! same payload signed for `kind=upsert` cannot be replayed under
//! `kind=delete` (round-13 review #1). [`derive_target_hash`] is the
//! single canonical helper both signers and admission callers use to
//! produce that hash.
//!
//! # Construction
//!
//! [`SignedAdmission::new`] recomputes the canonical hash from the
//! caller-supplied `(kind, plan_ref, payload)` and asserts equality
//! with `intent.target_hash`. The store accepts only this type, so a
//! buggy or adversarial verb-layer caller that mixed up `kind` or
//! `plan_ref` cannot mint the admission token in the first place —
//! replay state is never consumed.

use crate::domain::VerifiedSignedIntent;
use sha2::{Digest, Sha256};

/// Closed enum for `wal_ops.kind` — mirrors the CHECK constraint from
/// migration 0002 (widened by 0041). Stored as a `&'static str` so
/// the `SQLite` column lookup matches verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WalActionKind {
    /// Memory record upsert (verbs: `ingest`, `summarize` with `persist`, etc.).
    Upsert,
    /// Memory record delete tombstone.
    Delete,
    /// Promote a memory record to a wider visibility tier.
    Promote,
    /// Expire a memory record (TTL-driven).
    Expire,
    /// Forget a single session's records.
    ForgetSession,
    /// Forget a single record.
    ForgetRecord,
    /// In-place evolve / recompute of a record's content.
    Evolve,
}

impl WalActionKind {
    /// Wire-format string matching the `wal_ops.kind` CHECK constraint.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Delete => "delete",
            Self::Promote => "promote",
            Self::Expire => "expire",
            Self::ForgetSession => "forget_session",
            Self::ForgetRecord => "forget_record",
            Self::Evolve => "evolve",
        }
    }
}

/// Errors that prevent minting a [`SignedAdmission`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AdmissionError {
    /// Recomputed `sha256(payload)` did not match the signed
    /// `target_hash`. Either the caller passed the wrong payload, the
    /// wrong `kind`, or the verified intent does not actually
    /// authorize this operation.
    #[error("target_hash mismatch: signed={signed} computed_for(kind={kind:?})={computed}")]
    TargetHashMismatch {
        /// The `target_hash` from the verified envelope.
        signed: String,
        /// The kind the caller nominated.
        kind: WalActionKind,
        /// What the constructor recomputed from the payload.
        computed: String,
    },
}

/// A `VerifiedSignedIntent` paired with the WAL action and plan
/// reference the issuer's `target_hash` covers. The store consumes
/// only this type, so a downstream caller cannot stage replay state
/// for one `kind` under a signature meant for another.
///
/// **Construction** is gated on a content-addressed hash recomputation:
/// `SignedAdmission::new(verified, kind, plan_ref, payload)` computes
/// `sha256(payload)`, compares it to `verified.target_hash`, and only
/// returns `Ok` when they match byte-for-byte. The pair `(verified,
/// kind, plan_ref)` is then sealed inside the struct.
#[derive(Debug, Clone)]
pub struct SignedAdmission {
    intent: VerifiedSignedIntent,
    kind: WalActionKind,
    plan_ref: Option<String>,
}

impl SignedAdmission {
    /// Mint a [`SignedAdmission`] by proving the caller's `(kind,
    /// payload)` tuple hashes to the same `target_hash` the issuer
    /// signed.
    ///
    /// `payload` is the canonical bytes of the record / plan / receipt
    /// the verb staged. The constructor computes `sha256(payload)` and
    /// asserts it equals `intent.as_inner().target_hash` (after the
    /// `sha256:` prefix). A mismatch returns
    /// [`AdmissionError::TargetHashMismatch`].
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError::TargetHashMismatch`] when the
    /// recomputed hash does not match the signed `target_hash`.
    pub fn new(
        intent: VerifiedSignedIntent,
        kind: WalActionKind,
        plan_ref: Option<String>,
        payload: &[u8],
    ) -> Result<Self, AdmissionError> {
        let computed = derive_target_hash(kind, plan_ref.as_deref(), payload);
        let signed = &intent.as_inner().target_hash;
        if &computed != signed {
            return Err(AdmissionError::TargetHashMismatch {
                signed: signed.clone(),
                kind,
                computed,
            });
        }
        Ok(Self {
            intent,
            kind,
            plan_ref,
        })
    }

    /// Borrow the verified envelope (read-only).
    #[must_use]
    pub fn intent(&self) -> &VerifiedSignedIntent {
        &self.intent
    }

    /// The WAL action the issuer's `target_hash` authorized.
    #[must_use]
    pub fn kind(&self) -> WalActionKind {
        self.kind
    }

    /// Optional `plan_ref` ULID — passed through verbatim to `wal_ops`.
    #[must_use]
    pub fn plan_ref(&self) -> Option<&str> {
        self.plan_ref.as_deref()
    }
}

/// Canonical replay `target_hash` for issue #52: domain-separated
/// `sha256` over `(kind, plan_ref, payload)`. Both signers and
/// admission callers MUST use this helper so `target_hash` binds the
/// WAL action plus its plan reference, not just the payload bytes.
///
/// Encoding (length-prefixed, ASCII-numeric for unambiguous parse):
/// ```text
/// "cairn.replay.target_hash.v1\n"
/// "kind=" <kind_db_str> "\n"
/// "plan_ref=" <"" | plan_ref_ulid> "\n"
/// "payload_len=" <decimal> "\n"
/// payload_bytes
/// ```
///
/// The `\n` terminators + the `payload_len` line make the encoding
/// prefix-free, so concatenation cannot collide across distinct
/// `(kind, plan_ref, payload)` triples. Returns the IDL
/// `Nonce16Base64`-shaped string `"sha256:<64 lowercase hex>"`.
#[must_use]
pub fn derive_target_hash(kind: WalActionKind, plan_ref: Option<&str>, payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"cairn.replay.target_hash.v1\n");
    hasher.update(b"kind=");
    hasher.update(kind.as_db_str().as_bytes());
    hasher.update(b"\n");
    hasher.update(b"plan_ref=");
    if let Some(p) = plan_ref {
        hasher.update(p.as_bytes());
    }
    hasher.update(b"\n");
    hasher.update(b"payload_len=");
    hasher.update(payload.len().to_string().as_bytes());
    hasher.update(b"\n");
    hasher.update(payload);
    let digest = hasher.finalize();
    format!("sha256:{}", hex_lower(&digest))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::common::{Ed25519Signature, Identity, Nonce16Base64, Ulid};
    use crate::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};

    fn intent_with_target(target_hash: &str) -> VerifiedSignedIntent {
        let raw = SignedIntent {
            chain_parents: vec![],
            expires_at: "2026-04-22T14:07:11Z".into(),
            issued_at: "2026-04-22T14:02:11Z".into(),
            issuer: Identity("hmn:tafeng".into()),
            key_version: 1,
            nonce: Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".into()),
            operation_id: Ulid("01HQZX9F5N0000000000000000".into()),
            scope: SignedIntentScope {
                tenant: "acme".into(),
                workspace: "ws".into(),
                entity: "ent".into(),
                tier: SignedIntentScopeTier::Project,
            },
            sequence: Some(1),
            server_challenge: None,
            signature: Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
            target_hash: target_hash.into(),
        };
        VerifiedSignedIntent::from_verified_for_test(raw)
    }

    #[test]
    fn new_accepts_matching_payload() {
        let payload = b"hello, world";
        let target = derive_target_hash(WalActionKind::Upsert, None, payload);
        let verified = intent_with_target(&target);
        let admission = SignedAdmission::new(verified, WalActionKind::Upsert, None, payload)
            .expect("matching payload must mint");
        assert_eq!(admission.kind(), WalActionKind::Upsert);
        assert!(admission.plan_ref().is_none());
    }

    #[test]
    fn new_rejects_mismatched_payload() {
        let payload = b"hello, world";
        let target = derive_target_hash(WalActionKind::Upsert, None, b"different bytes");
        let verified = intent_with_target(&target);
        let err = SignedAdmission::new(verified, WalActionKind::Upsert, None, payload)
            .expect_err("mismatch must reject");
        assert!(matches!(err, AdmissionError::TargetHashMismatch { .. }));
    }

    #[test]
    fn new_rejects_kind_substitution() {
        // Round-13 review #1: same payload, signed for one kind cannot
        // be replayed under a different kind.
        let payload = b"some record body";
        let signed_target = derive_target_hash(WalActionKind::Upsert, None, payload);
        let verified = intent_with_target(&signed_target);
        // Caller tries to admit under `Delete` for a payload signed
        // for `Upsert` — domain separation makes the digest differ.
        let err = SignedAdmission::new(verified, WalActionKind::Delete, None, payload)
            .expect_err("kind substitution must reject");
        assert!(matches!(err, AdmissionError::TargetHashMismatch { .. }));
    }

    #[test]
    fn new_rejects_plan_ref_substitution() {
        // Same payload, same kind, but a different plan_ref → different
        // target_hash. Domain separation prevents an attacker from
        // re-targeting the signed authorization to a different plan.
        let payload = b"plan body";
        let signed_target = derive_target_hash(
            WalActionKind::Upsert,
            Some("01HQZX9F5N00000000000000PA"),
            payload,
        );
        let verified = intent_with_target(&signed_target);
        let err = SignedAdmission::new(
            verified,
            WalActionKind::Upsert,
            Some("01HQZX9F5N00000000000000PB".into()), // different plan
            payload,
        )
        .expect_err("plan_ref substitution must reject");
        assert!(matches!(err, AdmissionError::TargetHashMismatch { .. }));
    }

    #[test]
    fn new_rejects_payload_length_substitution() {
        // Belt and braces: the length prefix is part of the canonical
        // encoding, so a payload that happens to share a prefix with
        // a longer signed payload still fails.
        let signed_target = derive_target_hash(WalActionKind::Upsert, None, b"longer payload");
        let verified = intent_with_target(&signed_target);
        let err = SignedAdmission::new(verified, WalActionKind::Upsert, None, b"longer")
            .expect_err("length substitution must reject");
        assert!(matches!(err, AdmissionError::TargetHashMismatch { .. }));
    }

    #[test]
    fn kind_db_str_matches_wal_ops_check() {
        // Mirrors migration 0002's CHECK list (widened by 0041).
        assert_eq!(WalActionKind::Upsert.as_db_str(), "upsert");
        assert_eq!(WalActionKind::Delete.as_db_str(), "delete");
        assert_eq!(WalActionKind::Promote.as_db_str(), "promote");
        assert_eq!(WalActionKind::Expire.as_db_str(), "expire");
        assert_eq!(WalActionKind::ForgetSession.as_db_str(), "forget_session");
        assert_eq!(WalActionKind::ForgetRecord.as_db_str(), "forget_record");
        assert_eq!(WalActionKind::Evolve.as_db_str(), "evolve");
    }
}
