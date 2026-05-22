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

use crate::domain::sharing::{ShareLinkPayload, SignedShareLink};
use crate::domain::{DomainError, Ed25519Signature, Identity, MemoryVisibility, Rfc3339Timestamp};
use crate::generated::common::SignedShareLink as WireSignedShareLink;

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

/// Convert a wire-form [`WireSignedShareLink`] (generated from the IDL)
/// into the hand-written [`SignedShareLink`] domain type so the verb
/// layer can call `validate_shape` / `verify_signature` directly.
///
/// Both shapes serialize identically; this conversion threads the
/// strongly-typed primitives (`Ed25519Signature`, `Rfc3339Timestamp`,
/// `Identity`, `MemoryVisibility`) through their `parse` constructors
/// so any malformed field surfaces as a [`DomainError`] before the
/// signature check runs.
///
/// # Errors
///
/// Returns the first [`DomainError`] from any of the typed parsers
/// (signature, timestamps, identity, visibility, `key_version`).
pub fn signed_share_link_from_wire(
    wire: &WireSignedShareLink,
) -> Result<SignedShareLink, DomainError> {
    let signature = Ed25519Signature::parse(wire.signature.0.clone())?;
    let issued_at = Rfc3339Timestamp::parse(wire.payload.issued_at.clone())?;
    let expires_at = Rfc3339Timestamp::parse(wire.payload.expires_at.clone())?;
    let issuer = Identity::parse(wire.payload.issuer.0.clone())?;
    let grantee = match &wire.payload.grantee {
        Some(g) => Some(Identity::parse(g.0.clone())?),
        None => None,
    };
    let grant_tier = MemoryVisibility::parse(grant_tier_to_wire(wire.payload.grant_tier))?;
    let key_version: u32 =
        wire.payload
            .key_version
            .try_into()
            .map_err(|_| DomainError::Unauthorized {
                message: "share link key_version must fit in u32".to_owned(),
            })?;

    let scope = crate::domain::ScopeTuple {
        agent: wire.payload.scope.agent.clone(),
        entity: wire.payload.scope.entity.clone(),
        project: wire.payload.scope.project.clone(),
        session_id: wire.payload.scope.session_id.clone(),
        tenant: wire.payload.scope.tenant.clone(),
        user: wire.payload.scope.user.clone(),
        workspace: wire.payload.scope.workspace.clone(),
    };

    let payload = ShareLinkPayload {
        operation_id: wire.payload.operation_id.0.clone(),
        nonce: wire.payload.nonce.0.clone(),
        target_hash: wire.payload.target_hash.clone(),
        target_id_hashes: wire.payload.target_id_hashes.clone(),
        scope,
        grant_tier,
        grantee,
        issuer,
        issued_at,
        expires_at,
        key_version,
    };

    Ok(SignedShareLink {
        link_id: wire.link_id.clone(),
        payload,
        signature,
    })
}

const fn grant_tier_to_wire(
    tier: crate::generated::common::ShareLinkPayloadGrantTier,
) -> &'static str {
    use crate::generated::common::ShareLinkPayloadGrantTier as T;
    match tier {
        T::Session => "session",
        T::Project => "project",
        T::Team => "team",
        T::Org => "org",
        T::Public => "public",
    }
}
