//! Federation propagation workflow module (brief §12.a, §10).
//!
//! Provides the typed payloads and job-kind constants shared between the
//! enqueueing verbs (T7 `propose_share`, T9 `revoke_share`) and the
//! dispatching handler (T13 `PropagationHandler`).
//!
//! ## Structure
//!
//! - [`payload`] — kind constants, type aliases, and parse/serialize helpers.
//! - [`handler`] — `PropagationHandler`, the `JobHandler` that drains the
//!   outbound queue via a [`cairn_core::contract::federation_transport::FederationTransport`].

pub mod handler;
pub mod payload;

pub use handler::PropagationHandler;
pub use payload::{
    OUTBOUND_REVOKE_KIND, OUTBOUND_SHARE_KIND, OutboundRevokePayload, OutboundSharePayload,
    outbound_revoke_to_bytes, outbound_share_to_bytes, parse_outbound_revoke,
    parse_outbound_share,
};
