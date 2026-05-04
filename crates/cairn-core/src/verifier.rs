//! `SignedIntent` verifier — the single production path that mints
//! [`crate::domain::VerifiedSignedIntent`] tokens.
//!
//! Pipeline (brief §4.2 hot-path, executed in this order to short-circuit
//! cheap rejections first):
//! 1. Syntactic post-parse defense in depth — catches direct field-init
//!    bypassing `RawSignedIntent::TryFrom`.
//! 2. Timestamp window vs server-supplied `now`.
//! 3. Issuer-kind ↔ scope-tier fit.
//! 4. Resolver lookup → public key + lifecycle.
//! 5. JCS canonicalize + Ed25519 verify.
//!
//! Replay/sequence consumption is *not* in this pipeline — it lives in
//! the `SQLite` WAL transaction (#52, #55).

use std::time::SystemTime;

use crate::contract::issuer_key_resolver::IssuerKeyResolver;
use crate::domain::{
    Identity, VerifiedSignedIntent,
    intent::{SignedIntentVerifier, sealed::VerifierWitness},
};
use crate::generated::envelope::SignedIntent;
use crate::intent::VerifyError;

/// Concrete verifier impl. Empty by design — the trait method is
/// default-implemented in [`SignedIntentVerifier`] and the witness is
/// what gates construction.
pub struct CoreSignedIntentVerifier;

impl SignedIntentVerifier for CoreSignedIntentVerifier {}

/// Verify a `SignedIntent` and mint a [`VerifiedSignedIntent`] token.
///
/// Five ordered checks (see module docs). Any failure short-circuits with
/// `Err(VerifyError::*)` and no side effects.
///
/// # Errors
/// Returns the appropriate [`VerifyError`] variant.
#[allow(
    clippy::unused_async,
    reason = "steps 2-5 add resolver/timestamp awaits; async signature is stable now"
)]
pub async fn verify_signed_intent(
    intent: SignedIntent,
    _resolver: &dyn IssuerKeyResolver,
    _now: SystemTime,
) -> Result<VerifiedSignedIntent, VerifyError> {
    step1_syntactic(&intent)?;
    // Steps 2-5 land in subsequent tasks.
    Ok(<CoreSignedIntentVerifier as SignedIntentVerifier>::__from_verified(
        intent,
        VerifierWitness::new(),
    ))
}

/// Step 1: post-parse defense in depth. Most invariants are already
/// enforced by `RawSignedIntent::TryFrom`; this re-checks the few that
/// could be bypassed by direct field-init within `cairn-core`.
fn step1_syntactic(intent: &SignedIntent) -> Result<(), VerifyError> {
    Identity::parse(intent.issuer.0.clone()).map_err(|e| VerifyError::Malformed {
        field: "issuer",
        reason: format!("{e}"),
    })?;
    if (u8::from(intent.sequence.is_some()) + u8::from(intent.server_challenge.is_some())) != 1 {
        return Err(VerifyError::Malformed {
            field: "sequence_or_challenge",
            reason: "exactly one of [sequence, server_challenge] is required".to_owned(),
        });
    }
    if intent.key_version < 1 {
        return Err(VerifyError::Malformed {
            field: "key_version",
            reason: format!("must be >= 1; got {}", intent.key_version),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::issuer_key_resolver::{KeyLifecycle, ResolvedKey, ResolverError};
    use crate::domain::identity::keys::KeyVersion;
    use crate::generated::common;
    use crate::generated::envelope::{SignedIntentScope, SignedIntentScopeTier};
    use async_trait::async_trait;

    /// Always-Ok resolver returning a fixed active pubkey. Steps 2-5
    /// are stubs in this task, so the resolver is unused — kept to
    /// match the function signature.
    struct OkResolver;

    #[async_trait]
    impl IssuerKeyResolver for OkResolver {
        async fn lookup(
            &self,
            _issuer: &Identity,
            _key_version: KeyVersion,
        ) -> Result<Option<ResolvedKey>, ResolverError> {
            Ok(Some(ResolvedKey {
                public_key: [0u8; 32],
                lifecycle: KeyLifecycle::Active,
            }))
        }
    }

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

    #[tokio::test]
    async fn rejects_bad_issuer_identity() {
        let mut i = good_intent();
        i.issuer = common::Identity("not-a-prefix:foo".to_owned());
        let err = verify_signed_intent(i, &OkResolver, SystemTime::now())
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "issuer", .. }));
    }

    #[tokio::test]
    async fn rejects_both_sequence_and_challenge() {
        let mut i = good_intent();
        i.server_challenge = Some(common::Nonce16Base64("BBBBBBBBBBBBBBBBBBBBBA==".to_owned()));
        let err = verify_signed_intent(i, &OkResolver, SystemTime::now())
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "sequence_or_challenge", .. }));
    }

    #[tokio::test]
    async fn rejects_neither_sequence_nor_challenge() {
        let mut i = good_intent();
        i.sequence = None;
        let err = verify_signed_intent(i, &OkResolver, SystemTime::now())
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "sequence_or_challenge", .. }));
    }

    #[tokio::test]
    async fn rejects_zero_key_version() {
        let mut i = good_intent();
        i.key_version = 0;
        let err = verify_signed_intent(i, &OkResolver, SystemTime::now())
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "key_version", .. }));
    }
}
