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
    let link = env.link.as_ref().expect("propose envelope has link");
    assert_eq!(key.issuer_key_id, env.issuer_key_id.0.as_str());
    assert_eq!(key.link_id, link.link_id.as_str());
    assert_eq!(key.nonce, link.payload.nonce.0.as_str());
}

#[test]
fn peer_endpoint_is_a_simple_newtype() {
    let p1 = PeerEndpoint("loopback://node-a".into());
    let p2 = PeerEndpoint("loopback://node-a".into());
    assert_eq!(p1, p2);
}
