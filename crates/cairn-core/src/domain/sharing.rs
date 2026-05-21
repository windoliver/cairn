//! Signed share links and promotion consent receipts (brief §4.2, §5.6, §12.a, §14).
//!
//! This module is pure domain logic: no I/O, no keychain lookup, no DB writes.

use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::domain::{
    CanonicalRecordHash, DomainError, Ed25519Signature, Identity, MemoryRecord, MemoryVisibility,
    Rfc3339Timestamp, ScopeTuple,
};
use crate::policy_trace::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry};
use crate::rebac::{RebacAction, RebacContext};

const MAX_TARGET_ID_HASHES: usize = 64;

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

/// Signed consent receipt authorizing one visibility promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionConsentReceipt {
    /// Stable receipt identifier used for revocation and journal references.
    pub receipt_id: String,
    /// Signed body-free receipt payload.
    pub payload: PromotionConsentPayload,
    /// Ed25519 signature over canonical JSON of `payload`.
    pub signature: Ed25519Signature,
}

/// Body-free payload signed by a human for shared-tier promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionConsentPayload {
    /// WAL operation id this receipt authorizes.
    pub operation_id: String,
    /// Fresh 16-byte base64 nonce.
    pub nonce: String,
    /// Parent operation ids this promotion depends on.
    pub chain_parents: Vec<String>,
    /// Canonical hash of the record being promoted.
    pub target_hash: String,
    /// Salted or canonical hash of the target id.
    pub target_id_hash: String,
    /// Current record visibility tier.
    pub from_tier: MemoryVisibility,
    /// Requested promoted visibility tier.
    pub to_tier: MemoryVisibility,
    /// Scope the receipt authorizes.
    pub scope: ScopeTuple,
    /// Human principal that signed the receipt.
    pub human_identity: Identity,
    /// Receipt issue time.
    pub issued_at: Rfc3339Timestamp,
    /// Receipt expiry time.
    pub expires_at: Rfc3339Timestamp,
    /// Signer's key version.
    pub key_version: u32,
}

/// Signed share link authorizing a bounded grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedShareLink {
    /// Stable link id used for revocation and consent journal subject codes.
    pub link_id: String,
    /// Signed body-free link payload.
    pub payload: ShareLinkPayload,
    /// Ed25519 signature over canonical JSON of `payload`.
    pub signature: Ed25519Signature,
}

/// Body-free payload for a signed share-link grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareLinkPayload {
    /// Operation id that minted the link.
    pub operation_id: String,
    /// Fresh 16-byte base64 nonce.
    pub nonce: String,
    /// Hash of the record-set manifest or target grant payload.
    pub target_hash: String,
    /// Salted or canonical target-id hashes included in the grant.
    pub target_id_hashes: Vec<String>,
    /// Scope the link may expose.
    pub scope: ScopeTuple,
    /// Shared tier granted by the link.
    pub grant_tier: MemoryVisibility,
    /// Optional grantee. `None` means bearer-style access.
    pub grantee: Option<Identity>,
    /// Human issuer that granted the share link.
    pub issuer: Identity,
    /// Link issue time.
    pub issued_at: Rfc3339Timestamp,
    /// Link expiry time.
    pub expires_at: Rfc3339Timestamp,
    /// Issuer key version.
    pub key_version: u32,
}

/// Revocation state supplied by the store or identity layer at evaluation time.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharingRevocationState {
    /// Receipt ids that can no longer authorize promotion.
    pub revoked_receipt_ids: BTreeSet<String>,
    /// Share link ids that can no longer authorize grants.
    pub revoked_share_link_ids: BTreeSet<String>,
    /// Whether the current signer key is revoked.
    pub signer_key_revoked: bool,
}

/// Inputs for the shared-tier promotion gate.
pub struct PromotionGateInput<'a> {
    /// Record being promoted.
    pub record: &'a MemoryRecord,
    /// Current visibility tier requested by the apply operation.
    pub from_tier: MemoryVisibility,
    /// Target visibility tier requested by the apply operation.
    pub to_tier: MemoryVisibility,
    /// Signed consent receipt authorizing the promotion.
    pub receipt: &'a PromotionConsentReceipt,
    /// Apply-time wall clock.
    pub now: &'a Rfc3339Timestamp,
    /// WAL operation id being applied.
    pub operation_id: &'a str,
    /// Human signer verifying key.
    pub signer_key: &'a VerifyingKey,
    /// Apply-time revocation state.
    pub revocation: &'a SharingRevocationState,
    /// Apply-time `ReBAC` relation context.
    pub rebac: &'a RebacContext,
}

/// Inputs for signed share-link grant evaluation.
pub struct ShareLinkGateInput<'a> {
    /// Signed link being evaluated.
    pub link: &'a SignedShareLink,
    /// Evaluation timestamp.
    pub now: &'a Rfc3339Timestamp,
    /// Expected record-set or grant-manifest hash.
    pub expected_target_hash: &'a str,
    /// Verifying key for `link.payload.issuer`.
    pub signer_key: &'a VerifyingKey,
    /// Revocation state supplied by store/identity layer.
    pub revocation: &'a SharingRevocationState,
    /// Maximum tier the issuer may grant.
    pub max_tier: MemoryVisibility,
}

/// Denial with both typed error and body-free trace.
#[derive(Debug)]
pub struct SharingGateRejection {
    /// Typed domain error for the caller.
    pub error: DomainError,
    /// Body-free policy trace for audit and response surfaces.
    pub trace: PolicyTraceEntry,
}

impl PromotionConsentReceipt {
    /// Validate body-free shape before or after signature verification.
    pub fn validate_shape(&self) -> Result<(), DomainError> {
        validate_id("receipt_id", &self.receipt_id)?;
        validate_ulid("operation_id", &self.payload.operation_id)?;
        validate_bound_id(
            "receipt_id",
            &self.receipt_id,
            "rcpt-",
            &self.payload.operation_id,
        )?;
        validate_nonce(&self.payload.nonce)?;
        validate_chain_parents(&self.payload.chain_parents)?;
        validate_target_hash(&self.payload.target_hash)?;
        validate_hash("target_id_hash", &self.payload.target_id_hash)?;
        validate_shared_promotion_tiers(self.payload.from_tier, self.payload.to_tier)?;
        self.payload.scope.validate()?;
        if self.payload.human_identity.kind() != crate::domain::IdentityKind::Human {
            return Err(DomainError::Unauthorized {
                message: "promotion receipt signer must be a human identity".to_owned(),
            });
        }
        if self.payload.key_version == 0 {
            return Err(DomainError::Unauthorized {
                message: "promotion receipt key_version must be >= 1".to_owned(),
            });
        }
        if self
            .payload
            .expires_at
            .cmp_chronological(&self.payload.issued_at)
            != std::cmp::Ordering::Greater
        {
            return Err(DomainError::ExpiredIntent {
                issued_at: self.payload.issued_at.as_str().to_owned(),
                expires_at: self.payload.expires_at.as_str().to_owned(),
                now: self.payload.expires_at.as_str().to_owned(),
            });
        }
        Ok(())
    }

    /// Verify the Ed25519 signature over canonical payload bytes.
    pub fn verify_signature(&self, key: &VerifyingKey) -> Result<(), DomainError> {
        self.validate_shape()?;
        let bytes = crate::domain::canonical::canonical_bytes(&self.payload)?;
        let signature = decode_signature(&self.signature)?;
        key.verify(&bytes, &signature)
            .map_err(|_| DomainError::InvalidSignature)
    }
}

impl SignedShareLink {
    /// Validate body-free shape before or after signature verification.
    pub fn validate_shape(&self) -> Result<(), DomainError> {
        validate_id("link_id", &self.link_id)?;
        validate_ulid("operation_id", &self.payload.operation_id)?;
        validate_bound_id(
            "link_id",
            &self.link_id,
            "share-",
            &self.payload.operation_id,
        )?;
        validate_nonce(&self.payload.nonce)?;
        validate_target_hash(&self.payload.target_hash)?;
        validate_target_id_hashes(&self.payload.target_id_hashes)?;
        validate_shared_tier("grant_tier", self.payload.grant_tier)?;
        self.payload.scope.validate()?;
        if self.payload.issuer.kind() != crate::domain::IdentityKind::Human {
            return Err(DomainError::Unauthorized {
                message: "share link issuer must be a human identity".to_owned(),
            });
        }
        if self.payload.key_version == 0 {
            return Err(DomainError::Unauthorized {
                message: "share link key_version must be >= 1".to_owned(),
            });
        }
        if self
            .payload
            .expires_at
            .cmp_chronological(&self.payload.issued_at)
            != std::cmp::Ordering::Greater
        {
            return Err(DomainError::ExpiredIntent {
                issued_at: self.payload.issued_at.as_str().to_owned(),
                expires_at: self.payload.expires_at.as_str().to_owned(),
                now: self.payload.expires_at.as_str().to_owned(),
            });
        }
        Ok(())
    }

    /// Verify the Ed25519 signature over canonical payload bytes.
    pub fn verify_signature(&self, key: &VerifyingKey) -> Result<(), DomainError> {
        self.validate_shape()?;
        let bytes = crate::domain::canonical::canonical_bytes(&self.payload)?;
        let signature = decode_signature(&self.signature)?;
        key.verify(&bytes, &signature)
            .map_err(|_| DomainError::InvalidSignature)
    }
}

/// Verify that a signed consent receipt authorizes a shared-tier promotion at
/// apply time.
pub fn verify_promotion_gate(
    input: PromotionGateInput<'_>,
) -> Result<PolicyTraceEntry, SharingGateRejection> {
    if input.receipt.payload.human_identity.kind() != crate::domain::IdentityKind::Human {
        return Err(reject_promotion(
            SharingDecisionKind::NotHuman,
            DomainError::Unauthorized {
                message: "promotion receipt signer must be a human identity".to_owned(),
            },
        ));
    }

    if let Err(error) = input.receipt.validate_shape() {
        return Err(reject_promotion(SharingDecisionKind::InvalidShape, error));
    }

    let signature_result = (|| {
        let bytes = crate::domain::canonical::canonical_bytes(&input.receipt.payload)
            .map_err(|_| DomainError::InvalidSignature)?;
        let signature = decode_signature(&input.receipt.signature)?;
        input
            .signer_key
            .verify(&bytes, &signature)
            .map_err(|_| DomainError::InvalidSignature)
    })();
    if signature_result.is_err() {
        return Err(reject_promotion(
            SharingDecisionKind::BadSignature,
            DomainError::InvalidSignature,
        ));
    }

    if input.record.visibility != input.from_tier {
        return Err(reject_promotion(
            SharingDecisionKind::TierMismatch,
            DomainError::UnsupportedVisibility {
                value: format!(
                    "record visibility `{}` does not match requested from_tier `{}`",
                    input.record.visibility.as_str(),
                    input.from_tier.as_str()
                ),
            },
        ));
    }

    let target_hash_matches = CanonicalRecordHash::compute(input.record)
        .map(|hash| hash.as_str() == input.receipt.payload.target_hash)
        .unwrap_or(false);
    if !target_hash_matches {
        return Err(reject_promotion(
            SharingDecisionKind::TargetMismatch,
            DomainError::InvalidPayloadHash {
                message: "promotion receipt target_hash does not match record".to_owned(),
            },
        ));
    }

    if input.receipt.payload.scope != input.record.scope {
        return Err(reject_promotion(
            SharingDecisionKind::ScopeMismatch,
            DomainError::ScopeDenied {
                message: "promotion receipt scope does not match record scope".to_owned(),
            },
        ));
    }

    if input.receipt.payload.operation_id != input.operation_id {
        return Err(reject_promotion(
            SharingDecisionKind::ScopeMismatch,
            DomainError::ScopeDenied {
                message: "promotion receipt operation_id does not match apply operation".to_owned(),
            },
        ));
    }

    if input.receipt.payload.from_tier != input.from_tier
        || input.receipt.payload.to_tier != input.to_tier
    {
        return Err(reject_promotion(
            SharingDecisionKind::TierMismatch,
            DomainError::UnsupportedVisibility {
                value: format!(
                    "receipt tiers {}->{} do not match requested {}->{}",
                    input.receipt.payload.from_tier.as_str(),
                    input.receipt.payload.to_tier.as_str(),
                    input.from_tier.as_str(),
                    input.to_tier.as_str()
                ),
            },
        ));
    }

    if input
        .now
        .cmp_chronological(&input.receipt.payload.expires_at)
        != std::cmp::Ordering::Less
    {
        return Err(reject_promotion(
            SharingDecisionKind::Expired,
            DomainError::ExpiredIntent {
                issued_at: input.receipt.payload.issued_at.as_str().to_owned(),
                expires_at: input.receipt.payload.expires_at.as_str().to_owned(),
                now: input.now.as_str().to_owned(),
            },
        ));
    }

    if input.revocation.signer_key_revoked
        || input
            .revocation
            .revoked_receipt_ids
            .contains(&input.receipt.receipt_id)
    {
        return Err(reject_promotion(
            SharingDecisionKind::Revoked,
            DomainError::Unauthorized {
                message: "promotion receipt or signer key was revoked".to_owned(),
            },
        ));
    }

    if input.rebac.principal() != Some(&input.receipt.payload.human_identity) {
        return Err(reject_promotion(
            SharingDecisionKind::NoRebacRelation,
            DomainError::ScopeDenied {
                message: "ReBAC principal does not match promotion receipt signer".to_owned(),
            },
        ));
    }

    let rebac_decision = input.rebac.evaluate(
        RebacAction::Write,
        &input.receipt.payload.scope,
        input.to_tier,
    );
    if !rebac_decision.allowed() {
        return Err(reject_promotion(
            SharingDecisionKind::NoRebacRelation,
            DomainError::ScopeDenied {
                message: "missing ReBAC write relation for promotion".to_owned(),
            },
        ));
    }

    Ok(sharing_trace(
        PolicyGate::ConsentReceipt,
        PolicyOutcome::Pass,
        SharingPolicySubject::ConsentReceipt,
        SharingPolicyAction::Promote,
        SharingDecisionKind::Allowed,
    ))
}

/// Verify a signed share link before honoring the grant.
pub fn verify_share_link_grant(
    input: ShareLinkGateInput<'_>,
) -> Result<PolicyTraceEntry, SharingGateRejection> {
    if input.link.payload.issuer.kind() != crate::domain::IdentityKind::Human {
        return Err(reject_share_link(
            SharingDecisionKind::NotHuman,
            DomainError::Unauthorized {
                message: "share link issuer must be a human identity".to_owned(),
            },
        ));
    }

    if let Err(error) = input.link.validate_shape() {
        return Err(reject_share_link(SharingDecisionKind::InvalidShape, error));
    }

    let signature_result = (|| {
        let bytes = crate::domain::canonical::canonical_bytes(&input.link.payload)
            .map_err(|_| DomainError::InvalidSignature)?;
        let signature = decode_signature(&input.link.signature)?;
        input
            .signer_key
            .verify(&bytes, &signature)
            .map_err(|_| DomainError::InvalidSignature)
    })();
    if signature_result.is_err() {
        return Err(reject_share_link(
            SharingDecisionKind::BadSignature,
            DomainError::InvalidSignature,
        ));
    }

    if input.link.payload.target_hash != input.expected_target_hash {
        return Err(reject_share_link(
            SharingDecisionKind::TargetMismatch,
            DomainError::InvalidPayloadHash {
                message: "share link target_hash does not match expected target".to_owned(),
            },
        ));
    }

    if input.link.payload.grant_tier > input.max_tier {
        return Err(reject_share_link(
            SharingDecisionKind::TierMismatch,
            DomainError::UnsupportedVisibility {
                value: format!(
                    "share link grant_tier `{}` exceeds authorized max `{}`",
                    input.link.payload.grant_tier.as_str(),
                    input.max_tier.as_str()
                ),
            },
        ));
    }

    if input.now.cmp_chronological(&input.link.payload.expires_at) != std::cmp::Ordering::Less {
        return Err(reject_share_link(
            SharingDecisionKind::Expired,
            DomainError::ExpiredIntent {
                issued_at: input.link.payload.issued_at.as_str().to_owned(),
                expires_at: input.link.payload.expires_at.as_str().to_owned(),
                now: input.now.as_str().to_owned(),
            },
        ));
    }

    if input.revocation.signer_key_revoked
        || input
            .revocation
            .revoked_share_link_ids
            .contains(&input.link.link_id)
    {
        return Err(reject_share_link(
            SharingDecisionKind::Revoked,
            DomainError::Unauthorized {
                message: "share link or signer key was revoked".to_owned(),
            },
        ));
    }

    Ok(sharing_trace(
        PolicyGate::ShareLink,
        PolicyOutcome::Pass,
        SharingPolicySubject::ShareLink,
        SharingPolicyAction::Grant,
        SharingDecisionKind::Allowed,
    ))
}

fn reject_promotion(reason: SharingDecisionKind, error: DomainError) -> SharingGateRejection {
    SharingGateRejection {
        error,
        trace: sharing_trace(
            PolicyGate::ConsentReceipt,
            PolicyOutcome::Deny,
            SharingPolicySubject::ConsentReceipt,
            SharingPolicyAction::Promote,
            reason,
        ),
    }
}

fn reject_share_link(reason: SharingDecisionKind, error: DomainError) -> SharingGateRejection {
    SharingGateRejection {
        error,
        trace: sharing_trace(
            PolicyGate::ShareLink,
            PolicyOutcome::Deny,
            SharingPolicySubject::ShareLink,
            SharingPolicyAction::Grant,
            reason,
        ),
    }
}

fn sharing_trace(
    gate: PolicyGate,
    outcome: PolicyOutcome,
    subject: SharingPolicySubject,
    action: SharingPolicyAction,
    reason: SharingDecisionKind,
) -> PolicyTraceEntry {
    PolicyTraceEntry::new(
        gate,
        outcome,
        PolicyDetail::Sharing {
            subject,
            action,
            reason,
        },
    )
}

fn validate_id(field: &'static str, value: &str) -> Result<(), DomainError> {
    if value.is_empty() || value.len() > 128 {
        return Err(DomainError::EmptyField { field });
    }
    if !value
        .bytes()
        .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'-'))
    {
        return Err(DomainError::MalformedScope {
            message: format!("{field} chars must be in [A-Za-z0-9._:-]"),
        });
    }
    Ok(())
}

fn validate_ulid(field: &'static str, value: &str) -> Result<(), DomainError> {
    ulid::Ulid::from_string(value).map_err(|_| DomainError::MalformedScope {
        message: format!("{field} must be a ULID"),
    })?;
    Ok(())
}

fn validate_bound_id(
    field: &'static str,
    value: &str,
    prefix: &str,
    operation_id: &str,
) -> Result<(), DomainError> {
    let expected = format!("{prefix}{operation_id}");
    if value != expected {
        return Err(DomainError::MalformedScope {
            message: format!("{field} must equal `{expected}`"),
        });
    }
    Ok(())
}

fn validate_chain_parents(values: &[String]) -> Result<(), DomainError> {
    if values.len() > 64 {
        return Err(DomainError::MalformedScope {
            message: "chain_parents exceeds 64 items".to_owned(),
        });
    }
    let mut seen = BTreeSet::new();
    for value in values {
        validate_ulid("chain_parents", value)?;
        if !seen.insert(value) {
            return Err(DomainError::MalformedScope {
                message: "chain_parents must be unique".to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_nonce(value: &str) -> Result<(), DomainError> {
    let bytes = value.as_bytes();
    let (head, tail) = match bytes.len() {
        22 => (&bytes[..21], bytes[21]),
        24 if bytes[22] == b'=' && bytes[23] == b'=' => (&bytes[..21], bytes[21]),
        _ => {
            return Err(DomainError::MissingSignature {
                message: "nonce must encode 16 base64 bytes".to_owned(),
            });
        }
    };
    let base64 = |b: &u8| matches!(*b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/');
    if !head.iter().all(base64) || !matches!(tail, b'A' | b'Q' | b'g' | b'w') {
        return Err(DomainError::MissingSignature {
            message: "nonce must be canonical 16-byte base64".to_owned(),
        });
    }
    Ok(())
}

fn validate_target_hash(value: &str) -> Result<(), DomainError> {
    if let Some(hex) = value.strip_prefix("sha256:")
        && hex.len() == 64
        && is_lowercase_hex(hex)
    {
        return Ok(());
    }
    Err(DomainError::InvalidPayloadHash {
        message: "target_hash must be sha256:<64 lowercase hex>".to_owned(),
    })
}

fn validate_hash(field: &'static str, value: &str) -> Result<(), DomainError> {
    if let Some(hex) = value.strip_prefix("sha256:") {
        if hex.len() == 64 && is_lowercase_hex(hex) {
            return Ok(());
        }
    }
    if let Some(hex) = value.strip_prefix("hash:") {
        if (32..=128).contains(&hex.len()) && is_lowercase_hex(hex) {
            return Ok(());
        }
    }
    Err(DomainError::InvalidPayloadHash {
        message: format!(
            "{field} must be sha256:<64 lowercase hex> or hash:<32..=128 lowercase hex>"
        ),
    })
}

fn validate_target_id_hashes(values: &[String]) -> Result<(), DomainError> {
    if values.is_empty() {
        return Err(DomainError::InvalidPayloadHash {
            message: "target_id_hashes must not be empty".to_owned(),
        });
    }
    if values.len() > MAX_TARGET_ID_HASHES {
        return Err(DomainError::InvalidPayloadHash {
            message: format!("target_id_hashes exceeds {MAX_TARGET_ID_HASHES} items"),
        });
    }

    let mut seen = BTreeSet::new();
    for value in values {
        validate_hash("target_id_hashes", value)?;
        if !seen.insert(value) {
            return Err(DomainError::InvalidPayloadHash {
                message: "target_id_hashes must be unique".to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_shared_promotion_tiers(
    from_tier: MemoryVisibility,
    to_tier: MemoryVisibility,
) -> Result<(), DomainError> {
    validate_shared_tier("to_tier", to_tier)?;
    if to_tier <= from_tier {
        return Err(DomainError::UnsupportedVisibility {
            value: format!(
                "to_tier `{}` must be broader than from_tier `{}`",
                to_tier.as_str(),
                from_tier.as_str()
            ),
        });
    }
    Ok(())
}

fn validate_shared_tier(field: &'static str, tier: MemoryVisibility) -> Result<(), DomainError> {
    if crate::rebac::is_shared_tier(tier) {
        return Ok(());
    }
    Err(DomainError::UnsupportedVisibility {
        value: format!("{field} `{}` is not a shared tier", tier.as_str()),
    })
}

fn is_lowercase_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn decode_signature(signature: &Ed25519Signature) -> Result<Signature, DomainError> {
    let Some(hex) = signature.as_str().strip_prefix("ed25519:") else {
        return Err(DomainError::InvalidSignature);
    };
    let mut bytes = [0_u8; 64];
    for (idx, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        let hi = hex_nibble(pair[0]).ok_or(DomainError::InvalidSignature)?;
        let lo = hex_nibble(pair[1]).ok_or(DomainError::InvalidSignature)?;
        bytes[idx] = (hi << 4) | lo;
    }
    Ok(Signature::from_bytes(&bytes))
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}
