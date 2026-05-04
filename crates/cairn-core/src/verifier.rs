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

use std::num::NonZeroU32;
use std::time::SystemTime;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::contract::issuer_key_resolver::{IssuerKeyResolver, KeyLifecycle, ResolvedKey};
use crate::domain::{
    Identity, VerifiedSignedIntent,
    intent::{SignedIntentVerifier, sealed::VerifierWitness},
};
use crate::domain::identity::keys::KeyVersion;
use crate::generated::envelope::SignedIntent;
use crate::intent::{ExpiryReason, VerifyError};
use crate::intent::canonical_envelope::canonicalize_signed_payload;

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
#[tracing::instrument(
    skip(intent, resolver),
    err,
    fields(
        verb = "verify_signed_intent",
        issuer = %intent.issuer.0,
        key_version = intent.key_version,
        operation_id = %intent.operation_id.0,
    ),
)]
pub async fn verify_signed_intent(
    intent: SignedIntent,
    resolver: &dyn IssuerKeyResolver,
    now: SystemTime,
) -> Result<VerifiedSignedIntent, VerifyError> {
    step1_syntactic(&intent)?;
    step2_timestamp_window(&intent, now)?;
    step3_scope_fit(&intent)?;
    let resolved_key = step4_resolver_lookup(&intent, resolver).await?;
    step5_signature_verify(&intent, &resolved_key)?;
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

const MAX_SKEW_SECS: i64 = 120; // ±2 min, brief §4.2
const MAX_TTL_SECS: i64 = 5 * 60; // 5 min flat at P0; brief §4.2

/// Step 2: timestamp window. `issued_at` ±2 min of `now`,
/// `expires_at − issued_at ≤ 5 min`, `now ≤ expires_at`.
fn step2_timestamp_window(intent: &SignedIntent, now: SystemTime) -> Result<(), VerifyError> {
    use chrono::{DateTime, Utc};
    let issued = DateTime::parse_from_rfc3339(&intent.issued_at)
        .map_err(|e| VerifyError::Malformed { field: "issued_at", reason: e.to_string() })?
        .with_timezone(&Utc);
    let expires = DateTime::parse_from_rfc3339(&intent.expires_at)
        .map_err(|e| VerifyError::Malformed { field: "expires_at", reason: e.to_string() })?
        .with_timezone(&Utc);
    let now_dt: DateTime<Utc> = now.into();
    let now_str = now_dt.to_rfc3339();

    let skew = (now_dt - issued).num_seconds().abs();
    if skew > MAX_SKEW_SECS {
        return Err(VerifyError::ExpiredIntent {
            issued_at: intent.issued_at.clone(),
            expires_at: intent.expires_at.clone(),
            now: now_str,
            kind: ExpiryReason::Skewed,
        });
    }
    let ttl = (expires - issued).num_seconds();
    if ttl > MAX_TTL_SECS {
        return Err(VerifyError::ExpiredIntent {
            issued_at: intent.issued_at.clone(),
            expires_at: intent.expires_at.clone(),
            now: now_str,
            kind: ExpiryReason::TtlExceeded,
        });
    }
    if now_dt >= expires {
        return Err(VerifyError::ExpiredIntent {
            issued_at: intent.issued_at.clone(),
            expires_at: intent.expires_at.clone(),
            now: now_str,
            kind: ExpiryReason::Past,
        });
    }
    Ok(())
}

/// Step 3: identity-kind ↔ tier fit. P0 baseline (brief §4.2):
/// `snr:` → tier == private; `agt:` → tier ∈ {private, session, project};
/// `hmn:` → all tiers. Closes the "agent self-promotes to public" attack
/// without per-agent policy infrastructure.
fn step3_scope_fit(intent: &SignedIntent) -> Result<(), VerifyError> {
    use crate::domain::identity::IdentityKind;
    use crate::generated::envelope::SignedIntentScopeTier as Tier;

    let kind = Identity::parse(intent.issuer.0.clone())
        .map_err(|e| VerifyError::Malformed { field: "issuer", reason: e.to_string() })?
        .kind();

    let allowed = matches!(
        (kind, intent.scope.tier),
        (IdentityKind::Sensor, Tier::Private)
            | (IdentityKind::Agent, Tier::Private | Tier::Session | Tier::Project)
            | (IdentityKind::Human, _),
    );

    if !allowed {
        return Err(VerifyError::ScopeDenied {
            issuer_kind: kind,
            requested_tier: intent.scope.tier,
        });
    }
    Ok(())
}

/// Step 4: resolver lookup. Returns the resolved key on success;
/// maps `None` / `NonOperational` / `Purged` → `UnknownKey`,
/// `Revoked` w/ `effective_at ≤ issued_at` → `RevokedKey`.
async fn step4_resolver_lookup(
    intent: &SignedIntent,
    resolver: &dyn IssuerKeyResolver,
) -> Result<ResolvedKey, VerifyError> {
    let issuer = Identity::parse(intent.issuer.0.clone())
        .map_err(|e| VerifyError::Malformed { field: "issuer", reason: e.to_string() })?;
    let kv_u32 = u32::try_from(intent.key_version).map_err(|_| VerifyError::Malformed {
        field: "key_version",
        reason: format!("out of u32 range: {}", intent.key_version),
    })?;
    let key_version = NonZeroU32::new(kv_u32)
        .map(KeyVersion::new)
        .ok_or(VerifyError::Malformed {
            field: "key_version",
            reason: "must be >= 1".to_owned(),
        })?;
    let resolved_key = resolver
        .lookup(&issuer, key_version)
        .await
        .map_err(VerifyError::ResolverFailure)?
        .ok_or_else(|| VerifyError::UnknownKey {
            issuer: issuer.clone(),
            key_version,
        })?;
    match &resolved_key.lifecycle {
        KeyLifecycle::Active => Ok(resolved_key),
        KeyLifecycle::Revoked { effective_at } => {
            // Typed comparison — survives RFC3339 sub-second / timezone variation.
            let issued_dt =
                chrono::DateTime::parse_from_rfc3339(&intent.issued_at).map_err(|e| {
                    VerifyError::Malformed { field: "issued_at", reason: e.to_string() }
                })?;
            let effective_dt =
                chrono::DateTime::parse_from_rfc3339(effective_at.as_str()).map_err(|e| {
                    VerifyError::Malformed { field: "effective_at", reason: e.to_string() }
                })?;
            if effective_dt <= issued_dt {
                Err(VerifyError::RevokedKey {
                    issuer,
                    key_version,
                    effective_at: effective_at.as_str().to_owned(),
                })
            } else {
                Ok(resolved_key)
            }
        }
        KeyLifecycle::NonOperational | KeyLifecycle::Purged => {
            Err(VerifyError::UnknownKey { issuer, key_version })
        }
    }
}

/// Step 5: JCS-canonicalize envelope-minus-signature, parse the
/// `ed25519:<128-hex>` signature blob, parse the resolved 32-byte
/// pubkey, and run Ed25519 verify. Any failure → `InvalidSignature`
/// (opaque on purpose — no oracle for differential timing).
fn step5_signature_verify(
    intent: &SignedIntent,
    resolved: &ResolvedKey,
) -> Result<(), VerifyError> {
    let canonical = canonicalize_signed_payload(intent)?;
    let sig_hex = intent
        .signature
        .0
        .strip_prefix("ed25519:")
        .ok_or(VerifyError::InvalidSignature)?;
    let sig_bytes = hex::decode(sig_hex).map_err(|_| VerifyError::InvalidSignature)?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| VerifyError::InvalidSignature)?;
    let signature = Signature::from_bytes(&sig_arr);
    let verifying = VerifyingKey::from_bytes(&resolved.public_key)
        .map_err(|_| VerifyError::InvalidSignature)?;
    verifying
        .verify(&canonical, &signature)
        .map_err(|_| VerifyError::InvalidSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::issuer_key_resolver::{KeyLifecycle, ResolvedKey, ResolverError};
    use crate::domain::identity::keys::KeyVersion;
    use crate::generated::common;
    use crate::generated::envelope::{SignedIntentScope, SignedIntentScopeTier};
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};

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

    fn rfc3339_to_systemtime(s: &str) -> SystemTime {
        DateTime::parse_from_rfc3339(s)
            .expect("rfc3339")
            .with_timezone(&Utc)
            .into()
    }

    use ed25519_dalek::{Signer, SigningKey};
    use rand_core::OsRng;
    use crate::intent::canonical_envelope::canonicalize_signed_payload;

    /// Build a (intent, resolver) pair with a real signature so the
    /// happy path can flow through step 5.
    fn signed_pair() -> (SignedIntent, FakeResolver) {
        signed_pair_with(|_| {})
    }

    /// Build a (intent, resolver) pair with overrides applied to the intent
    /// before signing. The resolver entry's pubkey matches the freshly
    /// generated signing key. Use this for happy-path tests that need to
    /// alter intent fields like scope.tier or issuer.
    fn signed_pair_with<F: FnOnce(&mut SignedIntent)>(mutate: F) -> (SignedIntent, FakeResolver) {
        let mut rng = OsRng;
        let signing = SigningKey::generate(&mut rng);
        let mut intent = good_intent();
        mutate(&mut intent);
        let canonical = canonicalize_signed_payload(&intent).expect("canon");
        let sig = signing.sign(&canonical);
        intent.signature = common::Ed25519Signature(format!("ed25519:{}", hex::encode(sig.to_bytes())));

        let r = FakeResolver::new();
        let issuer_str = intent.issuer.0.clone();
        let kv_u32 = u32::try_from(intent.key_version).expect("key_version fits u32");
        r.set(&issuer_str, kv_u32, Some(ResolvedKey {
            public_key: signing.verifying_key().to_bytes(),
            lifecycle: KeyLifecycle::Active,
        }));
        (intent, r)
    }

    #[tokio::test]
    async fn rejects_bad_issuer_identity() {
        let mut i = good_intent();
        i.issuer = common::Identity("not-a-prefix:foo".to_owned());
        // Step 1 rejects before reaching the resolver.
        let err = verify_signed_intent(i, &FakeResolver::new(), rfc3339_to_systemtime("2026-04-22T14:02:30Z"))
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "issuer", .. }));
    }

    #[tokio::test]
    async fn rejects_both_sequence_and_challenge() {
        let mut i = good_intent();
        i.server_challenge = Some(common::Nonce16Base64("BBBBBBBBBBBBBBBBBBBBBA==".to_owned()));
        let err = verify_signed_intent(i, &FakeResolver::new(), rfc3339_to_systemtime("2026-04-22T14:02:30Z"))
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "sequence_or_challenge", .. }));
    }

    #[tokio::test]
    async fn rejects_neither_sequence_nor_challenge() {
        let mut i = good_intent();
        i.sequence = None;
        let err = verify_signed_intent(i, &FakeResolver::new(), rfc3339_to_systemtime("2026-04-22T14:02:30Z"))
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "sequence_or_challenge", .. }));
    }

    #[tokio::test]
    async fn rejects_zero_key_version() {
        let mut i = good_intent();
        i.key_version = 0;
        let err = verify_signed_intent(i, &FakeResolver::new(), rfc3339_to_systemtime("2026-04-22T14:02:30Z"))
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "key_version", .. }));
    }

    #[tokio::test]
    async fn rejects_skewed_issued_at_in_future() {
        let mut i = good_intent();
        // issued_at is 14:02:11; set now = 13:55:11 (7min in past) → Skewed
        i.issued_at = "2026-04-22T14:02:11Z".to_owned();
        i.expires_at = "2026-04-22T14:07:11Z".to_owned();
        let now = rfc3339_to_systemtime("2026-04-22T13:55:11Z");
        let err = verify_signed_intent(i, &FakeResolver::new(), now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ExpiredIntent { kind: ExpiryReason::Skewed, .. }));
    }

    #[tokio::test]
    async fn rejects_past_expires_at() {
        let mut i = good_intent();
        i.issued_at = "2026-04-22T14:02:11Z".to_owned();
        i.expires_at = "2026-04-22T14:03:11Z".to_owned();
        // now is 82s after issued (within ±2min skew) but after expires_at → Past
        let now = rfc3339_to_systemtime("2026-04-22T14:03:33Z");
        let err = verify_signed_intent(i, &FakeResolver::new(), now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ExpiredIntent { kind: ExpiryReason::Past, .. }));
    }

    #[tokio::test]
    async fn rejects_overlong_ttl() {
        let mut i = good_intent();
        i.issued_at = "2026-04-22T14:02:11Z".to_owned();
        // 6-min TTL > 5-min cap
        i.expires_at = "2026-04-22T14:08:11Z".to_owned();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &FakeResolver::new(), now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ExpiredIntent { kind: ExpiryReason::TtlExceeded, .. }));
    }

    #[tokio::test]
    async fn accepts_window_within_bounds() {
        let (intent, r) = signed_pair();
        // good_intent: issued=14:02:11, expires=14:07:11 (5min flat).
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(intent, &r, now).await.expect("within bounds");
    }

    #[tokio::test]
    async fn rejects_sensor_writing_session_tier() {
        let mut i = good_intent();
        i.issuer = common::Identity("snr:local:screen:host:v1".to_owned());
        i.scope.tier = SignedIntentScopeTier::Session;
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        // Step 3 rejects before reaching the resolver.
        let err = verify_signed_intent(i, &FakeResolver::new(), now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ScopeDenied { .. }));
    }

    #[tokio::test]
    async fn accepts_sensor_writing_private_tier() {
        let (intent, r) = signed_pair_with(|i| {
            i.issuer = common::Identity("snr:local:screen:host:v1".to_owned());
            i.scope.tier = SignedIntentScopeTier::Private;
        });
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(intent, &r, now).await.expect("ok");
    }

    #[tokio::test]
    async fn rejects_agent_writing_team_tier() {
        let mut i = good_intent();
        i.issuer = common::Identity("agt:claude-code:opus-4-7:reviewer:v1".to_owned());
        i.scope.tier = SignedIntentScopeTier::Team;
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        // Step 3 rejects before reaching the resolver.
        let err = verify_signed_intent(i, &FakeResolver::new(), now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ScopeDenied { .. }));
    }

    #[tokio::test]
    async fn accepts_agent_writing_project_tier() {
        let (intent, r) = signed_pair_with(|i| {
            i.issuer = common::Identity("agt:claude-code:opus-4-7:reviewer:v1".to_owned());
            i.scope.tier = SignedIntentScopeTier::Project;
        });
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(intent, &r, now).await.expect("ok");
    }

    #[tokio::test]
    async fn accepts_human_writing_public_tier() {
        let (intent, r) = signed_pair_with(|i| {
            i.issuer = common::Identity("hmn:tafeng".to_owned());
            i.scope.tier = SignedIntentScopeTier::Public;
        });
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(intent, &r, now).await.expect("ok");
    }

    use std::collections::HashMap;
    use std::sync::Mutex;
    use crate::domain::timestamp::Rfc3339Timestamp;

    /// Programmable fake resolver — returns whatever the test puts in
    /// the table for a given (issuer string, `key_version`) pair.
    struct FakeResolver {
        pub(super) table: Mutex<HashMap<(String, u32), Option<ResolvedKey>>>,
        fail_with: Mutex<Option<&'static str>>,
    }

    impl FakeResolver {
        fn new() -> Self {
            Self {
                table: Mutex::new(HashMap::new()),
                fail_with: Mutex::new(None),
            }
        }
        fn set(&self, issuer: &str, ver: u32, value: Option<ResolvedKey>) {
            self.table
                .lock()
                .expect("lock")
                .insert((issuer.to_owned(), ver), value);
        }
        fn fail_with(&self, msg: &'static str) {
            *self.fail_with.lock().expect("lock") = Some(msg);
        }
    }

    #[async_trait]
    impl IssuerKeyResolver for FakeResolver {
        async fn lookup(
            &self,
            issuer: &Identity,
            key_version: KeyVersion,
        ) -> Result<Option<ResolvedKey>, ResolverError> {
            if let Some(msg) = *self.fail_with.lock().expect("lock") {
                return Err(ResolverError::Backend(msg.into()));
            }
            let key = (issuer.to_string(), key_version.as_u32());
            Ok(self.table.lock().expect("lock").get(&key).cloned().flatten())
        }
    }

    fn rfc3339(s: &str) -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse(s.to_owned()).expect("rfc3339")
    }

    #[tokio::test]
    async fn rejects_unknown_key() {
        let r = FakeResolver::new();
        let i = good_intent();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::UnknownKey { .. }));
    }

    #[tokio::test]
    async fn rejects_non_operational_lifecycle() {
        let r = FakeResolver::new();
        r.set("hmn:tafeng", 1, Some(ResolvedKey {
            public_key: [0u8; 32],
            lifecycle: KeyLifecycle::NonOperational,
        }));
        let i = good_intent();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::UnknownKey { .. }));
    }

    #[tokio::test]
    async fn rejects_purged_lifecycle() {
        let r = FakeResolver::new();
        r.set("hmn:tafeng", 1, Some(ResolvedKey {
            public_key: [0u8; 32],
            lifecycle: KeyLifecycle::Purged,
        }));
        let i = good_intent();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::UnknownKey { .. }));
    }

    #[tokio::test]
    async fn rejects_revoked_before_issued_at() {
        let r = FakeResolver::new();
        r.set("hmn:tafeng", 1, Some(ResolvedKey {
            public_key: [0u8; 32],
            lifecycle: KeyLifecycle::Revoked {
                effective_at: rfc3339("2026-04-22T14:02:00Z"),
            },
        }));
        let i = good_intent(); // issued_at = 2026-04-22T14:02:11Z
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::RevokedKey { .. }));
    }

    #[tokio::test]
    async fn accepts_revoked_after_issued_at() {
        let (intent, r) = signed_pair();
        // Snapshot the pubkey signed_pair seeded, then overwrite with Revoked-future.
        let pk = r.table.lock().expect("lock")
            .get(&("hmn:tafeng".to_owned(), 1))
            .cloned().flatten().expect("seeded").public_key;
        r.set("hmn:tafeng", 1, Some(ResolvedKey {
            public_key: pk,
            lifecycle: KeyLifecycle::Revoked {
                effective_at: rfc3339("2026-04-22T15:00:00Z"),
            },
        }));
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(intent, &r, now).await.expect("revocation post-dates op");
    }

    #[tokio::test]
    async fn surfaces_resolver_io_error() {
        let r = FakeResolver::new();
        r.fail_with("synthetic backend failure");
        let i = good_intent();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ResolverFailure(_)));
    }

    // ─── Step 5: Ed25519 signature verification ───────────────────────────────

    #[tokio::test]
    async fn accepts_well_formed_signed_intent() {
        let (intent, r) = signed_pair();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(intent, &r, now).await.expect("real signature verifies");
    }

    #[tokio::test]
    async fn rejects_tampered_signature() {
        let (mut intent, r) = signed_pair();
        // Flip one hex char of the signature.
        let s = intent.signature.0.clone();
        let mut chars: Vec<char> = s.chars().collect();
        let idx = chars.len() / 2;
        // Pick a char that's a-f, flip it.
        chars[idx] = if chars[idx] == 'a' { 'b' } else { 'a' };
        intent.signature = common::Ed25519Signature(chars.into_iter().collect());
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(intent, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::InvalidSignature));
    }

    #[tokio::test]
    async fn rejects_swapped_target_hash() {
        let (mut intent, r) = signed_pair();
        intent.target_hash = format!("sha256:{}", "9".repeat(64));
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(intent, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::InvalidSignature));
    }

    #[tokio::test]
    async fn rejects_signature_against_wrong_pubkey() {
        let (intent, _r) = signed_pair();
        // Replace the resolver with a different pubkey (random).
        let other = SigningKey::generate(&mut OsRng);
        let r = FakeResolver::new();
        r.set("hmn:tafeng", 1, Some(ResolvedKey {
            public_key: other.verifying_key().to_bytes(),
            lifecycle: KeyLifecycle::Active,
        }));
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(intent, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::InvalidSignature));
    }
}
