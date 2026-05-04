//! `SignedIntent` fixture builder + companion fake `IssuerKeyResolver`.
//!
//! Use [`SignedIntentFixture::default`] + [`SignedIntentFixture::build`] for
//! "Just give me a valid envelope" cases. Override fields directly via the
//! struct's public fields (combined with `..Default::default()`) when a
//! test needs a specific shape.
//!
//! ```ignore
//! let (intent, resolver, now) = SignedIntentFixture::default().build();
//! verify_signed_intent(intent, &resolver, now).await.expect("verifies");
//!
//! let (intent, _, _) = SignedIntentFixture {
//!     tier: SignedIntentScopeTier::Private,
//!     ..SignedIntentFixture::default()
//! }
//! .build();
//! ```

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand_core::OsRng;

use cairn_core::contract::issuer_key_resolver::{
    IssuerKeyResolver, KeyLifecycle, ResolvedKey, ResolverError,
};
use cairn_core::domain::Identity;
use cairn_core::domain::identity::keys::KeyVersion;
use cairn_core::generated::common;
use cairn_core::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};
use cairn_core::intent::canonical_envelope::canonicalize_signed_payload;

/// Override hooks for a freshly-minted `(SignedIntent, FakeIssuerKeyResolver, now)`
/// trio. All fields default to a P0-valid, hmn-issued, project-scoped intent
/// signed by a freshly generated keypair.
pub struct SignedIntentFixture {
    /// Wire-form issuer string (e.g. `hmn:tafeng`, `agt:claude:opus:v1`).
    pub issuer: String,
    /// RFC3339 `issued_at` timestamp.
    pub issued_at: String,
    /// RFC3339 `expires_at` timestamp.
    pub expires_at: String,
    /// RFC3339 string converted into the `now` `SystemTime` returned by [`Self::build`].
    pub now: String,
    /// Scope tier embedded in the signed envelope.
    pub tier: SignedIntentScopeTier,
    /// `key_version` field on the envelope (must be ≥ 1 for happy-path verify).
    pub key_version: i64,
    /// `sequence` field; mutually exclusive with [`Self::server_challenge`].
    pub sequence: Option<u64>,
    /// `server_challenge` field; mutually exclusive with [`Self::sequence`].
    pub server_challenge: Option<String>,
    /// Lifecycle the resolver returns for the matching pubkey entry.
    pub lifecycle: KeyLifecycle,
}

impl Default for SignedIntentFixture {
    fn default() -> Self {
        Self {
            issuer: "hmn:tafeng".to_owned(),
            issued_at: "2026-04-22T14:02:11Z".to_owned(),
            expires_at: "2026-04-22T14:07:11Z".to_owned(),
            now: "2026-04-22T14:02:30Z".to_owned(),
            tier: SignedIntentScopeTier::Project,
            key_version: 1,
            sequence: Some(1),
            server_challenge: None,
            lifecycle: KeyLifecycle::Active,
        }
    }
}

impl SignedIntentFixture {
    /// Mint the trio. Generates a fresh Ed25519 keypair, populates the
    /// envelope, signs the JCS-canonical bytes, and registers the matching
    /// pubkey with a [`FakeIssuerKeyResolver`].
    ///
    /// # Panics
    /// Panics if the configured timestamps are not RFC3339 or if the
    /// canonicalizer rejects the envelope shape — both indicate a broken
    /// fixture, not a runtime condition.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "fixture-time invariants — bad inputs mean the test data itself is broken"
    )]
    pub fn build(self) -> (SignedIntent, FakeIssuerKeyResolver, SystemTime) {
        let mut rng = OsRng;
        let signing = SigningKey::generate(&mut rng);
        let verifying: VerifyingKey = signing.verifying_key();

        let mut intent = SignedIntent {
            chain_parents: vec![],
            expires_at: self.expires_at,
            issued_at: self.issued_at,
            issuer: common::Identity(self.issuer.clone()),
            key_version: self.key_version,
            nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".to_owned()),
            operation_id: common::Ulid("01HQZX9F5N0000000000000000".to_owned()),
            scope: SignedIntentScope {
                tenant: "acme".to_owned(),
                workspace: "ws".to_owned(),
                entity: "ent".to_owned(),
                tier: self.tier,
            },
            sequence: self.sequence,
            server_challenge: self.server_challenge.map(common::Nonce16Base64),
            signature: common::Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
            target_hash: format!("sha256:{}", "a".repeat(64)),
        };
        let canonical = canonicalize_signed_payload(&intent).expect("canonicalize fixture");
        let sig = signing.sign(&canonical);
        intent.signature =
            common::Ed25519Signature(format!("ed25519:{}", hex::encode(sig.to_bytes())));

        let resolver = FakeIssuerKeyResolver::new();
        let kv_u32 = u32::try_from(self.key_version).expect("fixture key_version fits u32");
        resolver.set(
            &self.issuer,
            kv_u32,
            Some(ResolvedKey {
                public_key: verifying.to_bytes(),
                lifecycle: self.lifecycle,
            }),
        );

        let now: SystemTime = DateTime::parse_from_rfc3339(&self.now)
            .expect("rfc3339 fixture now")
            .with_timezone(&Utc)
            .into();
        (intent, resolver, now)
    }
}

/// Programmable in-memory [`IssuerKeyResolver`]. Test-only.
pub struct FakeIssuerKeyResolver {
    table: Mutex<HashMap<(String, u32), Option<ResolvedKey>>>,
    fail_with: Mutex<Option<&'static str>>,
}

impl FakeIssuerKeyResolver {
    /// Empty resolver — every lookup returns `Ok(None)`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            table: Mutex::new(HashMap::new()),
            fail_with: Mutex::new(None),
        }
    }

    /// Insert (or clear, by passing `None`) the entry returned for
    /// `(issuer, key_version)`.
    ///
    /// # Panics
    /// Panics if the internal `Mutex` is poisoned, which would only happen
    /// if a prior test panicked while holding the lock.
    #[allow(
        clippy::expect_used,
        reason = "test fixture; mutex poisoning surfaces a prior test panic immediately"
    )]
    pub fn set(&self, issuer: &str, key_version: u32, value: Option<ResolvedKey>) {
        self.table
            .lock()
            .expect("FakeIssuerKeyResolver table lock")
            .insert((issuer.to_owned(), key_version), value);
    }

    /// Make every subsequent `lookup` return `Err(ResolverError::Backend(msg))`.
    /// Use to simulate transient backend faults.
    ///
    /// # Panics
    /// Panics if the internal `Mutex` is poisoned.
    #[allow(
        clippy::expect_used,
        reason = "test fixture; mutex poisoning surfaces a prior test panic immediately"
    )]
    pub fn fail_with(&self, msg: &'static str) {
        *self
            .fail_with
            .lock()
            .expect("FakeIssuerKeyResolver fail_with lock") = Some(msg);
    }
}

impl Default for FakeIssuerKeyResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IssuerKeyResolver for FakeIssuerKeyResolver {
    async fn lookup(
        &self,
        issuer: &Identity,
        key_version: KeyVersion,
    ) -> Result<Option<ResolvedKey>, ResolverError> {
        #[allow(
            clippy::expect_used,
            reason = "test fixture; mutex poisoning surfaces a prior test panic immediately"
        )]
        if let Some(msg) = *self
            .fail_with
            .lock()
            .expect("FakeIssuerKeyResolver fail_with lock")
        {
            return Err(ResolverError::Backend(msg.into()));
        }
        let key = (issuer.to_string(), key_version.as_u32());
        #[allow(
            clippy::expect_used,
            reason = "test fixture; mutex poisoning surfaces a prior test panic immediately"
        )]
        Ok(self
            .table
            .lock()
            .expect("FakeIssuerKeyResolver table lock")
            .get(&key)
            .cloned()
            .flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builder_default_passes_verification() {
        use cairn_core::verifier::verify_signed_intent;
        let (intent, resolver, now) = SignedIntentFixture::default().build();
        verify_signed_intent(intent, &resolver, now)
            .await
            .expect("default fixture verifies");
    }

    #[tokio::test]
    async fn builder_overrides_tier() {
        let (intent, _, _) = SignedIntentFixture {
            tier: SignedIntentScopeTier::Private,
            ..SignedIntentFixture::default()
        }
        .build();
        assert_eq!(intent.scope.tier, SignedIntentScopeTier::Private);
    }

    #[tokio::test]
    async fn fake_resolver_returns_none_for_unset_key() {
        let r = FakeIssuerKeyResolver::new();
        let issuer = Identity::parse("hmn:nobody").expect("parse");
        let kv = KeyVersion::new(std::num::NonZeroU32::new(1).expect("nz"));
        let out = r.lookup(&issuer, kv).await.expect("ok");
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn fake_resolver_fail_with_propagates_backend_error() {
        let r = FakeIssuerKeyResolver::new();
        r.fail_with("simulated outage");
        let issuer = Identity::parse("hmn:nobody").expect("parse");
        let kv = KeyVersion::new(std::num::NonZeroU32::new(1).expect("nz"));
        let err = r.lookup(&issuer, kv).await.expect_err("must fail");
        assert!(matches!(err, ResolverError::Backend(_)));
    }
}
