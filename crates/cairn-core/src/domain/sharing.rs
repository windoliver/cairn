//! Signed share links and promotion consent receipts (brief §4.2, §5.6, §12.a, §14).
//!
//! This module is pure domain logic: no I/O, no keychain lookup, no DB writes.

use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::domain::{
    DomainError, Ed25519Signature, Identity, MemoryVisibility, Rfc3339Timestamp, ScopeTuple,
};

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
