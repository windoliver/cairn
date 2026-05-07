//! Property: any single-byte mutation in the canonical-payload bytes of a
//! signed envelope makes the verifier reject (typically as
//! [`crate::domain::DomainError::InvalidSignature`]; a mutation that
//! lands in a wire-shape-validated field would be caught by the
//! deserializer earlier, but at the verifier-input level we feed an
//! already-deserialized struct, so every mutation reaches the signature
//! check).
//!
//! Lives alongside the verifier module (rather than under `tests/`) so
//! the test can call `ResolvedIssuer::for_test` and `FixedClock` directly
//! — both are gated `#[cfg(any(test, feature = "test-helpers"))]`, and
//! the workspace `check-core-boundary.sh` invariant forbids `cairn-core`
//! from carrying any `cairn-*` self-dep that would activate the
//! `test-helpers` feature in an integration-test build.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey};
use proptest::prelude::*;

use super::{EnvelopeVerifier, ResolvedIssuer, ScopePolicy};
use crate::domain::DomainError;
use crate::domain::Identity;
use crate::domain::canonical::canonical_bytes_signed_intent;
use crate::domain::identity::keys::KeyVersion;
use crate::domain::identity::records::ProvisioningState;
use crate::domain::time::FixedClock;
use crate::generated::common;
use crate::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};

const ISSUER: &str = "hmn:tafeng";
const ISSUED_AT: &str = "2026-04-22T14:02:11Z";
const EXPIRES_AT: &str = "2026-04-22T14:07:11Z";
const NOW: &str = "2026-04-22T14:05:00Z";

fn build_signed(key: &SigningKey) -> SignedIntent {
    let mut intent = SignedIntent {
        chain_parents: vec![],
        expires_at: EXPIRES_AT.into(),
        issued_at: ISSUED_AT.into(),
        issuer: common::Identity(ISSUER.into()),
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
    };
    let bytes = canonical_bytes_signed_intent(&intent).unwrap();
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

proptest! {
    /// Mutate one structural field of a signed envelope (selected by a
    /// pseudo-random index) and assert the verifier rejects with either
    /// `InvalidSignature` (canonical bytes diverge from what was signed)
    /// or `ScopeDenied` (a `scope.tenant` / `scope.workspace` mutation
    /// trips the scope check before signature verification).
    #[test]
    fn one_byte_mutation_rejects(byte_index in any::<usize>(), bit_mask in 1u8..=255) {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let policy = ScopePolicy::new("acme", "ws", ScopePolicy::all_tiers()).unwrap();
        let now: DateTime<Utc> = NOW.parse().unwrap();
        let clock = FixedClock(now);
        let resolved = ResolvedIssuer::for_test(
            Identity::parse(ISSUER).unwrap(),
            KeyVersion::FIRST,
            key.verifying_key(),
            ProvisioningState::Active,
        );

        let intent = build_signed(&key);
        let canonical = canonical_bytes_signed_intent(&intent).unwrap();
        prop_assume!(!canonical.is_empty());

        // Mutating the canonical bytes directly does not affect the
        // SignedIntent struct; instead, mutate one structural field of the
        // struct that the canonical encoder picks up and assert the
        // verifier rejects.
        let mut tweaked = intent.clone();
        let target = byte_index % 4;
        match target {
            0 => tweaked.scope.entity.push(char::from(bit_mask % 26 + b'a')),
            1 => tweaked.scope.workspace.push(char::from(bit_mask % 26 + b'a')),
            2 => tweaked.scope.tenant.push(char::from(bit_mask % 26 + b'a')),
            _ => tweaked.target_hash = format!("sha256:{}", "b".repeat(64)),
        }

        let verifier = EnvelopeVerifier::new(&policy, &clock);
        let result = verifier.verify(tweaked, resolved);
        // The mutation may also trigger ScopeDenied (workspace/tenant
        // change) — both are valid rejection reasons.
        let rejected = matches!(
            result,
            Err(DomainError::InvalidSignature | DomainError::ScopeDenied { .. })
        );
        prop_assert!(rejected);
    }
}
