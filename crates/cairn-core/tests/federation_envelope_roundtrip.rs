//! Round-trip serialization for the generated `FederationEnvelope`, plus
//! confirmation that the hand-written domain helpers (`dedup_key`,
//! `PeerEndpoint`) compose correctly with it.

use cairn_core::domain::federation::{
    DedupKey, FederationEnvelope, FederationEnvelopeExt, FederationEnvelopeKind, PeerEndpoint,
};

#[test]
fn propose_envelope_roundtrips_through_canonical_json() {
    let fixture = include_str!("fixtures/federation/propose_envelope.json");
    let env: FederationEnvelope = serde_json::from_str(fixture).expect("parse");
    assert_eq!(env.kind, FederationEnvelopeKind::Propose);
    let re = serde_json::to_string(&env).expect("serialize");
    let env2: FederationEnvelope = serde_json::from_str(&re).expect("reparse");
    assert_eq!(env, env2);
}

#[test]
fn propose_envelope_dedup_key_uses_link_id_and_nonce() {
    let fixture = include_str!("fixtures/federation/propose_envelope.json");
    let env: FederationEnvelope = serde_json::from_str(fixture).expect("parse");
    let key: DedupKey<'_> = env.dedup_key().expect("propose envelope has dedup key");
    assert_eq!(key.issuer_key_id, "hmn:alice");
    assert_eq!(key.link_id, "01HQZX9F5N0000000000000000");
    assert_eq!(key.nonce, "AAAAAAAAAAAAAAAAAAAAAA==");
}

#[test]
fn peer_endpoint_is_a_simple_newtype() {
    let p1 = PeerEndpoint("loopback://node-a".into());
    let p2 = PeerEndpoint("loopback://node-a".into());
    assert_eq!(p1, p2);
}

#[test]
fn revoke_envelope_returns_none_for_dedup_key() {
    let fixture = include_str!("fixtures/federation/revoke_envelope.json");
    let env: FederationEnvelope = serde_json::from_str(fixture).expect("parse");
    assert_eq!(env.kind, FederationEnvelopeKind::Revoke);
    assert!(env.dedup_key().is_none());
    let re = serde_json::to_string(&env).expect("serialize");
    let env2: FederationEnvelope = serde_json::from_str(&re).expect("reparse");
    assert_eq!(env, env2);
}
