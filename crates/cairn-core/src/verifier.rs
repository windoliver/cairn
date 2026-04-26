//! `SignedIntent` verifier — the single production path that mints
//! [`crate::domain::VerifiedSignedIntent`] tokens.
//!
//! [`verify_signed_intent`] is the public entry point; adapters call
//! it once at the trust boundary and pass the resulting token to
//! [`crate::domain::MemoryRecord::validate_against_intent`].
//!
//! ## P0 status
//!
//! At P0 this is a **placeholder** that performs only syntactic checks:
//! issuer identity parses, `target_hash` is `sha256:<hex>`, and the
//! issued/expires timestamps are well-formed. **No** Ed25519 signature
//! verification, **no** nonce or sequence replay protection, **no**
//! key/issuer trust evaluation — those land at P1+ alongside the
//! keychain integration. A successful return therefore proves only
//! that the envelope is well-formed; do **not** infer the issuer
//! actually consented to the write until the real verifier ships.
//!
//! Adapters wiring this up at P0 should still treat the resulting
//! token as the trust boundary's authority — once P1 lands, every
//! call site already routes through the function and gets the real
//! crypto for free.

use crate::domain::{
    DomainError, Identity, Rfc3339Timestamp, VerifiedSignedIntent,
    intent::{SignedIntentVerifier, sealed::VerifierWitness},
};
use crate::generated::envelope::SignedIntent;

/// Concrete verifier impl. Empty by design — the trait method is
/// default-implemented in [`SignedIntentVerifier`] and the witness is
/// what gates construction.
pub struct CoreSignedIntentVerifier;

impl SignedIntentVerifier for CoreSignedIntentVerifier {}

/// Verify a `SignedIntent` and mint a [`VerifiedSignedIntent`] token.
///
/// At P0 this performs syntactic-only checks (see module docs). The
/// API and call sites stay stable so wiring the real crypto in P1+ is
/// an internal change to this function alone.
pub fn verify_signed_intent(intent: SignedIntent) -> Result<VerifiedSignedIntent, DomainError> {
    Identity::parse(intent.issuer.0.clone()).map_err(|e| DomainError::InvalidIdentity {
        message: format!("intent.issuer is not a valid identity: {e}"),
    })?;
    if !intent.target_hash.starts_with("sha256:")
        || intent.target_hash.len() != "sha256:".len() + 64
    {
        return Err(DomainError::MissingSignature {
            message: format!(
                "intent.target_hash `{}` is not in `sha256:<64 hex>` form",
                intent.target_hash
            ),
        });
    }
    if !intent.target_hash["sha256:".len()..]
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(DomainError::MissingSignature {
            message: format!(
                "intent.target_hash `{}` contains non-lowercase-hex characters",
                intent.target_hash
            ),
        });
    }
    Rfc3339Timestamp::parse(intent.issued_at.clone()).map_err(|e| {
        DomainError::InvalidTimestamp {
            message: format!("intent.issued_at: {e}"),
        }
    })?;
    Rfc3339Timestamp::parse(intent.expires_at.clone()).map_err(|e| {
        DomainError::InvalidTimestamp {
            message: format!("intent.expires_at: {e}"),
        }
    })?;
    Ok(
        <CoreSignedIntentVerifier as SignedIntentVerifier>::__from_verified(
            intent,
            VerifierWitness::new(),
        ),
    )
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
            issuer: common::Identity("usr:tafeng".to_owned()),
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
    fn accepts_well_formed_intent() {
        verify_signed_intent(good_intent()).expect("syntactic checks pass");
    }

    #[test]
    fn rejects_bad_issuer_identity() {
        let mut i = good_intent();
        i.issuer = common::Identity("not-a-prefix:foo".to_owned());
        let err = verify_signed_intent(i).unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
    }

    #[test]
    fn rejects_bad_target_hash_prefix() {
        let mut i = good_intent();
        i.target_hash = format!("md5:{}", "a".repeat(64));
        let err = verify_signed_intent(i).unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn rejects_bad_target_hash_length() {
        let mut i = good_intent();
        i.target_hash = format!("sha256:{}", "a".repeat(63));
        let err = verify_signed_intent(i).unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn rejects_bad_issued_at() {
        let mut i = good_intent();
        i.issued_at = "not-a-timestamp".to_owned();
        let err = verify_signed_intent(i).unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }
}
