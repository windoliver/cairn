//! Smoke test: confirm the `FederationTransport` trait can be implemented
//! and exercised. The actual loopback adapter ships in cairn-test-fixtures
//! at Task 10; here we only verify the contract shape.

use async_trait::async_trait;

use cairn_core::contract::federation_transport::{
    FederationTransport, SendOutcome, TransportReason,
};
use cairn_core::domain::federation::{FederationEnvelope, PeerEndpoint};

struct AckOnly;

#[async_trait]
impl FederationTransport for AckOnly {
    async fn send(&self, _: &FederationEnvelope, _: &PeerEndpoint) -> SendOutcome {
        SendOutcome::Ack
    }
}

struct AlwaysTransient;

#[async_trait]
impl FederationTransport for AlwaysTransient {
    async fn send(&self, _: &FederationEnvelope, _: &PeerEndpoint) -> SendOutcome {
        SendOutcome::Transient(TransportReason("simulated outage".into()))
    }
}

fn fixture_envelope() -> FederationEnvelope {
    let raw = include_str!("fixtures/federation/propose_envelope.json");
    serde_json::from_str(raw).expect("parse fixture")
}

#[tokio::test]
async fn ack_transport_returns_ack() {
    let peer = PeerEndpoint("loopback://node-a".into());
    let env = fixture_envelope();
    assert_eq!(AckOnly.send(&env, &peer).await, SendOutcome::Ack);
}

#[tokio::test]
async fn transient_transport_carries_reason() {
    let peer = PeerEndpoint("loopback://node-a".into());
    let env = fixture_envelope();
    match AlwaysTransient.send(&env, &peer).await {
        SendOutcome::Transient(reason) => assert_eq!(reason.0, "simulated outage"),
        other => panic!("expected Transient, got {other:?}"),
    }
}

#[tokio::test]
async fn dyn_trait_object_dispatches() {
    // Confirm the trait is object-safe — PropagationHandler will hold a
    // `Arc<dyn FederationTransport>` so this must compile.
    let t: std::sync::Arc<dyn FederationTransport> = std::sync::Arc::new(AckOnly);
    let peer = PeerEndpoint("loopback://a".into());
    let env = fixture_envelope();
    assert_eq!(t.send(&env, &peer).await, SendOutcome::Ack);
}
