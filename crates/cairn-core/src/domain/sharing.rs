//! Signed share links and promotion consent receipts (brief §4.2, §5.6, §12.a, §14).
//!
//! This module is pure domain logic: no I/O, no keychain lookup, no DB writes.

use serde::{Deserialize, Serialize};

/// Policy-trace subject for sharing gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharingPolicySubject {
    /// Promotion consent receipt gate.
    ConsentReceipt,
    /// Signed share-link grant gate.
    ShareLink,
}

impl SharingPolicySubject {
    /// Body-free detail token.
    #[must_use]
    pub const fn as_detail_str(self) -> &'static str {
        match self {
            Self::ConsentReceipt => "consent",
            Self::ShareLink => "share_link",
        }
    }
}

/// Sharing action captured in policy trace detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharingPolicyAction {
    /// Promotion across visibility tiers.
    Promote,
    /// Share-link grant evaluation.
    Grant,
}

impl SharingPolicyAction {
    /// Body-free detail token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Promote => "promote",
            Self::Grant => "grant",
        }
    }
}

/// Body-free decision reason for signed sharing gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharingDecisionKind {
    /// Gate allowed the operation.
    Allowed,
    /// Receipt or link expired before apply/evaluation.
    Expired,
    /// Target hash or target-id hash set did not match.
    TargetMismatch,
    /// Scope did not match the requested operation.
    ScopeMismatch,
    /// Tier did not match or exceeded the grant.
    TierMismatch,
    /// Signature verification failed.
    BadSignature,
    /// Receipt, link, or signer key was revoked.
    Revoked,
    /// Issuer or signer was not a human identity.
    NotHuman,
    /// ReBAC denied the shared-tier write.
    NoRebacRelation,
    /// Shape validation failed before signature verification.
    InvalidShape,
}

impl SharingDecisionKind {
    /// Body-free detail token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Expired => "expired",
            Self::TargetMismatch => "target_mismatch",
            Self::ScopeMismatch => "scope_mismatch",
            Self::TierMismatch => "tier_mismatch",
            Self::BadSignature => "bad_signature",
            Self::Revoked => "revoked",
            Self::NotHuman => "not_human",
            Self::NoRebacRelation => "no_rebac_relation",
            Self::InvalidShape => "invalid_shape",
        }
    }
}
