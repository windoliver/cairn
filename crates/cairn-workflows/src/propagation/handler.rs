//! `PropagationHandler` — drains the federation outbound queue (brief §12.a).
//!
//! Two flavours of the same [`JobHandler`]:
//!
//! * [`PropagationHandler::outbound_share`] handles
//!   [`OUTBOUND_SHARE_KIND`] jobs enqueued by `propose_share` (T7).
//! * [`PropagationHandler::outbound_revoke`] handles
//!   [`OUTBOUND_REVOKE_KIND`] jobs enqueued by `revoke_share` (T9).
//!
//! Both decode the canonical-JSON payload, build a [`FederationEnvelope`]
//! from it, and forward via a [`FederationTransport`]. The
//! [`SendOutcome`] returned by the transport maps onto a
//! [`HandlerOutcome`] the scheduler can act on:
//!
//! | `SendOutcome`             | `HandlerOutcome`                        |
//! |---------------------------|-----------------------------------------|
//! | `Ack`                     | `Done`                                  |
//! | `Transient(reason)`       | `Retry { class: Transient, reason }`    |
//! | `Permanent(reason)`       | `Permanent { class: Validation, reason }`|
//!
//! Malformed payloads (bytes that fail `serde_json::from_slice`) surface
//! as `Permanent { class: Validation, reason }` immediately — retrying a
//! bad envelope is a waste of budget.

use std::sync::Arc;

use async_trait::async_trait;
use cairn_core::contract::federation_transport::{FederationTransport, SendOutcome};
use cairn_core::contract::job_store::{FailureClass, JobKind, JobPayload};
use cairn_core::domain::federation::{
    FederationEnvelope, FederationEnvelopeKind, PeerEndpoint, signed_share_link_to_wire,
};
use cairn_core::generated::common::Identity as WireIdentity;
use tracing::warn;

use crate::propagation::payload::{
    OUTBOUND_REVOKE_KIND, OUTBOUND_SHARE_KIND, parse_outbound_revoke, parse_outbound_share,
};
use crate::scheduler::handler::{HandlerOutcome, JobHandler};

/// Which propagation flavour an instance handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Outbound share proposal (`propose` envelope).
    Share,
    /// Outbound share revocation (`revoke` envelope).
    Revoke,
}

/// `JobHandler` that turns an outbound propagation job into a
/// [`FederationTransport::send`] call.
///
/// The handler is parameterised over a default [`PeerEndpoint`]: T7/T9
/// enqueue the peer code into the consent journal, not into the job
/// payload itself, so the dispatcher only knows the "home peer" for now.
/// The default endpoint will become a per-job override when issue #123
/// lands its multi-peer follow-up (T14+). For the loopback transport
/// used in tests the value is ignored, so any well-formed string works.
pub struct PropagationHandler {
    transport: Arc<dyn FederationTransport>,
    default_peer: PeerEndpoint,
    direction: Direction,
    kind: JobKind,
}

impl PropagationHandler {
    /// Build a handler for [`OUTBOUND_SHARE_KIND`] jobs.
    #[must_use]
    pub fn outbound_share(transport: Arc<dyn FederationTransport>, peer: PeerEndpoint) -> Self {
        Self {
            transport,
            default_peer: peer,
            direction: Direction::Share,
            kind: JobKind::new(OUTBOUND_SHARE_KIND),
        }
    }

    /// Build a handler for [`OUTBOUND_REVOKE_KIND`] jobs.
    #[must_use]
    pub fn outbound_revoke(transport: Arc<dyn FederationTransport>, peer: PeerEndpoint) -> Self {
        Self {
            transport,
            default_peer: peer,
            direction: Direction::Revoke,
            kind: JobKind::new(OUTBOUND_REVOKE_KIND),
        }
    }

    fn build_envelope(&self, payload: &JobPayload) -> Result<FederationEnvelope, HandlerOutcome> {
        match self.direction {
            Direction::Share => {
                let link = parse_outbound_share(payload).map_err(|err| {
                    warn!(
                        target: "cairn_workflows::propagation::handler",
                        ?err,
                        "outbound_share payload failed to decode",
                    );
                    HandlerOutcome::validation_permanent(format!("decode share payload: {err}"))
                })?;
                let wire_link = signed_share_link_to_wire(&link).map_err(|err| {
                    warn!(
                        target: "cairn_workflows::propagation::handler",
                        ?err,
                        "outbound_share payload could not be re-encoded as wire form",
                    );
                    HandlerOutcome::validation_permanent(format!("encode share wire: {err}"))
                })?;
                Ok(FederationEnvelope {
                    kind: FederationEnvelopeKind::Propose,
                    issuer_key_id: WireIdentity(link.payload.issuer.as_str().to_owned()),
                    link: Some(wire_link),
                    revocation: None,
                    // T13 leaves the manifest empty. The IDL schema only
                    // requires `kind` + `issuer_key_id` (plus `link` for
                    // `kind=propose`); `manifest` is optional. T14's e2e
                    // tests will inject records via a richer build
                    // pipeline.
                    manifest: None,
                })
            }
            Direction::Revoke => {
                let revocation = parse_outbound_revoke(payload).map_err(|err| {
                    warn!(
                        target: "cairn_workflows::propagation::handler",
                        ?err,
                        "outbound_revoke payload failed to decode",
                    );
                    HandlerOutcome::validation_permanent(format!("decode revoke payload: {err}"))
                })?;
                let issuer = revocation.issuer.0.clone();
                Ok(FederationEnvelope {
                    kind: FederationEnvelopeKind::Revoke,
                    issuer_key_id: WireIdentity(issuer),
                    link: None,
                    revocation: Some(revocation),
                    manifest: None,
                })
            }
        }
    }
}

#[async_trait]
impl JobHandler for PropagationHandler {
    fn kind(&self) -> JobKind {
        self.kind.clone()
    }

    async fn handle(&self, payload: &JobPayload) -> HandlerOutcome {
        let envelope = match self.build_envelope(payload) {
            Ok(env) => env,
            Err(outcome) => return outcome,
        };

        match self.transport.send(&envelope, &self.default_peer).await {
            SendOutcome::Ack => HandlerOutcome::Done,
            SendOutcome::Transient(reason) => HandlerOutcome::Retry {
                reason: reason.0,
                class: FailureClass::Transient,
            },
            SendOutcome::Permanent(reason) => HandlerOutcome::Permanent {
                reason: reason.0,
                class: FailureClass::Validation,
            },
            // `SendOutcome` is `#[non_exhaustive]`. A future variant should
            // be surfaced as a validation-permanent failure so the
            // scheduler stops retrying immediately — silently degrading
            // would mask a transport-layer regression.
            other => HandlerOutcome::validation_permanent(format!(
                "unrecognised SendOutcome variant: {other:?}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbound_share_kind_matches_constant() {
        let transport: Arc<dyn FederationTransport> =
            Arc::new(cairn_test_fixtures::federation::LoopbackTransport::new());
        let h = PropagationHandler::outbound_share(transport, PeerEndpoint("x".into()));
        assert_eq!(h.kind().as_str(), OUTBOUND_SHARE_KIND);
    }

    #[test]
    fn outbound_revoke_kind_matches_constant() {
        let transport: Arc<dyn FederationTransport> =
            Arc::new(cairn_test_fixtures::federation::LoopbackTransport::new());
        let h = PropagationHandler::outbound_revoke(transport, PeerEndpoint("x".into()));
        assert_eq!(h.kind().as_str(), OUTBOUND_REVOKE_KIND);
    }
}
