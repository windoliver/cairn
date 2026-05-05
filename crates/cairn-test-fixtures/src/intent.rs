//! Envelope-verifier test fixtures.
//!
//! Helpers for building signed `SignedIntent` values, fixed clocks, and
//! a default `ScopePolicy` aligned with the verifier's unit-test fixtures.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey};

use cairn_core::domain::canonical::canonical_bytes_signed_intent;
use cairn_core::domain::time::FixedClock;
use cairn_core::generated::common;
use cairn_core::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};
use cairn_core::verifier::ScopePolicy;

/// Default issuer wire string used by all fixtures.
pub const FIXTURE_ISSUER: &str = "hmn:tafeng";
/// Default `issued_at` used by all fixtures.
pub const FIXTURE_ISSUED_AT: &str = "2026-04-22T14:02:11Z";
/// Default `expires_at` used by all fixtures.
pub const FIXTURE_EXPIRES_AT: &str = "2026-04-22T14:07:11Z";

/// Build an unsigned [`SignedIntent`] with a placeholder signature
/// (128 lowercase hex zeros) — pass to [`sign_intent`] before verifying.
#[must_use]
pub fn unsigned_intent() -> SignedIntent {
    SignedIntent {
        chain_parents: vec![],
        expires_at: FIXTURE_EXPIRES_AT.into(),
        issued_at: FIXTURE_ISSUED_AT.into(),
        issuer: common::Identity(FIXTURE_ISSUER.into()),
        key_version: 1,
        nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".into()),
        operation_id: common::Ulid("01HQZX9F5N0000000000000000".into()),
        scope: SignedIntentScope {
            tenant: "acme".into(),
            workspace: "ws".into(),
            entity: "ent".into(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence: Some(1),
        server_challenge: None,
        signature: common::Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
        target_hash: format!("sha256:{}", "a".repeat(64)),
    }
}

/// Sign `intent` with `key`, replacing `intent.signature` with the
/// resulting `ed25519:<hex>` value.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "dev fixture; canonical encoding cannot fail for a well-formed intent"
)]
pub fn sign_intent(key: &SigningKey, mut intent: SignedIntent) -> SignedIntent {
    let bytes = canonical_bytes_signed_intent(&intent).expect("canonical encoding always succeeds");
    let sig = key.sign(&bytes);
    let hex = sig
        .to_bytes()
        .iter()
        .fold(String::with_capacity(128), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(&mut s, "{b:02x}");
            s
        });
    intent.signature = common::Ed25519Signature(format!("ed25519:{hex}"));
    intent
}

/// Build a [`FixedClock`] from an RFC-3339 string.
///
/// # Panics
/// Panics if `iso` is not a valid RFC-3339 timestamp — fixture code only.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "dev fixture; caller passes a literal RFC-3339 string"
)]
pub fn fixed_clock_at(iso: &str) -> FixedClock {
    let t: DateTime<Utc> = iso.parse().expect("fixture: valid RFC-3339 string");
    FixedClock(t)
}

/// Default [`ScopePolicy`] aligned with [`unsigned_intent`].
///
/// # Panics
/// Panics if `ScopePolicy::new` rejects the inputs (it cannot — strings
/// are non-empty and the tier set is non-empty).
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "dev fixture; inputs are hardcoded non-empty"
)]
pub fn scope_policy_default() -> ScopePolicy {
    let mut tiers = HashSet::new();
    tiers.insert(SignedIntentScopeTier::Project);
    tiers.insert(SignedIntentScopeTier::Session);
    tiers.insert(SignedIntentScopeTier::Private);
    tiers.insert(SignedIntentScopeTier::Team);
    tiers.insert(SignedIntentScopeTier::Org);
    tiers.insert(SignedIntentScopeTier::Public);
    ScopePolicy::new("acme", "ws", tiers).expect("fixture: well-formed scope policy")
}
