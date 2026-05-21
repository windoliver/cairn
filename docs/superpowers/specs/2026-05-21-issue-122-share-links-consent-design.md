# Signed Share Links and Consent-Gated Promotion Design - Issue #122

**Date:** 2026-05-21
**Issue:** [#122 - Implement signed share links and consent-gated promotions](https://github.com/windoliver/cairn/issues/122)
**Brief sections:** section 4.2 Identity; section 5.6 WAL promote; section 11.3 Constraint gates; section 12.a Distribution Model; section 14 Privacy and Consent
**Dependency:** #121, merged by PR #397, provides the ReBAC shared-tier decision surface this design builds on.
**Status:** Design approved; written-spec review pending

---

## 1. Scope

Implement the core validation substrate for signed share links and consent-gated promotion into shared tiers. A record may move from local tiers into `project`, `team`, `org`, or `public` only when the apply-time gate sees a fresh, signed, non-revoked, body-free consent receipt that matches the exact target, scope, tier, operation, nonce, expiry, and human identity.

This PR adds:

- A typed promotion consent receipt model.
- A typed signed share link model for time-bound grants.
- Canonical hashing and Ed25519 verification helpers for those payloads.
- Apply-time validation that rejects expired, mismatched, malformed, non-human, or revoked receipts.
- Body-free policy trace details for consent and share-link decisions.
- Tests for valid signatures, tampering, expiry, mismatch, revocation, and body-free persistence.

Out of scope: org-wide propagation policy UI, hub transport, public indexing, and a full `promote` CLI command. This design focuses on the core rules and pure data needed by the WAL/store workflow to safely call `policy.verify_receipt`.

---

## 2. Architecture

The implementation stays inside the existing boundaries.

| Layer | Location | Responsibility |
|---|---|---|
| Receipt and link domain types | `cairn-core::domain::sharing` | Pure serde data, canonical payload bytes, validation, no I/O. |
| Signature verification | `cairn-core::domain::sharing` using existing `ed25519-dalek` dependency | Verify detached Ed25519 signatures over canonical bytes. |
| Apply-time gate | `cairn-core::domain::sharing::PromotionGate` | Check receipt/link freshness, hash binding, scope binding, human signer, tier, nonce, revocation, and ReBAC relation. |
| Policy trace vocabulary | `cairn-core::policy_trace` | Body-free gate/detail strings for allow and deny reasons. |
| Consent journal payload | `cairn-core::domain::consent` | Store only receipt ids, target id hashes, tiers, and symbolic codes. |

`cairn-core` remains pure: no keychain lookup, no wall-clock source, no DB writes. Callers pass the current timestamp, verifying key, revocation state, and existing ReBAC context into the gate. The SQLite/WAL layer remains responsible for atomic `primary.update_tier`, `rebac.add_relation`, and `consent_journal.append(promote)` as described in section 5.6.

---

## 3. Promotion Consent Receipt

The receipt is a detached-signature envelope. Field order is stable and used for canonical JSON. The signature covers every payload field except the signature itself.

```rust
pub struct PromotionConsentReceipt {
    pub receipt_id: String,
    pub payload: PromotionConsentPayload,
    pub signature: Ed25519Signature,
}

pub struct PromotionConsentPayload {
    pub operation_id: String,
    pub nonce: String,
    pub chain_parents: Vec<String>,
    pub target_hash: String,
    pub target_id_hash: String,
    pub from_tier: MemoryVisibility,
    pub to_tier: MemoryVisibility,
    pub scope: ScopeTuple,
    pub human_identity: Identity,
    pub issued_at: Rfc3339Timestamp,
    pub expires_at: Rfc3339Timestamp,
    pub key_version: u32,
}
```

Validation rules:

- `human_identity` must be a human identity (`hmn:` on `origin/main`).
- `to_tier` must be broader than `from_tier`.
- `to_tier` must be one of `project`, `team`, `org`, or `public`.
- `target_hash` must be `sha256:<64 lowercase hex>` and must match the canonical hash of the record being promoted.
- `target_id_hash` must match the consent journal hash grammar: either `sha256:<64 lowercase hex>` or `hash:<32..=128 lowercase hex>`. Raw record ids are rejected.
- `nonce` must be a 16-byte base64 nonce using the same shape as signed intents.
- `operation_id` must be ULID-shaped and must match the WAL operation being applied.
- `expires_at` must be after `issued_at`, and the current apply-time instant must be before `expires_at`.
- `chain_parents` must contain no more than 64 unique ULID-shaped ids.
- The receipt must not contain body, text, URL, title, command, raw content, or other banned content-bearing fields.

The gate re-checks all rules at apply time. A receipt that was valid when proposed but expired before WAL apply is rejected.

---

## 4. Signed Share Links

A share link is a grant token for read access to a record set, session, or scope. It is tamper-evident and revocable. It does not itself promote the record; it authorizes a bounded share/grant relation that the propagation or retrieve path can evaluate.

```rust
pub struct SignedShareLink {
    pub link_id: String,
    pub payload: ShareLinkPayload,
    pub signature: Ed25519Signature,
}

pub struct ShareLinkPayload {
    pub operation_id: String,
    pub nonce: String,
    pub target_hash: String,
    pub target_id_hashes: Vec<String>,
    pub scope: ScopeTuple,
    pub grant_tier: MemoryVisibility,
    pub grantee: Option<Identity>,
    pub issuer: Identity,
    pub issued_at: Rfc3339Timestamp,
    pub expires_at: Rfc3339Timestamp,
    pub key_version: u32,
}
```

Validation rules:

- `issuer` must be the human who granted sharing.
- `grant_tier` must be shared (`project`, `team`, `org`, `public`) and must not exceed the issuer's authorized tier.
- `target_hash` binds the link to the grant payload or record-set manifest, not to display text.
- `target_id_hashes` must each match `sha256:<64 lowercase hex>` or `hash:<32..=128 lowercase hex>`. The link never stores raw ids or body bytes.
- `grantee`, when present, must be a parseable human or agent identity. An absent grantee means bearer-style access, still bounded by expiry and revocation.
- Revocation is checked by `link_id` and by issuer key revocation state before any relation is honored.

Share links write `ConsentKind::Grant` / `ConsentKind::Revoke` journal entries with `subject_code = "share_link:<link_id>"`. The payload remains symbolic and body-free.

---

## 5. Apply-Time Promotion Gate

The promotion gate takes explicit inputs:

```rust
pub struct PromotionGateInput<'a> {
    pub record: &'a MemoryRecord,
    pub from_tier: MemoryVisibility,
    pub to_tier: MemoryVisibility,
    pub receipt: &'a PromotionConsentReceipt,
    pub now: &'a Rfc3339Timestamp,
    pub operation_id: &'a str,
    pub signer_key: &'a ed25519_dalek::VerifyingKey,
    pub revocation: &'a SharingRevocationState,
    pub rebac: &'a RebacContext,
}

pub struct SharingRevocationState {
    pub revoked_receipt_ids: BTreeSet<String>,
    pub revoked_share_link_ids: BTreeSet<String>,
    pub signer_key_revoked: bool,
}
```

The gate evaluates in this order:

1. Shape validation: receipt id, nonce, operation id, timestamps, tiers, and banned fields.
2. Signature validation: canonical receipt payload verifies against `signer_key`.
3. Human binding: receipt signer is the payload's `human_identity`.
4. Target binding: `receipt.payload.target_hash` equals `CanonicalRecordHash::compute(record)`.
5. Scope binding: receipt scope equals the record's tenant/workspace/entity and identity scope relevant to the promotion.
6. Tier binding: receipt `from_tier`/`to_tier` equals the requested promotion.
7. Freshness: `now < expires_at`.
8. Revocation: receipt id is absent from `revoked_receipt_ids` and `signer_key_revoked` is false.
9. ReBAC: existing #121 context allows a write relation for the requested shared tier.

Any denial returns a typed error and a body-free policy trace detail. The gate does not mutate state. The WAL/store layer calls it immediately before the atomic transaction that updates the tier, adds the ReBAC relation, and appends the consent journal row.

---

## 6. Policy Trace and Errors

Add policy trace vocabulary without leaking user content:

- Gate: `consent_receipt`
- Gate: `share_link`
- Details:
  - `consent:promote:allowed`
  - `consent:promote:expired`
  - `consent:promote:target_mismatch`
  - `consent:promote:scope_mismatch`
  - `consent:promote:tier_mismatch`
  - `consent:promote:bad_signature`
  - `consent:promote:revoked`
  - `consent:promote:not_human`
  - `share_link:grant:allowed`
  - `share_link:grant:expired`
  - `share_link:grant:revoked`
  - `share_link:grant:target_mismatch`

Errors should use existing `DomainError` variants where they already fit (`InvalidSignature`, `ExpiredIntent`, `ScopeDenied`, `Unauthorized`). If the existing variants produce ambiguous call-site logic, add narrow variants for promotion receipts and share links with symbolic messages only.

---

## 7. Consent Journal Privacy

Promotion consent journal rows use the existing body-free `ConsentKind::PromoteReceipt` payload:

```rust
ConsentPayload::PromoteReceipt {
    target_id_hash,
    from_tier,
    to_tier,
    receipt_id,
}
```

Share links use the existing `Decision` payload:

```rust
ConsentPayload::Decision {
    subject_code: format!("share_link:{link_id}"),
    policy_code: Some("grant" | "revoke"),
}
```

No receipt, share-link, or journal payload may serialize banned field names such as `body`, `text`, `content`, `raw`, `snippet`, `command`, `url`, `title`, `file_path`, `input`, `payload_text`, `user_input`, or `message`.

---

## 8. Testing

Tests are written first.

Core receipt tests:

- A well-formed promotion receipt verifies and authorizes `private -> team`.
- Tampering with `target_hash`, scope, nonce, operation id, tier, or human identity fails verification or apply-time validation.
- An expired receipt fails at apply time even if the signature is valid.
- A revoked receipt id fails closed.
- A non-human issuer cannot authorize shared-tier promotion.
- Receipt serialization does not contain banned body-bearing fields.

Share-link tests:

- A well-formed share link verifies and produces an allowed grant decision.
- Tampering with target hashes, scope, grant tier, expiry, or issuer fails.
- Revoked link ids fail closed.
- Bearer-style links still require expiry and revocation checks.
- Share-link journal events remain body-free.

ReBAC integration tests:

- A valid receipt without a matching ReBAC write relation still denies promotion.
- A valid receipt plus matching write relation allows promotion.
- `private` and `session` behavior remain unchanged.

Verification commands:

- `cargo test -p cairn-core sharing`
- `cargo test -p cairn-core rebac_policy`
- `cargo test -p cairn-core policy_trace`
- `cargo test --workspace`
- `cargo fmt --check`
- `scripts/check-core-boundary.sh`

---

## 9. Open Constraints

This design assumes the implementation branch is based on `origin/main` at or after merge commit `6206217c`, where #121 introduced `crate::rebac` and body-free ReBAC policy traces. Implementing from the older detached checkout would require redoing that prerequisite work and would create an unnecessary merge conflict.

The exact CLI and MCP shape for user-facing `cairn share` / `cairn promote` is intentionally not specified here. The core model must land first so later surfaces can reuse one policy implementation rather than recreating consent rules in adapters.
