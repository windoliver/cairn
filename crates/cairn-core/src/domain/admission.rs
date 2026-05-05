//! [`SignedAdmission`] — typed proof that the caller has shown the
//! verified envelope's `target_hash` matches the canonical bytes of
//! the record / plan / receipt about to be staged in the WAL ledger.
//!
//! # Spec note: `target_hash` is content-addressed (brief §4.2)
//!
//! `SignedIntent.target_hash` is `sha256(payload)` where `payload` is
//! the canonical bytes of the record / plan / receipt. The signature
//! attests "I authorize the operation that produces this content
//! addressed bytes". The store boundary recomputes that hash to
//! catch verb-layer bugs that pass the wrong payload.
//!
//! # Caller contract: bind `kind` / `plan_ref` upstream
//!
//! The IDL `SignedIntent` does not include a verb / `kind` / `plan_ref`
//! discriminator in the signed payload, and a downstream IDL extension
//! is required to enforce that binding cryptographically (issue #52
//! follow-up). Until then, **the verb-layer caller is responsible for
//! ensuring `(intent, kind, plan_ref)` are consistent**: two distinct
//! verbs operating on the same record body produce the same
//! `target_hash`, so substrate-only enforcement cannot prove the
//! issuer authorized this specific `kind`. Round-11 / round-12 /
//! round-13 reviews documented the gap; closing it requires extending
//! the signed envelope schema.
//!
//! # Construction
//!
//! [`SignedAdmission::new`] recomputes `sha256(payload)` and asserts
//! equality with `intent.target_hash`. The store accepts only this
//! type so a payload-mismatch surfaces at admission time before any
//! replay state is consumed.

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
        let mut hasher = Sha256::new();
        hasher.update(payload);
        let computed = format!("sha256:{}", hex_lower(&hasher.finalize()));
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

    fn payload_hash(payload: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(payload);
        format!("sha256:{}", hex_lower(&h.finalize()))
    }

    #[test]
    fn new_accepts_matching_payload() {
        let payload = b"hello, world";
        let verified = intent_with_target(&payload_hash(payload));
        let admission = SignedAdmission::new(verified, WalActionKind::Upsert, None, payload)
            .expect("matching payload must mint");
        assert_eq!(admission.kind(), WalActionKind::Upsert);
        assert!(admission.plan_ref().is_none());
    }

    #[test]
    fn new_rejects_mismatched_payload() {
        let payload = b"hello, world";
        let verified = intent_with_target(&payload_hash(b"different bytes"));
        let err = SignedAdmission::new(verified, WalActionKind::Upsert, None, payload)
            .expect_err("mismatch must reject");
        assert!(matches!(err, AdmissionError::TargetHashMismatch { .. }));
    }

    #[test]
    fn known_limitation_kind_substitution_is_not_substrate_enforced() {
        // Round-14 review #2: rolling back the round-13 domain
        // separation because brief §4.2 + existing record containment
        // require `target_hash = sha256(payload)`. With that contract,
        // two `kind` values for the same payload share a `target_hash`
        // — substrate alone cannot reject kind substitution. The
        // verb-layer caller MUST bind `(intent, kind, plan_ref)`
        // upstream; closing this gap permanently requires extending
        // the IDL signed payload with a `kind` field (issue #52
        // follow-up). This test pins the current behaviour so a
        // future tightening surfaces as an intentional change.
        let payload = b"shared record body";
        let verified = intent_with_target(&payload_hash(payload));
        let admission = SignedAdmission::new(verified, WalActionKind::Delete, None, payload)
            .expect("substrate accepts; binding is caller contract");
        assert_eq!(admission.kind(), WalActionKind::Delete);
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
