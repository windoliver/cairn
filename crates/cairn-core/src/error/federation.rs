//! Typed errors for the federation verbs `propose_share` / `accept_share`
//! / `revoke_share` (brief §12.a). Body-free — every variant maps to a
//! `domain::sharing::SharingDecisionKind` (when one exists) so policy
//! traces stay coherent across share-link, consent-receipt, and
//! federation code paths.

use thiserror::Error;

use crate::domain::sharing::SharingDecisionKind;

/// Federation verb error. Body-free for audit safety (brief §14).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FederationError {
    /// Receipt or link expired before apply time.
    #[error("receipt or link expired")]
    Expired,
    /// Target hash or id-hash set does not match the signed payload.
    #[error("target hash mismatch")]
    TargetMismatch,
    /// Requested operation is outside the link's scope tuple.
    #[error("scope outside grant")]
    ScopeMismatch,
    /// Record visibility tier is above the grant tier.
    #[error("visibility tier outside grant")]
    TierMismatch,
    /// Ed25519 signature verification failed.
    #[error("signature verification failed")]
    BadSignature,
    /// Link or receipt was previously revoked.
    #[error("link revoked")]
    Revoked,
    /// Issuer or signer is not a human identity (e.g. agent-only key).
    #[error("signer is not a human identity")]
    NotHuman,
    /// `ReBAC` denied the requested operation at the requested tier.
    #[error("rebac denied")]
    NoRebacRelation,
    /// Envelope shape failed validation before signature check.
    #[error("envelope shape invalid")]
    InvalidShape,
    /// Receiver detected a previously-applied `(issuer_key_id, link_id, nonce)`.
    /// The caller should treat this as a duplicate Ack, not an error.
    #[error("duplicate nonce")]
    DuplicateNonce,
    /// Revoke target or share-link lookup returned no row.
    #[error("unknown share link")]
    UnknownLink,
    /// Federation capability not advertised by this runtime.
    #[error("federation capability not advertised")]
    CapabilityDisabled,
}

impl FederationError {
    /// Mapping to `SharingDecisionKind` for unified policy-trace emission.
    /// Returns `None` for federation-only errors that have no analogue
    /// in the shared share-link decision vocabulary.
    #[must_use]
    pub const fn sharing_decision(&self) -> Option<SharingDecisionKind> {
        match self {
            Self::Expired => Some(SharingDecisionKind::Expired),
            Self::TargetMismatch => Some(SharingDecisionKind::TargetMismatch),
            Self::ScopeMismatch => Some(SharingDecisionKind::ScopeMismatch),
            Self::TierMismatch => Some(SharingDecisionKind::TierMismatch),
            Self::BadSignature => Some(SharingDecisionKind::BadSignature),
            Self::Revoked => Some(SharingDecisionKind::Revoked),
            Self::NotHuman => Some(SharingDecisionKind::NotHuman),
            Self::NoRebacRelation => Some(SharingDecisionKind::NoRebacRelation),
            Self::InvalidShape => Some(SharingDecisionKind::InvalidShape),
            Self::DuplicateNonce | Self::UnknownLink | Self::CapabilityDisabled => None,
        }
    }
}
