//! [`SignedAdmission`] — typed proof that a verified envelope's
//! `target_hash` covers the actual `(kind, payload, plan_ref)` tuple
//! the caller is about to stage in the WAL ledger.
//!
//! Brief context (§4.2): `SignedIntent.target_hash` is `sha256(record /
//! plan / receipt)` — content-addressed binding. The signature attests
//! "I authorize the operation that produces `target_hash=H`", but the
//! signed payload does not include a `kind` discriminator. A buggy or
//! adversarial verb-layer caller that mixed up `kind` could otherwise
//! consume sequence/challenge state under a verified signature for a
//! different `kind` than the issuer signed for.
//!
//! [`SignedAdmission::new`] closes that gap by **recomputing**
//! `sha256(payload)` server-side and asserting equality with the
//! signed `intent.target_hash`. The constructor is the only path that
//! mints a `SignedAdmission`; the store accepts only this type, so a
//! `kind` / `plan_ref` mismatch surfaces at admission time before any
//! replay state is consumed (issue #52 round-12 review #1).

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
        let digest = hasher.finalize();
        let computed = format!("sha256:{}", hex_lower(&digest));
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

    #[test]
    fn new_accepts_matching_payload() {
        let payload = b"hello, world";
        let mut h = Sha256::new();
        h.update(payload);
        let target = format!("sha256:{}", hex_lower(&h.finalize()));
        let verified = intent_with_target(&target);
        let admission = SignedAdmission::new(verified, WalActionKind::Upsert, None, payload)
            .expect("matching payload must mint");
        assert_eq!(admission.kind(), WalActionKind::Upsert);
        assert!(admission.plan_ref().is_none());
    }

    #[test]
    fn new_rejects_mismatched_payload() {
        let payload = b"hello, world";
        let mut h = Sha256::new();
        h.update(b"different bytes");
        let target = format!("sha256:{}", hex_lower(&h.finalize()));
        let verified = intent_with_target(&target);
        let err = SignedAdmission::new(verified, WalActionKind::Upsert, None, payload)
            .expect_err("mismatch must reject");
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
