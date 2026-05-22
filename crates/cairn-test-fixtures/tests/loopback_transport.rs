//! Integration tests for [`LoopbackTransport`].
//!
//! Covers Ack / Transient / Permanent outcomes, default-Ack when the
//! queue is empty, and envelope recording for later inspection.

#![allow(missing_docs)]

use cairn_core::contract::federation_transport::{FederationTransport, SendOutcome};
use cairn_core::domain::federation::{FederationEnvelope, FederationEnvelopeKind, PeerEndpoint};
use cairn_test_fixtures::federation::{LoopbackTransport, ProgrammedOutcome};

fn envelope() -> FederationEnvelope {
    let raw = include_str!("../../cairn-core/tests/fixtures/federation/propose_envelope.json");
    serde_json::from_str(raw).expect("parse fixture")
}

#[tokio::test]
async fn returns_programmed_outcomes_in_order() {
    let t = LoopbackTransport::new();
    t.program([
        ProgrammedOutcome::Transient("boom".into()),
        ProgrammedOutcome::Transient("boom".into()),
        ProgrammedOutcome::Ack,
    ]);
    let env = envelope();
    let peer = PeerEndpoint("loopback".into());

    assert!(matches!(t.send(&env, &peer).await, SendOutcome::Transient(_)));
    assert!(matches!(t.send(&env, &peer).await, SendOutcome::Transient(_)));
    assert_eq!(t.send(&env, &peer).await, SendOutcome::Ack);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn permanent_outcome_carries_reason() {
    let t = LoopbackTransport::new();
    t.program([ProgrammedOutcome::Permanent("rejected".into())]);
    let env = envelope();
    let peer = PeerEndpoint("p".into());
    match t.send(&env, &peer).await {
        SendOutcome::Permanent(reason) => assert_eq!(reason.0, "rejected"),
        other => panic!("expected Permanent, got {other:?}"),
    }
}

#[tokio::test]
async fn defaults_to_ack_when_queue_empty() {
    let t = LoopbackTransport::new();
    let env = envelope();
    let peer = PeerEndpoint("p".into());
    assert_eq!(t.send(&env, &peer).await, SendOutcome::Ack);
}

#[tokio::test]
async fn records_envelopes_for_inspection() {
    let t = LoopbackTransport::new();
    t.program([ProgrammedOutcome::Ack]);
    let env = envelope();
    t.send(&env, &PeerEndpoint("p".into())).await;
    assert_eq!(t.sent().len(), 1);
    assert_eq!(t.sent()[0].0.kind, FederationEnvelopeKind::Propose);
}
