//! In-memory `FederationTransport` for tests.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cairn_core::contract::federation_transport::{
    FederationTransport, SendOutcome, TransportReason,
};
use cairn_core::domain::federation::{FederationEnvelope, PeerEndpoint};

/// One programmed outcome the loopback transport returns from its next
/// `send` call. Built in declarative order; the transport consumes them
/// FIFO.
#[derive(Debug, Clone)]
pub enum ProgrammedOutcome {
    /// Return [`SendOutcome::Ack`].
    Ack,
    /// Return [`SendOutcome::Transient`] with the given reason string.
    Transient(String),
    /// Return [`SendOutcome::Permanent`] with the given reason string.
    Permanent(String),
}

/// In-memory `FederationTransport`. Records every envelope sent to it
/// and serves outcomes from a programmable FIFO queue. If the queue is
/// empty when `send` is called, the transport returns `Ack` by default.
#[derive(Default, Clone)]
pub struct LoopbackTransport {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    queued: Vec<ProgrammedOutcome>,
    sent: Vec<(FederationEnvelope, PeerEndpoint)>,
}

impl LoopbackTransport {
    /// Create a new, empty loopback transport.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue outcomes returned by subsequent `send` calls, in order.
    // Mutex poisoning is a programmer error (panic in a holding task) and
    // should propagate loudly — `expect` is the correct call here.
    #[allow(clippy::expect_used)]
    pub fn program(&self, outcomes: impl IntoIterator<Item = ProgrammedOutcome>) {
        let mut g = self.inner.lock().expect("loopback lock poisoned");
        g.queued.extend(outcomes);
    }

    /// All envelopes the transport has received, in send order.
    #[must_use]
    // Mutex poisoning is a programmer error — `expect` is correct here.
    #[allow(clippy::expect_used)]
    pub fn sent(&self) -> Vec<(FederationEnvelope, PeerEndpoint)> {
        self.inner
            .lock()
            .expect("loopback lock poisoned")
            .sent
            .clone()
    }

    /// Programmed outcomes still queued (not yet consumed by `send`).
    #[must_use]
    // Mutex poisoning is a programmer error — `expect` is correct here.
    #[allow(clippy::expect_used)]
    pub fn remaining(&self) -> usize {
        self.inner
            .lock()
            .expect("loopback lock poisoned")
            .queued
            .len()
    }
}

#[async_trait]
impl FederationTransport for LoopbackTransport {
    // Mutex poisoning is a programmer error — `expect` is correct here.
    #[allow(clippy::expect_used)]
    async fn send(&self, env: &FederationEnvelope, peer: &PeerEndpoint) -> SendOutcome {
        let mut g = self.inner.lock().expect("loopback lock poisoned");
        g.sent.push((env.clone(), peer.clone()));
        let outcome = if g.queued.is_empty() {
            ProgrammedOutcome::Ack
        } else {
            g.queued.remove(0)
        };
        match outcome {
            ProgrammedOutcome::Ack => SendOutcome::Ack,
            ProgrammedOutcome::Transient(r) => SendOutcome::Transient(TransportReason(r)),
            ProgrammedOutcome::Permanent(r) => SendOutcome::Permanent(TransportReason(r)),
        }
    }
}
