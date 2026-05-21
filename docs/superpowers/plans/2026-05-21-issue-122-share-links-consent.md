# Signed Share Links and Consent-Gated Promotion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the core domain substrate for signed share links and consent-gated promotion receipts from issue #122.

**Architecture:** Add a pure `cairn-core::domain::sharing` module that owns signed receipt/link payloads, canonical payload verification, apply-time promotion/share validation, revocation checks, and body-free consent journal helpers. Reuse existing `CanonicalRecordHash`, `Rfc3339Timestamp`, `Identity`, `RebacContext`, `ConsentPayload`, and `PolicyTraceEntry` types so adapters and WAL/store code can call one shared policy implementation.

**Tech Stack:** Rust 2024, `serde`, existing `ed25519-dalek`, existing `chrono` support on `Rfc3339Timestamp`, existing `ulid`, existing `cairn-core` policy trace and ReBAC modules.

---

## File Structure

- Create `crates/cairn-core/src/domain/sharing.rs`
  - Owns `PromotionConsentPayload`, `PromotionConsentReceipt`, `SignedShareLink`, `ShareLinkPayload`, `SharingRevocationState`, gate inputs, gate rejection type, sharing policy enums, signature verification, hash/nonce/ULID validation, and consent journal helper functions.
- Modify `crates/cairn-core/src/domain/mod.rs`
  - Adds `pub mod sharing;` and re-exports the public sharing types/functions used by tests and future adapters.
- Modify `crates/cairn-core/src/policy_trace/gate.rs`
  - Adds `PolicyGate::ConsentReceipt` and `PolicyGate::ShareLink`.
- Modify `crates/cairn-core/src/policy_trace/detail.rs`
  - Adds `PolicyDetail::Sharing` and renders `consent:promote:<reason>` / `share_link:grant:<reason>`.
- Modify `crates/cairn-core/tests/policy_trace_gate.rs`
  - Pins new gate wire strings.
- Modify `crates/cairn-core/tests/policy_trace_detail.rs`
  - Pins new body-free detail wire strings.
- Create `crates/cairn-core/tests/sharing_receipt.rs`
  - Covers receipt signature verification, promotion gate allow/deny, ReBAC denial, expiry, revocation, mismatch, and body-free journal helpers.
- Create `crates/cairn-core/tests/share_link.rs`
  - Covers signed share-link grant allow/deny, bearer links, expiry, revocation, mismatch, and body-free journal helpers.

---

### Task 1: Policy Trace Vocabulary

**Files:**
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Create: `crates/cairn-core/src/domain/sharing.rs`
- Modify: `crates/cairn-core/src/policy_trace/gate.rs`
- Modify: `crates/cairn-core/src/policy_trace/detail.rs`
- Test: `crates/cairn-core/tests/policy_trace_gate.rs`
- Test: `crates/cairn-core/tests/policy_trace_detail.rs`

- [ ] **Step 1: Write the failing policy trace tests**

Add these imports to `crates/cairn-core/tests/policy_trace_detail.rs`:

```rust
use cairn_core::domain::sharing::{
    SharingDecisionKind, SharingPolicyAction, SharingPolicySubject,
};
```

Add these tests to `crates/cairn-core/tests/policy_trace_detail.rs`:

```rust
#[test]
fn consent_receipt_detail_serializes_action_and_reason() {
    let d = PolicyDetail::Sharing {
        subject: SharingPolicySubject::ConsentReceipt,
        action: SharingPolicyAction::Promote,
        reason: SharingDecisionKind::TargetMismatch,
    };
    assert_eq!(d.to_wire_string(), "consent:promote:target_mismatch");
}

#[test]
fn share_link_detail_serializes_action_and_reason() {
    let d = PolicyDetail::Sharing {
        subject: SharingPolicySubject::ShareLink,
        action: SharingPolicyAction::Grant,
        reason: SharingDecisionKind::Revoked,
    };
    assert_eq!(d.to_wire_string(), "share_link:grant:revoked");
}
```

Add these cases to the `cases` array in `crates/cairn-core/tests/policy_trace_gate.rs`:

```rust
(PolicyGate::ConsentReceipt, "consent_receipt"),
(PolicyGate::ShareLink, "share_link"),
```

- [ ] **Step 2: Run the policy trace tests to verify they fail**

Run:

```bash
cargo test -p cairn-core --test policy_trace_detail --test policy_trace_gate
```

Expected: FAIL with errors mentioning missing `domain::sharing`, missing `PolicyDetail::Sharing`, or missing `PolicyGate::ConsentReceipt`.

- [ ] **Step 3: Add the minimal sharing enums and policy trace variants**

Create `crates/cairn-core/src/domain/sharing.rs` with this initial content:

```rust
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
```

In `crates/cairn-core/src/domain/mod.rs`, add the module and re-exports:

```rust
pub mod sharing;
```

```rust
pub use sharing::{SharingDecisionKind, SharingPolicyAction, SharingPolicySubject};
```

In `crates/cairn-core/src/policy_trace/gate.rs`, add enum variants:

```rust
/// Consent receipt gate for shared-tier promotion.
ConsentReceipt,
/// Signed share-link grant gate.
ShareLink,
```

Add match arms in `PolicyGate::as_str`:

```rust
Self::ConsentReceipt => "consent_receipt",
Self::ShareLink => "share_link",
```

In `crates/cairn-core/src/policy_trace/detail.rs`, add this import:

```rust
use crate::domain::sharing::{
    SharingDecisionKind, SharingPolicyAction, SharingPolicySubject,
};
```

Add this `PolicyDetail` variant after `Rebac`:

```rust
/// Signed sharing gate decision metadata.
Sharing {
    /// Consent receipt or share link.
    subject: SharingPolicySubject,
    /// Promote or grant.
    action: SharingPolicyAction,
    /// Body-free allow/deny reason.
    reason: SharingDecisionKind,
},
```

Add this `to_wire_string` match arm:

```rust
Self::Sharing {
    subject,
    action,
    reason,
} => format!(
    "{}:{}:{}",
    subject.as_detail_str(),
    action.as_str(),
    reason.as_str()
),
```

- [ ] **Step 4: Run the policy trace tests to verify they pass**

Run:

```bash
cargo test -p cairn-core --test policy_trace_detail --test policy_trace_gate
```

Expected: PASS.

- [ ] **Step 5: Commit Task 1**

Run:

```bash
git add crates/cairn-core/src/domain/mod.rs crates/cairn-core/src/domain/sharing.rs crates/cairn-core/src/policy_trace/gate.rs crates/cairn-core/src/policy_trace/detail.rs crates/cairn-core/tests/policy_trace_gate.rs crates/cairn-core/tests/policy_trace_detail.rs
git commit -m "feat(core): add sharing policy trace vocabulary"
```

---

### Task 2: Signed Receipt and Share-Link Shapes

**Files:**
- Modify: `crates/cairn-core/src/domain/sharing.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Test: `crates/cairn-core/tests/sharing_receipt.rs`
- Test: `crates/cairn-core/tests/share_link.rs`

- [ ] **Step 1: Write failing signature and shape tests**

Create `crates/cairn-core/tests/sharing_receipt.rs` with:

```rust
use cairn_core::domain::identity::keys::SigningKey;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::sharing::{PromotionConsentPayload, PromotionConsentReceipt};
use cairn_core::domain::{
    CanonicalRecordHash, Ed25519Signature, Identity, MemoryVisibility, Rfc3339Timestamp,
    ScopeTuple,
};

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[7_u8; 32])
}

fn signature_for<T: serde::Serialize>(payload: &T) -> Ed25519Signature {
    let bytes = cairn_core::domain::canonical::canonical_bytes(payload).expect("canonical bytes");
    let sig = signer().sign(&bytes);
    Ed25519Signature::parse(format!("ed25519:{}", hex(&sig.to_bytes()))).expect("signature")
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn scoped_record() -> cairn_core::domain::MemoryRecord {
    let mut record = sample_record();
    record.scope = receipt_scope();
    record.visibility = MemoryVisibility::Private;
    record
}

fn receipt_scope() -> ScopeTuple {
    ScopeTuple {
        tenant: Some("default".to_owned()),
        workspace: Some("vault-a".to_owned()),
        entity: Some("ingest".to_owned()),
        user: Some("hmn:tafeng".to_owned()),
        ..ScopeTuple::default()
    }
}

fn receipt_payload() -> PromotionConsentPayload {
    let record = scoped_record();
    PromotionConsentPayload {
        operation_id: "01HQZX9F5N0000000000000002".to_owned(),
        nonce: "AAAAAAAAAAAAAAAAAAAAAA==".to_owned(),
        chain_parents: vec!["01HQZX9F5N0000000000000003".to_owned()],
        target_hash: CanonicalRecordHash::compute(&record)
            .expect("record hash")
            .as_str()
            .to_owned(),
        target_id_hash: format!("hash:{}", "b".repeat(32)),
        from_tier: MemoryVisibility::Private,
        to_tier: MemoryVisibility::Team,
        scope: receipt_scope(),
        human_identity: Identity::parse("hmn:tafeng").expect("human"),
        issued_at: Rfc3339Timestamp::parse("2026-05-21T12:00:00Z").expect("issued"),
        expires_at: Rfc3339Timestamp::parse("2026-05-22T12:00:00Z").expect("expires"),
        key_version: 1,
    }
}

fn signed_receipt() -> PromotionConsentReceipt {
    let payload = receipt_payload();
    let signature = signature_for(&payload);
    PromotionConsentReceipt {
        receipt_id: "rcpt-01HQZX9F5N0000000000000002".to_owned(),
        payload,
        signature,
    }
}

#[test]
fn promotion_receipt_signature_verifies() {
    let receipt = signed_receipt();
    receipt
        .verify_signature(&signer().verifying_key())
        .expect("signature verifies");
}

#[test]
fn promotion_receipt_rejects_tampered_target_hash() {
    let mut receipt = signed_receipt();
    receipt.payload.target_hash = format!("sha256:{}", "0".repeat(64));
    let err = receipt
        .verify_signature(&signer().verifying_key())
        .expect_err("tampering breaks signature");
    assert!(matches!(err, cairn_core::domain::DomainError::InvalidSignature));
}

#[test]
fn promotion_receipt_shape_rejects_raw_target_id_hash() {
    let mut receipt = signed_receipt();
    receipt.payload.target_id_hash = "01HQZX9F5N0000000000000000".to_owned();
    let err = receipt.validate_shape().expect_err("raw id rejected");
    assert!(matches!(
        err,
        cairn_core::domain::DomainError::InvalidPayloadHash { .. }
            | cairn_core::domain::DomainError::ScopeDenied { .. }
    ));
}
```

Create `crates/cairn-core/tests/share_link.rs` with:

```rust
use cairn_core::domain::identity::keys::SigningKey;
use cairn_core::domain::sharing::{ShareLinkPayload, SignedShareLink};
use cairn_core::domain::{
    Ed25519Signature, Identity, MemoryVisibility, Rfc3339Timestamp, ScopeTuple,
};

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[9_u8; 32])
}

fn signature_for<T: serde::Serialize>(payload: &T) -> Ed25519Signature {
    let bytes = cairn_core::domain::canonical::canonical_bytes(payload).expect("canonical bytes");
    let sig = signer().sign(&bytes);
    Ed25519Signature::parse(format!("ed25519:{}", hex(&sig.to_bytes()))).expect("signature")
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn scope() -> ScopeTuple {
    ScopeTuple {
        tenant: Some("default".to_owned()),
        workspace: Some("vault-a".to_owned()),
        entity: Some("session".to_owned()),
        ..ScopeTuple::default()
    }
}

fn link_payload() -> ShareLinkPayload {
    ShareLinkPayload {
        operation_id: "01HQZX9F5N0000000000000004".to_owned(),
        nonce: "AAAAAAAAAAAAAAAAAAAAAA==".to_owned(),
        target_hash: format!("sha256:{}", "c".repeat(64)),
        target_id_hashes: vec![format!("hash:{}", "d".repeat(32))],
        scope: scope(),
        grant_tier: MemoryVisibility::Team,
        grantee: Some(Identity::parse("agt:cairn-cli:default:reader:v1").expect("agent")),
        issuer: Identity::parse("hmn:tafeng").expect("human"),
        issued_at: Rfc3339Timestamp::parse("2026-05-21T12:00:00Z").expect("issued"),
        expires_at: Rfc3339Timestamp::parse("2026-05-22T12:00:00Z").expect("expires"),
        key_version: 1,
    }
}

fn signed_link() -> SignedShareLink {
    let payload = link_payload();
    let signature = signature_for(&payload);
    SignedShareLink {
        link_id: "share-01HQZX9F5N0000000000000004".to_owned(),
        payload,
        signature,
    }
}

#[test]
fn share_link_signature_verifies() {
    let link = signed_link();
    link.verify_signature(&signer().verifying_key())
        .expect("signature verifies");
}

#[test]
fn share_link_rejects_tampered_scope() {
    let mut link = signed_link();
    link.payload.scope.entity = Some("other".to_owned());
    let err = link
        .verify_signature(&signer().verifying_key())
        .expect_err("tampering breaks signature");
    assert!(matches!(err, cairn_core::domain::DomainError::InvalidSignature));
}

#[test]
fn share_link_shape_rejects_empty_target_id_hashes() {
    let mut link = signed_link();
    link.payload.target_id_hashes.clear();
    let err = link.validate_shape().expect_err("empty target set rejected");
    assert!(matches!(err, cairn_core::domain::DomainError::InvalidPayloadHash { .. }));
}
```

- [ ] **Step 2: Run sharing shape tests to verify they fail**

Run:

```bash
cargo test -p cairn-core --test sharing_receipt --test share_link
```

Expected: FAIL with unresolved imports for `PromotionConsentPayload`, `PromotionConsentReceipt`, `ShareLinkPayload`, `SignedShareLink`, or missing methods.

- [ ] **Step 3: Implement receipt/link shapes, shape validation, and signature verification**

Replace `crates/cairn-core/src/domain/sharing.rs` with the Task 1 enums plus these imports and types:

```rust
use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::domain::{
    DomainError, Ed25519Signature, Identity, MemoryVisibility, Rfc3339Timestamp, ScopeTuple,
};
```

Add these structs below the sharing policy enums:

```rust
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SharingRevocationState {
    /// Receipt ids that can no longer authorize promotion.
    pub revoked_receipt_ids: BTreeSet<String>,
    /// Share link ids that can no longer authorize grants.
    pub revoked_share_link_ids: BTreeSet<String>,
    /// Whether the current signer key is revoked.
    pub signer_key_revoked: bool,
}
```

Add these impls and helper functions:

```rust
impl PromotionConsentReceipt {
    /// Validate body-free shape before or after signature verification.
    pub fn validate_shape(&self) -> Result<(), DomainError> {
        validate_id("receipt_id", &self.receipt_id)?;
        validate_ulid("operation_id", &self.payload.operation_id)?;
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
        validate_nonce(&self.payload.nonce)?;
        validate_target_hash(&self.payload.target_hash)?;
        if self.payload.target_id_hashes.is_empty() {
            return Err(DomainError::InvalidPayloadHash {
                message: "target_id_hashes must not be empty".to_owned(),
            });
        }
        for h in &self.payload.target_id_hashes {
            validate_hash("target_id_hashes", h)?;
        }
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
        message: format!("{field} must be sha256:<64 lowercase hex> or hash:<32..=128 lowercase hex>"),
    })
}

fn validate_shared_promotion_tiers(
    from_tier: MemoryVisibility,
    to_tier: MemoryVisibility,
) -> Result<(), DomainError> {
    validate_shared_tier("to_tier", to_tier)?;
    if to_tier <= from_tier {
        return Err(DomainError::UnsupportedVisibility {
            value: format!("to_tier `{}` must be broader than from_tier `{}`", to_tier.as_str(), from_tier.as_str()),
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
    value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
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
```

In `crates/cairn-core/src/domain/mod.rs`, extend the sharing re-export:

```rust
pub use sharing::{
    PromotionConsentPayload, PromotionConsentReceipt, ShareLinkPayload, SharingDecisionKind,
    SharingPolicyAction, SharingPolicySubject, SharingRevocationState, SignedShareLink,
};
```

- [ ] **Step 4: Run shape tests to verify they pass**

Run:

```bash
cargo test -p cairn-core --test sharing_receipt --test share_link
```

Expected: PASS.

- [ ] **Step 5: Commit Task 2**

Run:

```bash
git add crates/cairn-core/src/domain/sharing.rs crates/cairn-core/src/domain/mod.rs crates/cairn-core/tests/sharing_receipt.rs crates/cairn-core/tests/share_link.rs
git commit -m "feat(core): add signed sharing payloads"
```

---

### Task 3: Promotion Apply-Time Gate

**Files:**
- Modify: `crates/cairn-core/src/domain/sharing.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Test: `crates/cairn-core/tests/sharing_receipt.rs`

- [ ] **Step 1: Add failing promotion gate tests**

Append these imports to `crates/cairn-core/tests/sharing_receipt.rs`:

```rust
use cairn_core::domain::sharing::{
    PromotionGateInput, SharingDecisionKind, SharingRevocationState, verify_promotion_gate,
};
use cairn_core::rebac::{RebacAction, RebacContext, RebacRelation};
```

Append these helper functions:

```rust
fn rebac_for_team_write() -> RebacContext {
    let principal = Identity::parse("hmn:tafeng").expect("human");
    let scope = receipt_scope();
    RebacContext::new(
        principal.clone(),
        vec![RebacRelation::new(
            principal,
            RebacAction::Write,
            scope,
            MemoryVisibility::Team,
        )],
    )
}

fn promotion_input<'a>(
    record: &'a cairn_core::domain::MemoryRecord,
    receipt: &'a PromotionConsentReceipt,
    now: &'a Rfc3339Timestamp,
    revocation: &'a SharingRevocationState,
    rebac: &'a RebacContext,
) -> PromotionGateInput<'a> {
    PromotionGateInput {
        record,
        from_tier: MemoryVisibility::Private,
        to_tier: MemoryVisibility::Team,
        receipt,
        now,
        operation_id: "01HQZX9F5N0000000000000002",
        signer_key: &signer().verifying_key(),
        revocation,
        rebac,
    }
}
```

Append these tests:

```rust
#[test]
fn promotion_gate_allows_valid_receipt_and_rebac_relation() {
    let record = scoped_record();
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    let trace = verify_promotion_gate(promotion_input(
        &record,
        &receipt,
        &now,
        &revocation,
        &rebac,
    ))
    .expect("promotion allowed");

    assert_eq!(trace.detail.to_wire_string(), "consent:promote:allowed");
}

#[test]
fn promotion_gate_rejects_expired_receipt_at_apply_time() {
    let record = scoped_record();
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-23T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    let err = verify_promotion_gate(promotion_input(
        &record,
        &receipt,
        &now,
        &revocation,
        &rebac,
    ))
    .expect_err("expired receipt denied");

    assert_eq!(err.trace.detail.to_wire_string(), "consent:promote:expired");
}

#[test]
fn promotion_gate_rejects_target_hash_mismatch() {
    let mut record = scoped_record();
    record.body.push_str(" changed after receipt signing");
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = rebac_for_team_write();

    let err = verify_promotion_gate(promotion_input(
        &record,
        &receipt,
        &now,
        &revocation,
        &rebac,
    ))
    .expect_err("target mismatch denied");

    assert_eq!(
        err.trace.detail.to_wire_string(),
        "consent:promote:target_mismatch"
    );
}

#[test]
fn promotion_gate_rejects_revoked_receipt() {
    let record = scoped_record();
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let mut revocation = SharingRevocationState::default();
    revocation
        .revoked_receipt_ids
        .insert("rcpt-01HQZX9F5N0000000000000002".to_owned());
    let rebac = rebac_for_team_write();

    let err = verify_promotion_gate(promotion_input(
        &record,
        &receipt,
        &now,
        &revocation,
        &rebac,
    ))
    .expect_err("revoked receipt denied");

    assert_eq!(err.trace.detail.to_wire_string(), "consent:promote:revoked");
}

#[test]
fn promotion_gate_rejects_missing_rebac_write_relation() {
    let record = scoped_record();
    let receipt = signed_receipt();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let rebac = RebacContext::for_principal(Identity::parse("hmn:tafeng").expect("human"));

    let err = verify_promotion_gate(promotion_input(
        &record,
        &receipt,
        &now,
        &revocation,
        &rebac,
    ))
    .expect_err("missing ReBAC relation denied");

    assert_eq!(
        err.trace.detail.to_wire_string(),
        "consent:promote:no_rebac_relation"
    );
}
```

- [ ] **Step 2: Run promotion gate tests to verify they fail**

Run:

```bash
cargo test -p cairn-core --test sharing_receipt
```

Expected: FAIL with unresolved imports for `PromotionGateInput` and `verify_promotion_gate`.

- [ ] **Step 3: Implement promotion gate**

Add these imports to `crates/cairn-core/src/domain/sharing.rs`:

```rust
use crate::domain::{CanonicalRecordHash, MemoryRecord};
use crate::policy_trace::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry};
use crate::rebac::{RebacAction, RebacContext};
```

Add these types and functions:

```rust
/// Inputs for the shared-tier promotion gate.
pub struct PromotionGateInput<'a> {
    /// Record being promoted, still at `from_tier`.
    pub record: &'a MemoryRecord,
    /// Current tier.
    pub from_tier: MemoryVisibility,
    /// Requested shared tier.
    pub to_tier: MemoryVisibility,
    /// Signed receipt presented at apply time.
    pub receipt: &'a PromotionConsentReceipt,
    /// Apply-time timestamp.
    pub now: &'a Rfc3339Timestamp,
    /// WAL operation id being applied.
    pub operation_id: &'a str,
    /// Verifying key for `receipt.payload.human_identity`.
    pub signer_key: &'a VerifyingKey,
    /// Revocation state supplied by store/identity layer.
    pub revocation: &'a SharingRevocationState,
    /// Existing ReBAC context from #121.
    pub rebac: &'a RebacContext,
}

/// Denial with both typed error and body-free trace.
#[derive(Debug)]
pub struct SharingGateRejection {
    /// Typed domain error.
    pub error: DomainError,
    /// Body-free trace entry explaining the denial.
    pub trace: PolicyTraceEntry,
}

/// Verify a promotion consent receipt at WAL apply time.
pub fn verify_promotion_gate(
    input: PromotionGateInput<'_>,
) -> Result<PolicyTraceEntry, SharingGateRejection> {
    if let Err(error) = input.receipt.validate_shape() {
        return Err(reject_promotion(SharingDecisionKind::InvalidShape, error));
    }
    if input.receipt.verify_signature(input.signer_key).is_err() {
        return Err(reject_promotion(
            SharingDecisionKind::BadSignature,
            DomainError::InvalidSignature,
        ));
    }
    if input.receipt.payload.human_identity.kind() != crate::domain::IdentityKind::Human {
        return Err(reject_promotion(
            SharingDecisionKind::NotHuman,
            DomainError::Unauthorized {
                message: "promotion receipt signer must be human".to_owned(),
            },
        ));
    }
    let computed = CanonicalRecordHash::compute(input.record).map_err(|error| {
        reject_promotion(SharingDecisionKind::TargetMismatch, error)
    })?;
    if computed.as_str() != input.receipt.payload.target_hash {
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
                message: "promotion receipt operation_id does not match WAL operation".to_owned(),
            },
        ));
    }
    if input.receipt.payload.from_tier != input.from_tier
        || input.receipt.payload.to_tier != input.to_tier
    {
        return Err(reject_promotion(
            SharingDecisionKind::TierMismatch,
            DomainError::UnsupportedVisibility {
                value: "promotion receipt tiers do not match requested promotion".to_owned(),
            },
        ));
    }
    if input.now.cmp_chronological(&input.receipt.payload.expires_at)
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
                message: "promotion receipt or signer key is revoked".to_owned(),
            },
        ));
    }
    let decision = input
        .rebac
        .evaluate(RebacAction::Write, &input.receipt.payload.scope, input.to_tier);
    if !decision.allowed() {
        return Err(reject_promotion(
            SharingDecisionKind::NoRebacRelation,
            DomainError::ScopeDenied {
                message: "rebac denied shared-tier promotion write".to_owned(),
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
```

In `crates/cairn-core/src/domain/mod.rs`, extend sharing re-exports:

```rust
pub use sharing::{
    PromotionConsentPayload, PromotionConsentReceipt, PromotionGateInput, ShareLinkPayload,
    SharingDecisionKind, SharingGateRejection, SharingPolicyAction, SharingPolicySubject,
    SharingRevocationState, SignedShareLink, verify_promotion_gate,
};
```

- [ ] **Step 4: Run promotion gate tests to verify they pass**

Run:

```bash
cargo test -p cairn-core --test sharing_receipt
```

Expected: PASS.

- [ ] **Step 5: Commit Task 3**

Run:

```bash
git add crates/cairn-core/src/domain/sharing.rs crates/cairn-core/src/domain/mod.rs crates/cairn-core/tests/sharing_receipt.rs
git commit -m "feat(core): validate promotion consent receipts"
```

---

### Task 4: Share-Link Grant Gate

**Files:**
- Modify: `crates/cairn-core/src/domain/sharing.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Test: `crates/cairn-core/tests/share_link.rs`

- [ ] **Step 1: Add failing share-link gate tests**

Append these imports to `crates/cairn-core/tests/share_link.rs`:

```rust
use cairn_core::domain::sharing::{
    ShareLinkGateInput, SharingRevocationState, verify_share_link_grant,
};
```

Append this helper:

```rust
fn share_link_input<'a>(
    link: &'a SignedShareLink,
    now: &'a Rfc3339Timestamp,
    revocation: &'a SharingRevocationState,
) -> ShareLinkGateInput<'a> {
    ShareLinkGateInput {
        link,
        now,
        expected_target_hash: &link.payload.target_hash,
        signer_key: &signer().verifying_key(),
        revocation,
        max_tier: MemoryVisibility::Team,
    }
}
```

Append these tests:

```rust
#[test]
fn share_link_gate_allows_valid_link() {
    let link = signed_link();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();

    let trace = verify_share_link_grant(share_link_input(&link, &now, &revocation))
        .expect("grant allowed");

    assert_eq!(trace.detail.to_wire_string(), "share_link:grant:allowed");
}

#[test]
fn share_link_gate_allows_bearer_link_with_expiry_and_revocation_checks() {
    let mut link = signed_link();
    link.payload.grantee = None;
    link.signature = signature_for(&link.payload);
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();

    let trace = verify_share_link_grant(share_link_input(&link, &now, &revocation))
        .expect("bearer grant allowed");

    assert_eq!(trace.detail.to_wire_string(), "share_link:grant:allowed");
}

#[test]
fn share_link_gate_rejects_expired_link() {
    let link = signed_link();
    let now = Rfc3339Timestamp::parse("2026-05-23T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();

    let err = verify_share_link_grant(share_link_input(&link, &now, &revocation))
        .expect_err("expired link denied");

    assert_eq!(err.trace.detail.to_wire_string(), "share_link:grant:expired");
}

#[test]
fn share_link_gate_rejects_revoked_link() {
    let link = signed_link();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let mut revocation = SharingRevocationState::default();
    revocation
        .revoked_share_link_ids
        .insert("share-01HQZX9F5N0000000000000004".to_owned());

    let err = verify_share_link_grant(share_link_input(&link, &now, &revocation))
        .expect_err("revoked link denied");

    assert_eq!(err.trace.detail.to_wire_string(), "share_link:grant:revoked");
}

#[test]
fn share_link_gate_rejects_target_hash_mismatch() {
    let link = signed_link();
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();
    let input = ShareLinkGateInput {
        expected_target_hash: &format!("sha256:{}", "0".repeat(64)),
        ..share_link_input(&link, &now, &revocation)
    };

    let err = verify_share_link_grant(input).expect_err("target mismatch denied");

    assert_eq!(
        err.trace.detail.to_wire_string(),
        "share_link:grant:target_mismatch"
    );
}

#[test]
fn share_link_gate_rejects_tier_above_authorized_max() {
    let mut link = signed_link();
    link.payload.grant_tier = MemoryVisibility::Org;
    link.signature = signature_for(&link.payload);
    let now = Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("now");
    let revocation = SharingRevocationState::default();

    let err = verify_share_link_grant(share_link_input(&link, &now, &revocation))
        .expect_err("tier above max denied");

    assert_eq!(
        err.trace.detail.to_wire_string(),
        "share_link:grant:tier_mismatch"
    );
}
```

- [ ] **Step 2: Run share-link gate tests to verify they fail**

Run:

```bash
cargo test -p cairn-core --test share_link
```

Expected: FAIL with unresolved imports for `ShareLinkGateInput` and `verify_share_link_grant`.

- [ ] **Step 3: Implement share-link gate**

Add these types and functions to `crates/cairn-core/src/domain/sharing.rs`:

```rust
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

/// Verify a signed share link before honoring the grant.
pub fn verify_share_link_grant(
    input: ShareLinkGateInput<'_>,
) -> Result<PolicyTraceEntry, SharingGateRejection> {
    if let Err(error) = input.link.validate_shape() {
        return Err(reject_share_link(SharingDecisionKind::InvalidShape, error));
    }
    if input.link.verify_signature(input.signer_key).is_err() {
        return Err(reject_share_link(
            SharingDecisionKind::BadSignature,
            DomainError::InvalidSignature,
        ));
    }
    if input.link.payload.issuer.kind() != crate::domain::IdentityKind::Human {
        return Err(reject_share_link(
            SharingDecisionKind::NotHuman,
            DomainError::Unauthorized {
                message: "share link issuer must be human".to_owned(),
            },
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
                value: "share link grant_tier exceeds issuer authorization".to_owned(),
            },
        ));
    }
    if input.now.cmp_chronological(&input.link.payload.expires_at)
        != std::cmp::Ordering::Less
    {
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
                message: "share link or signer key is revoked".to_owned(),
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
```

In `crates/cairn-core/src/domain/mod.rs`, extend sharing re-exports:

```rust
pub use sharing::{
    PromotionConsentPayload, PromotionConsentReceipt, PromotionGateInput, ShareLinkGateInput,
    ShareLinkPayload, SharingDecisionKind, SharingGateRejection, SharingPolicyAction,
    SharingPolicySubject, SharingRevocationState, SignedShareLink, verify_promotion_gate,
    verify_share_link_grant,
};
```

- [ ] **Step 4: Run share-link tests to verify they pass**

Run:

```bash
cargo test -p cairn-core --test share_link
```

Expected: PASS.

- [ ] **Step 5: Commit Task 4**

Run:

```bash
git add crates/cairn-core/src/domain/sharing.rs crates/cairn-core/src/domain/mod.rs crates/cairn-core/tests/share_link.rs
git commit -m "feat(core): validate signed share links"
```

---

### Task 5: Body-Free Consent Journal Helpers

**Files:**
- Modify: `crates/cairn-core/src/domain/sharing.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Test: `crates/cairn-core/tests/sharing_receipt.rs`
- Test: `crates/cairn-core/tests/share_link.rs`

- [ ] **Step 1: Add failing consent journal helper tests**

Append these imports to `crates/cairn-core/tests/sharing_receipt.rs`:

```rust
use cairn_core::domain::{ConsentEvent, ConsentKind};
```

Append this test:

```rust
#[test]
fn promotion_receipt_consent_event_is_body_free_and_valid() {
    let receipt = signed_receipt();
    let payload = receipt.promote_consent_payload();
    let event = ConsentEvent {
        consent_id: "01HQZX9F5N0000000000000005".to_owned(),
        kind: ConsentKind::PromoteReceipt,
        actor: Identity::parse("hmn:tafeng").expect("human"),
        subject: receipt.payload.target_id_hash.clone(),
        scope: "tenant=default,workspace=vault-a,entity=ingest,user=hmn:tafeng".to_owned(),
        op_id: Some(receipt.payload.operation_id.clone()),
        sensor_id: None,
        payload,
        decided_at: Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("decided"),
        expires_at: Some(receipt.payload.expires_at.clone()),
    };

    event.validate().expect("journal event valid");
    let value = serde_json::to_value(&event).expect("json");
    let serialized = value.to_string();
    for banned in [
        "\"body\"",
        "\"text\"",
        "\"content\"",
        "\"raw\"",
        "\"snippet\"",
        "\"command\"",
        "\"url\"",
        "\"title\"",
        "\"file_path\"",
        "\"input\"",
        "\"message\"",
    ] {
        assert!(!serialized.contains(banned), "banned field {banned}");
    }
}
```

Append these imports to `crates/cairn-core/tests/share_link.rs`:

```rust
use cairn_core::domain::{ConsentEvent, ConsentKind};
use cairn_core::domain::sharing::ShareLinkJournalDecision;
```

Append this test:

```rust
#[test]
fn share_link_grant_consent_event_is_body_free_and_valid() {
    let link = signed_link();
    let (subject, payload) = link.decision_payload(ShareLinkJournalDecision::Grant);
    let event = ConsentEvent {
        consent_id: "01HQZX9F5N0000000000000006".to_owned(),
        kind: ConsentKind::Grant,
        actor: Identity::parse("hmn:tafeng").expect("human"),
        subject,
        scope: "tenant=default,workspace=vault-a,entity=session".to_owned(),
        op_id: Some(link.payload.operation_id.clone()),
        sensor_id: None,
        payload,
        decided_at: Rfc3339Timestamp::parse("2026-05-21T12:30:00Z").expect("decided"),
        expires_at: Some(link.payload.expires_at.clone()),
    };

    event.validate().expect("journal event valid");
    let serialized = serde_json::to_value(&event).expect("json").to_string();
    for banned in [
        "\"body\"",
        "\"text\"",
        "\"content\"",
        "\"raw\"",
        "\"snippet\"",
        "\"command\"",
        "\"url\"",
        "\"title\"",
        "\"file_path\"",
        "\"input\"",
        "\"message\"",
    ] {
        assert!(!serialized.contains(banned), "banned field {banned}");
    }
}
```

- [ ] **Step 2: Run helper tests to verify they fail**

Run:

```bash
cargo test -p cairn-core --test sharing_receipt --test share_link
```

Expected: FAIL with missing `promote_consent_payload`, `ShareLinkJournalDecision`, or `decision_payload`.

- [ ] **Step 3: Implement body-free helper functions**

Add this import to `crates/cairn-core/src/domain/sharing.rs`:

```rust
use crate::domain::ConsentPayload;
```

Add this enum and impl block:

```rust
/// Consent journal decision for a share link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareLinkJournalDecision {
    /// Link grant.
    Grant,
    /// Link revoke.
    Revoke,
}

impl ShareLinkJournalDecision {
    /// Body-free policy code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Grant => "grant",
            Self::Revoke => "revoke",
        }
    }
}
```

Add this method to `impl PromotionConsentReceipt`:

```rust
/// Build the body-free `ConsentPayload` for a promotion journal row.
#[must_use]
pub fn promote_consent_payload(&self) -> ConsentPayload {
    ConsentPayload::PromoteReceipt {
        target_id_hash: self.payload.target_id_hash.clone(),
        from_tier: self.payload.from_tier,
        to_tier: self.payload.to_tier,
        receipt_id: self.receipt_id.clone(),
    }
}
```

Add this method to `impl SignedShareLink`:

```rust
/// Build the body-free subject and `ConsentPayload` for a share-link journal row.
#[must_use]
pub fn decision_payload(&self, decision: ShareLinkJournalDecision) -> (String, ConsentPayload) {
    (
        format!("share_link:{}", self.link_id),
        ConsentPayload::Decision {
            subject_code: format!("share_link:{}", self.link_id),
            policy_code: Some(decision.as_str().to_owned()),
        },
    )
}
```

In `crates/cairn-core/src/domain/mod.rs`, extend sharing re-exports:

```rust
pub use sharing::{
    PromotionConsentPayload, PromotionConsentReceipt, PromotionGateInput, ShareLinkGateInput,
    ShareLinkJournalDecision, ShareLinkPayload, SharingDecisionKind, SharingGateRejection,
    SharingPolicyAction, SharingPolicySubject, SharingRevocationState, SignedShareLink,
    verify_promotion_gate, verify_share_link_grant,
};
```

- [ ] **Step 4: Run helper tests to verify they pass**

Run:

```bash
cargo test -p cairn-core --test sharing_receipt --test share_link
```

Expected: PASS.

- [ ] **Step 5: Commit Task 5**

Run:

```bash
git add crates/cairn-core/src/domain/sharing.rs crates/cairn-core/src/domain/mod.rs crates/cairn-core/tests/sharing_receipt.rs crates/cairn-core/tests/share_link.rs
git commit -m "feat(core): add body-free sharing journal helpers"
```

---

### Task 6: Full Verification and Cleanup

**Files:**
- Modify only files already touched if compiler, clippy, or formatting requires local cleanup.

- [ ] **Step 1: Run focused sharing tests**

Run:

```bash
cargo test -p cairn-core --test sharing_receipt --test share_link
```

Expected: PASS.

- [ ] **Step 2: Run policy trace regression tests**

Run:

```bash
cargo test -p cairn-core --test policy_trace_detail --test policy_trace_gate --test rebac_policy
```

Expected: PASS.

- [ ] **Step 3: Run full core tests**

Run:

```bash
cargo test -p cairn-core
```

Expected: PASS.

- [ ] **Step 4: Run workspace formatting and boundary checks**

Run:

```bash
cargo fmt --check
scripts/check-core-boundary.sh
```

Expected: both commands exit 0.

- [ ] **Step 5: Run workspace tests if local time budget allows**

Run:

```bash
cargo test --workspace
```

Expected: PASS. If this is too slow or fails in unrelated crates, capture the failing command output and keep the focused `cairn-core` evidence in the final report.

- [ ] **Step 6: Commit cleanup if there were changes**

Run only if Step 1 through Step 5 produced code changes:

```bash
git add crates/cairn-core/src/domain/sharing.rs crates/cairn-core/src/domain/mod.rs crates/cairn-core/src/policy_trace/gate.rs crates/cairn-core/src/policy_trace/detail.rs crates/cairn-core/tests/policy_trace_gate.rs crates/cairn-core/tests/policy_trace_detail.rs crates/cairn-core/tests/sharing_receipt.rs crates/cairn-core/tests/share_link.rs
git commit -m "test(core): verify signed sharing gates"
```

---

## Self-Review Checklist

- Spec coverage:
  - Signed promotion receipts: Tasks 2 and 3.
  - Signed share links: Tasks 2 and 4.
  - Target hashes, scopes, nonces, expirations, and human identities: Tasks 2 through 4.
  - Expired and mismatched receipt rejection at apply time: Task 3.
  - Revocation tests: Tasks 3 and 4.
  - Body-free consent journal metadata: Task 5.
  - ReBAC dependency from #121: Task 3.
- Red-flag scan:
  - The plan contains no deferred implementation notes and no unspecified validation steps.
- Type consistency:
  - `PromotionConsentPayload`, `PromotionConsentReceipt`, `ShareLinkPayload`, `SignedShareLink`, `SharingRevocationState`, `PromotionGateInput`, `ShareLinkGateInput`, and `SharingGateRejection` are introduced before later tasks use them.
  - `SharingDecisionKind` tokens match the policy trace tests.
  - `ShareLinkJournalDecision` is introduced in Task 5 before tests use it.
