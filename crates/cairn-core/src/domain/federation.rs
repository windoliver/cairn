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
use crate::generated::common::{
    Ed25519Signature as WireEd25519Signature, Identity as WireIdentity,
    Nonce16Base64 as WireNonce, ScopeTuple as WireScopeTuple,
    ShareLinkPayload as WireShareLinkPayload, ShareLinkPayloadGrantTier as WireGrantTier,
    SignedShareLink as WireSignedShareLink, Ulid as WireUlid,
};

/// Pluggable peer address. The `FederationTransport` interprets this;
/// core only stores and forwards it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerEndpoint(pub String);

/// Idempotency key for federation propose envelopes.
///
/// Propose envelopes dedupe by `(issuer_key_id, link_id)`. The `link_id`
/// is already nonce-bound — each propose mints a fresh `link_id` together
/// with a nonce — so dropping the nonce from the dedup key loses nothing.
/// `FederationAccept` consent rows do not persist the nonce, making
/// nonce-based dedup unimplementable at the storage layer.
///
/// Revoke envelopes dedupe by `link_id` alone (any sender can issue the
/// revoke signal, but the receiver-side projection is unique per link).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DedupKey<'a> {
    /// Stable key identifier of the issuing principal.
    pub issuer_key_id: &'a str,
    /// Unique share-link identifier.
    pub link_id: &'a str,
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

fn grant_tier_from_domain(tier: MemoryVisibility) -> Result<WireGrantTier, DomainError> {
    use MemoryVisibility as V;
    match tier {
        V::Session => Ok(WireGrantTier::Session),
        V::Project => Ok(WireGrantTier::Project),
        V::Team => Ok(WireGrantTier::Team),
        V::Org => Ok(WireGrantTier::Org),
        V::Public => Ok(WireGrantTier::Public),
        V::Private => Err(DomainError::Unauthorized {
            message: "share link grant_tier may not be 'private'".to_owned(),
        }),
        // `MemoryVisibility` is `#[non_exhaustive]`; any future tier added to
        // the enum must be wired explicitly. Returning `Unauthorized` for
        // anything unrecognised is the same failure mode as `Private` —
        // signed share links never carry unknown tiers across federation.
        #[allow(unreachable_patterns)]
        _ => Err(DomainError::Unauthorized {
            message: "share link grant_tier is not federation-eligible".to_owned(),
        }),
    }
}

/// Convert the hand-written [`SignedShareLink`] domain type back into the
/// wire-form [`WireSignedShareLink`] (generated from the IDL) so the
/// propagation handler can embed it inside a [`FederationEnvelope`].
///
/// This is the inverse of [`signed_share_link_from_wire`]; both shapes
/// serialize identically, so a domain → wire → domain round-trip is
/// lossless.
///
/// # Errors
///
/// Returns [`DomainError`] when:
/// * `key_version` does not fit in `i64` (effectively unreachable —
///   `u32::MAX < i64::MAX`).
/// * `grant_tier` is `MemoryVisibility::Private` (or any future non-
///   federation-eligible tier), which has no wire representation.
pub fn signed_share_link_to_wire(
    link: &SignedShareLink,
) -> Result<WireSignedShareLink, DomainError> {
    let payload = &link.payload;
    let scope = WireScopeTuple {
        agent: payload.scope.agent.clone(),
        entity: payload.scope.entity.clone(),
        project: payload.scope.project.clone(),
        session_id: payload.scope.session_id.clone(),
        tenant: payload.scope.tenant.clone(),
        user: payload.scope.user.clone(),
        workspace: payload.scope.workspace.clone(),
    };

    let wire_payload = WireShareLinkPayload {
        expires_at: payload.expires_at.as_str().to_owned(),
        grant_tier: grant_tier_from_domain(payload.grant_tier)?,
        grantee: payload.grantee.as_ref().map(|g| WireIdentity(g.as_str().to_owned())),
        issued_at: payload.issued_at.as_str().to_owned(),
        issuer: WireIdentity(payload.issuer.as_str().to_owned()),
        key_version: i64::from(payload.key_version),
        nonce: WireNonce(payload.nonce.clone()),
        operation_id: WireUlid(payload.operation_id.clone()),
        scope,
        target_hash: payload.target_hash.clone(),
        target_id_hashes: payload.target_id_hashes.clone(),
    };

    Ok(WireSignedShareLink {
        link_id: link.link_id.clone(),
        payload: wire_payload,
        signature: WireEd25519Signature(link.signature.as_str().to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_envelope() -> FederationEnvelope {
        let raw = include_str!("../../tests/fixtures/federation/propose_envelope.json");
        serde_json::from_str(raw).expect("parse propose_envelope.json")
    }

    #[test]
    fn signed_share_link_wire_round_trip_is_lossless() {
        let envelope = fixture_envelope();
        let wire_link = envelope.link.clone().expect("propose has link");
        let domain = signed_share_link_from_wire(&wire_link).expect("from_wire");
        let back = signed_share_link_to_wire(&domain).expect("to_wire");
        assert_eq!(wire_link, back);
    }

    #[test]
    fn signed_share_link_to_wire_then_from_wire_is_lossless() {
        let envelope = fixture_envelope();
        let wire_link = envelope.link.clone().expect("propose has link");
        let domain1 = signed_share_link_from_wire(&wire_link).expect("from_wire");
        let wire2 = signed_share_link_to_wire(&domain1).expect("to_wire");
        let domain2 = signed_share_link_from_wire(&wire2).expect("from_wire 2");
        assert_eq!(domain1, domain2);
    }

    #[test]
    fn signed_share_link_to_wire_rejects_private_tier() {
        let envelope = fixture_envelope();
        let wire_link = envelope.link.clone().expect("propose has link");
        let mut domain = signed_share_link_from_wire(&wire_link).expect("from_wire");
        domain.payload.grant_tier = MemoryVisibility::Private;
        let err = signed_share_link_to_wire(&domain).expect_err("private rejected");
        assert!(matches!(err, DomainError::Unauthorized { .. }));
    }
}
