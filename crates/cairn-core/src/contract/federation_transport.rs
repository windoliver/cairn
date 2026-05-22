//! Pluggable transport for federation envelopes (brief §12.a).
//!
//! The default in-process adapter ships in `cairn-test-fixtures` and is
//! used by integration tests. Production deployments will wire an HTTP
//! adapter as a separate crate (follow-up to issue #123).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::domain::federation::{FederationEnvelope, PeerEndpoint};

/// Body-free reason carried with transport outcomes. Strings are
/// operator-facing diagnostics and must not include record bodies or
/// other consent-gated content (brief §14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransportReason(pub String);

/// Result of one [`FederationTransport::send`] attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SendOutcome {
    /// Peer acknowledged successful apply.
    Ack,
    /// Retryable failure (timeouts, 5xx, network blip). The scheduler
    /// will requeue with exponential backoff up to the workflow's
    /// retry policy.
    Transient(TransportReason),
    /// Non-retryable failure (4xx, signature reject at peer, peer `ReBAC`
    /// deny). The scheduler marks the job dead immediately.
    Permanent(TransportReason),
}

/// Pluggable transport for outbound federation envelopes.
///
/// Implementations must be cancel-safe: the scheduler may drop the
/// future after a lease expires. They must also be safe to call
/// concurrently from multiple worker tasks.
#[async_trait]
pub trait FederationTransport: Send + Sync {
    /// Send one envelope to one peer. The transport is responsible for
    /// classifying the network outcome as `Ack`, `Transient`, or
    /// `Permanent`; it must not retry internally — retry policy lives
    /// in the scheduler.
    async fn send(&self, envelope: &FederationEnvelope, peer: &PeerEndpoint) -> SendOutcome;
}
