//! Federation envelope domain helpers (brief §12.a).
//!
//! The wire types ([`FederationEnvelope`], [`FederationEnvelopeKind`],
//! [`SignedRevocation`], [`MemoryRecordStub`]) are codegenerated from
//! the `cairn.federation.v1` IDL and re-exported here so downstream
//! code imports them from a stable `domain::federation::*` path.
//!
//! This module adds:
//! - [`PeerEndpoint`], a hand-written newtype around the peer-address
//!   string (Rust-side abstraction, no envelope counterpart).
//! - [`FederationEnvelopeExt::dedup_key`], the idempotency key used by
//!   `accept_share`.

use serde::{Deserialize, Serialize};

pub use crate::generated::common::{
    FederationEnvelope, FederationEnvelopeKind, MemoryRecordStub, SignedRevocation,
};

/// Pluggable peer address. The `FederationTransport` interprets this;
/// core only stores and forwards it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerEndpoint(pub String);

/// Idempotency key for federation propose envelopes.
///
/// Propose envelopes dedupe by `(issuer_key_id, link_id, nonce)`. Revoke
/// envelopes dedupe by `link_id` alone (any sender can issue the revoke
/// signal, but the receiver-side projection is unique per link).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DedupKey<'a> {
    /// Stable key identifier of the issuing principal.
    pub issuer_key_id: &'a str,
    /// Unique share-link identifier.
    pub link_id: &'a str,
    /// One-time nonce from the share-link payload.
    pub nonce: &'a str,
}

/// Extension trait adding helper methods to the codegenerated
/// [`FederationEnvelope`].
pub trait FederationEnvelopeExt {
    /// Idempotency key for `accept_share` dedup. Returns `None` for
    /// revoke envelopes, which dedup by `link_id` via the consent journal.
    fn dedup_key(&self) -> Option<DedupKey<'_>>;
}

impl FederationEnvelopeExt for FederationEnvelope {
    fn dedup_key(&self) -> Option<DedupKey<'_>> {
        // Revoke envelopes have no link — they dedup differently.
        let link = self.link.as_ref()?;
        Some(DedupKey {
            issuer_key_id: self.issuer_key_id.0.as_str(),
            link_id: link.link_id.as_str(),
            nonce: link.payload.nonce.0.as_str(),
        })
    }
}
