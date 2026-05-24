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
use cairn_core::generated::common::{
    Identity as WireIdentity, MemoryRecordStub as WireStub,
    MemoryRecordStubVisibility as WireStubVis, ScopeTuple as WireScope, Ulid as WireUlid,
};
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
/// The handler carries a `default_peer` that is used as the fallback when
/// no per-job peer is present.  `propose_share` (T7) serializes
/// `req.peer` into the `OutboundSharePayload.peer` field; the handler
/// reads it back and sends to that peer, falling back to `default_peer`
/// when the field is absent (e.g. payloads written before the peer-
/// routing fix, or when the caller omitted `--peer`).
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

    /// Try to extract a per-job `peer` override from the payload JSON.
    /// Returns `None` for revoke payloads and for share payloads written
    /// before the peer-routing fix (missing field → `serde(default)`).
    fn extract_peer_override(&self, payload: &JobPayload) -> Option<PeerEndpoint> {
        if self.direction != Direction::Share {
            return None;
        }
        let parsed = parse_outbound_share(payload).ok()?;
        parsed.peer.map(PeerEndpoint)
    }

    fn build_envelope(&self, payload: &JobPayload) -> Result<FederationEnvelope, HandlerOutcome> {
        match self.direction {
            Direction::Share => {
                let share_payload = parse_outbound_share(payload).map_err(|err| {
                    warn!(
                        target: "cairn_workflows::propagation::handler",
                        ?err,
                        "outbound_share payload failed to decode",
                    );
                    HandlerOutcome::validation_permanent(format!("decode share payload: {err}"))
                })?;
                let wire_link = signed_share_link_to_wire(&share_payload.link).map_err(|err| {
                    warn!(
                        target: "cairn_workflows::propagation::handler",
                        ?err,
                        "outbound_share payload could not be re-encoded as wire form",
                    );
                    HandlerOutcome::validation_permanent(format!("encode share wire: {err}"))
                })?;
                let manifest_stubs: Vec<WireStub> = share_payload
                    .manifest
                    .iter()
                    .map(|entry| {
                        let scope = parse_scope_wire(&entry.scope_wire);
                        WireStub {
                            record_id: WireUlid(entry.record_id.clone()),
                            kind: entry.kind.clone(),
                            body: Some(entry.body.clone()),
                            body_hash: entry.body_hash.clone(),
                            visibility: parse_visibility_wire(&entry.visibility),
                            scope,
                            tags: if entry.tags.is_empty() {
                                None
                            } else {
                                Some(entry.tags.clone())
                            },
                        }
                    })
                    .collect();
                Ok(FederationEnvelope {
                    kind: FederationEnvelopeKind::Propose,
                    issuer_key_id: WireIdentity(
                        share_payload.link.payload.issuer.as_str().to_owned(),
                    ),
                    link: Some(wire_link),
                    revocation: None,
                    manifest: if manifest_stubs.is_empty() {
                        None
                    } else {
                        Some(manifest_stubs)
                    },
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
        // Extract the per-job peer override (if any) before building the
        // envelope.  For share payloads the peer rides in the JSON; revoke
        // payloads do not carry one yet.
        let peer_override = self.extract_peer_override(payload);
        let target_peer = peer_override.as_ref().unwrap_or(&self.default_peer);

        let envelope = match self.build_envelope(payload) {
            Ok(env) => env,
            Err(outcome) => return outcome,
        };

        match self.transport.send(&envelope, target_peer).await {
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

/// Parse a visibility wire string into the generated wire enum.
/// Falls back to `Private` for unrecognised values — the receiver's
/// `accept_share` will validate the stub independently.
fn parse_visibility_wire(s: &str) -> WireStubVis {
    match s {
        "session" => WireStubVis::Session,
        "project" => WireStubVis::Project,
        "team" => WireStubVis::Team,
        "org" => WireStubVis::Org,
        "public" => WireStubVis::Public,
        // "private" and any unrecognised value default to Private —
        // the receiver's accept_share validates independently.
        _ => WireStubVis::Private,
    }
}

/// Parse a canonical-wire scope string back into a wire [`WireScope`].
///
/// The canonical wire form is `key=value,key=value` sorted
/// alphabetically. We reconstruct the struct from its key-value pairs.
fn parse_scope_wire(s: &str) -> WireScope {
    let mut scope = WireScope {
        agent: None,
        entity: None,
        project: None,
        session_id: None,
        tenant: None,
        user: None,
        workspace: None,
    };
    for pair in s.split(',') {
        if let Some((key, value)) = pair.split_once('=') {
            let v = Some(value.to_owned());
            match key {
                "agent" => scope.agent = v,
                "entity" => scope.entity = v,
                "project" => scope.project = v,
                "session_id" => scope.session_id = v,
                "tenant" => scope.tenant = v,
                "user" => scope.user = v,
                "workspace" => scope.workspace = v,
                _ => {}
            }
        }
    }
    scope
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
