//! Integration tests for `PropagationHandler` (T13, brief §12.a).
//!
//! Asserts the outcome mapping between [`SendOutcome`] returned by the
//! transport and [`HandlerOutcome`] surfaced to the scheduler:
//!
//! | `SendOutcome`              | `HandlerOutcome`                          |
//! |----------------------------|-------------------------------------------|
//! | `Ack`                      | `Done`                                    |
//! | `Transient(reason)`        | `Retry { class: Transient, reason }`      |
//! | `Permanent(reason)`        | `Permanent { class: Validation, reason }` |
//!
//! Plus the malformed-payload path (decode failure → validation
//! permanent).

use std::sync::Arc;

use cairn_core::contract::federation_transport::FederationTransport;
use cairn_core::contract::job_store::FailureClass;
use cairn_core::domain::federation::{
    FederationEnvelope, FederationEnvelopeKind, PeerEndpoint, signed_share_link_from_wire,
};
use cairn_test_fixtures::federation::{LoopbackTransport, ProgrammedOutcome};
use cairn_workflows::propagation::handler::PropagationHandler;
use cairn_workflows::propagation::payload::{
    ManifestEntry, OUTBOUND_REVOKE_KIND, OUTBOUND_SHARE_KIND, OutboundSharePayload,
};
use cairn_workflows::scheduler::handler::{HandlerOutcome, JobHandler};

const PROPOSE_FIXTURE: &str =
    include_str!("../../cairn-core/tests/fixtures/federation/propose_envelope.json");
const REVOKE_FIXTURE: &str =
    include_str!("../../cairn-core/tests/fixtures/federation/revoke_envelope.json");

fn share_payload_bytes() -> Vec<u8> {
    let envelope: FederationEnvelope =
        serde_json::from_str(PROPOSE_FIXTURE).expect("parse propose fixture");
    let wire_link = envelope.link.expect("propose has link");
    let domain_link = signed_share_link_from_wire(&wire_link).expect("from_wire");
    let manifest = vec![ManifestEntry {
        record_id: "01HQZX9F5N0000000000000099".to_owned(),
        kind: "user".to_owned(),
        body: "fixture body".to_owned(),
        body_hash: format!("sha256:{}", "a".repeat(64)),
        visibility: "team".to_owned(),
        scope_wire: "project=test-project".to_owned(),
        tags: Vec::new(),
    }];
    let payload = OutboundSharePayload {
        link: domain_link,
        manifest,
        peer: None,
    };
    serde_json::to_vec(&payload).expect("serialize OutboundSharePayload")
}

fn revoke_payload_bytes() -> Vec<u8> {
    let envelope: FederationEnvelope =
        serde_json::from_str(REVOKE_FIXTURE).expect("parse revoke fixture");
    let revocation = envelope.revocation.expect("revoke has revocation");
    serde_json::to_vec(&revocation).expect("serialize revocation")
}

fn peer() -> PeerEndpoint {
    PeerEndpoint("loopback-default".into())
}

#[tokio::test]
async fn outbound_share_ack_yields_done() {
    let transport = Arc::new(LoopbackTransport::new());
    transport.program([ProgrammedOutcome::Ack]);
    let handler = PropagationHandler::outbound_share(
        Arc::clone(&transport) as Arc<dyn FederationTransport>,
        peer(),
    );
    assert_eq!(handler.kind().as_str(), OUTBOUND_SHARE_KIND);

    let outcome = handler.handle(&share_payload_bytes()).await;
    assert_eq!(outcome, HandlerOutcome::Done);

    let sent = transport.sent();
    assert_eq!(sent.len(), 1, "transport should observe exactly one send");
    let (env, addr) = &sent[0];
    assert_eq!(env.kind, FederationEnvelopeKind::Propose);
    assert!(env.link.is_some(), "propose envelope must carry a link");
    assert!(env.revocation.is_none());
    assert!(
        env.manifest.is_some(),
        "propose envelope must carry a manifest"
    );
    assert_eq!(env.issuer_key_id.0, "hmn:alice");
    assert_eq!(addr, &peer());
}

#[tokio::test]
async fn outbound_share_transient_yields_retry() {
    let transport = Arc::new(LoopbackTransport::new());
    transport.program([ProgrammedOutcome::Transient("net blip".into())]);
    let handler =
        PropagationHandler::outbound_share(transport as Arc<dyn FederationTransport>, peer());

    let outcome = handler.handle(&share_payload_bytes()).await;
    match outcome {
        HandlerOutcome::Retry { reason, class } => {
            assert_eq!(class, FailureClass::Transient);
            assert_eq!(reason, "net blip");
        }
        other => panic!("expected Retry, got {other:?}"),
    }
}

#[tokio::test]
async fn outbound_share_permanent_yields_permanent_validation() {
    let transport = Arc::new(LoopbackTransport::new());
    transport.program([ProgrammedOutcome::Permanent("peer rejected".into())]);
    let handler =
        PropagationHandler::outbound_share(transport as Arc<dyn FederationTransport>, peer());

    let outcome = handler.handle(&share_payload_bytes()).await;
    match outcome {
        HandlerOutcome::Permanent { reason, class } => {
            assert_eq!(class, FailureClass::Validation);
            assert_eq!(reason, "peer rejected");
        }
        other => panic!("expected Permanent, got {other:?}"),
    }
}

#[tokio::test]
async fn outbound_revoke_ack_yields_done() {
    let transport = Arc::new(LoopbackTransport::new());
    transport.program([ProgrammedOutcome::Ack]);
    let handler = PropagationHandler::outbound_revoke(
        Arc::clone(&transport) as Arc<dyn FederationTransport>,
        peer(),
    );
    assert_eq!(handler.kind().as_str(), OUTBOUND_REVOKE_KIND);

    let outcome = handler.handle(&revoke_payload_bytes()).await;
    assert_eq!(outcome, HandlerOutcome::Done);

    let sent = transport.sent();
    assert_eq!(sent.len(), 1, "transport should observe exactly one send");
    let (env, _addr) = &sent[0];
    assert_eq!(env.kind, FederationEnvelopeKind::Revoke);
    assert!(env.link.is_none());
    assert!(
        env.revocation.is_some(),
        "revoke envelope must carry revocation"
    );
    assert_eq!(env.issuer_key_id.0, "hmn:alice");
}

#[tokio::test]
async fn corrupt_payload_yields_validation_permanent() {
    let transport = Arc::new(LoopbackTransport::new());
    let handler =
        PropagationHandler::outbound_share(transport as Arc<dyn FederationTransport>, peer());

    let outcome = handler.handle(&b"not valid json"[..].to_vec()).await;
    match outcome {
        HandlerOutcome::Permanent { class, .. } => {
            assert_eq!(class, FailureClass::Validation);
        }
        other => panic!("expected Permanent(Validation), got {other:?}"),
    }
}
