//! Property tests for the JCS canonicalizer (issue #51).
//!
//! These run against `cairn-core`'s public surface so they live in
//! `tests/` (integration-style) rather than the inline `#[cfg(test)]`
//! module — `proptest!` fits awkwardly inside a unit-test module.

use proptest::prelude::*;

use cairn_core::generated::common;
use cairn_core::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};
use cairn_core::intent::canonical_envelope::canonicalize_signed_payload;

fn arb_intent() -> impl Strategy<Value = SignedIntent> {
    (
        // Stay within JS safe-integer range for sequence values — the wire
        // form is JSON, and JCS preserves integers as IEEE-754 doubles.
        // Range strategy avoids `prop_filter` rejection storms.
        0_u64..=9_007_199_254_740_991_u64,
        1_i64..1_000_000,
        prop_oneof![
            Just(SignedIntentScopeTier::Private),
            Just(SignedIntentScopeTier::Session),
            Just(SignedIntentScopeTier::Project),
            Just(SignedIntentScopeTier::Team),
            Just(SignedIntentScopeTier::Org),
            Just(SignedIntentScopeTier::Public),
        ],
        "[a-z]{1,8}",
        "[a-z]{1,8}",
        "[a-z]{1,8}",
    )
        .prop_map(|(seq, kv, tier, tenant, ws, ent)| SignedIntent {
            chain_parents: vec![],
            expires_at: "2026-04-22T14:07:11Z".to_owned(),
            issued_at: "2026-04-22T14:02:11Z".to_owned(),
            issuer: common::Identity("hmn:tafeng".to_owned()),
            key_version: kv,
            nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".to_owned()),
            operation_id: common::Ulid("01HQZX9F5N0000000000000000".to_owned()),
            scope: SignedIntentScope { tenant, workspace: ws, entity: ent, tier },
            sequence: Some(seq),
            server_challenge: None,
            signature: common::Ed25519Signature(format!("ed25519:{}", "a".repeat(128))),
            target_hash: format!("sha256:{}", "a".repeat(64)),
        })
}

proptest! {
    /// Property 1: canonicalization is deterministic across calls.
    #[test]
    fn determinism(intent in arb_intent()) {
        let a = canonicalize_signed_payload(&intent).expect("a");
        let b = canonicalize_signed_payload(&intent).expect("b");
        prop_assert_eq!(a, b);
    }

    /// Property 2: canonical bytes never contain the literal `"signature"` key.
    #[test]
    fn signature_excluded(intent in arb_intent()) {
        let bytes = canonicalize_signed_payload(&intent).expect("ok");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        prop_assert!(!s.contains("\"signature\""));
    }

    /// Property 3: mutating `target_hash` always changes canonical bytes.
    #[test]
    fn target_hash_tamper_changes_bytes(mut intent in arb_intent()) {
        let baseline = canonicalize_signed_payload(&intent).expect("base");
        intent.target_hash = format!("sha256:{}", "b".repeat(64));
        let after = canonicalize_signed_payload(&intent).expect("mut");
        prop_assert_ne!(baseline, after);
    }

    /// Property 4: mutating `key_version` always changes canonical bytes.
    /// `arb_intent` constrains `key_version` to `1..1_000_000`, so adding 1
    /// stays well clear of `i64::MAX` and the new value is always distinct.
    #[test]
    fn key_version_tamper_changes_bytes(mut intent in arb_intent()) {
        let baseline = canonicalize_signed_payload(&intent).expect("base");
        intent.key_version = intent.key_version.checked_add(1).expect("kv < 1_000_000 in arb");
        let after = canonicalize_signed_payload(&intent).expect("mut");
        prop_assert_ne!(baseline, after);
    }
}
