# Federation hub protocol + propagation workflow — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the `cairn.federation.v1` extension protocol (`propose_share` / `accept_share` / `revoke_share`) and the `PropagationWorkflow` that moves consented records between Cairn vaults over a pluggable `FederationTransport`, satisfying issue #123 (brief §12.a, §10, §19).

**Architecture:** Pure-function verbs in `cairn-core` build signed envelopes (piggybacking on `domain::sharing` types from #122) and enqueue scheduler jobs. `PropagationHandler` in `cairn-workflows` drains the queue through a `FederationTransport` trait; an in-process `LoopbackTransport` in `cairn-test-fixtures` exercises the receive side via the existing MCP `accept_share` verb. Idempotency comes from `(issuer_key_id, link_id, nonce)` dedup against `consent_journal`. Capability is gated by `wiring::federation_extension_ready()` and stays off until the wiring task lands the end-to-end dispatch path.

**Tech Stack:** Rust 1.95, `tokio`, `rusqlite`, `serde`, `ed25519-dalek`, `thiserror`, `proptest`, `insta`, `schemars`, existing `cairn-core` contracts (`MemoryStore`, `JobStore`, `Keystore`, `ConsentLookup`).

**Spec:** `docs/superpowers/specs/2026-05-22-issue-123-federation-design.md`.

---

## File Structure

**Create:**
- `crates/cairn-idl/schema/verbs/propose_share.json`
- `crates/cairn-idl/schema/verbs/accept_share.json`
- `crates/cairn-idl/schema/verbs/revoke_share.json`
- `crates/cairn-core/src/contract/federation_transport.rs`
- `crates/cairn-core/src/domain/federation.rs`
- `crates/cairn-core/src/error/federation.rs`
- `crates/cairn-core/src/verbs/propose_share.rs`
- `crates/cairn-core/src/verbs/accept_share.rs`
- `crates/cairn-core/src/verbs/revoke_share.rs`
- `crates/cairn-core/tests/federation_propose.rs`
- `crates/cairn-core/tests/federation_accept.rs`
- `crates/cairn-core/tests/federation_revoke.rs`
- `crates/cairn-core/tests/federation_idempotency.rs` (proptest)
- `crates/cairn-core/tests/fixtures/federation/propose_envelope.json`
- `crates/cairn-core/tests/fixtures/federation/accept_envelope.json`
- `crates/cairn-core/tests/fixtures/federation/revoke_envelope.json`
- `crates/cairn-workflows/src/propagation/mod.rs`
- `crates/cairn-workflows/src/propagation/payload.rs`
- `crates/cairn-workflows/src/propagation/handler.rs`
- `crates/cairn-workflows/src/propagation/trigger.rs`
- `crates/cairn-workflows/tests/propagation_e2e.rs`
- `crates/cairn-test-fixtures/src/federation.rs`
- `crates/cairn-mcp/src/extensions/federation.rs`
- `crates/cairn-mcp/tests/federation_tools.rs`

**Modify:**
- `crates/cairn-core/src/contract/mod.rs` — re-export `federation_transport`
- `crates/cairn-core/src/domain/mod.rs` — re-export `federation`
- `crates/cairn-core/src/error/mod.rs` — re-export `federation`
- `crates/cairn-core/src/verbs/mod.rs` — re-export the three new verbs
- `crates/cairn-core/src/domain/consent.rs` — add three `ConsentEventKind` variants
- `crates/cairn-core/src/status/wiring.rs` — add `FEDERATION_*_WIRED` consts + `federation_extension_ready()`
- `crates/cairn-core/src/status/mod.rs` — gate `CairnMcpV1ExtensionFederation` advertisement on the new readiness fn
- `crates/cairn-core/src/status/remediation.rs` — add remediation hint for federation capability
- `crates/cairn-test-fixtures/src/lib.rs` — re-export `federation`
- `crates/cairn-workflows/src/lib.rs` — re-export `propagation`
- `crates/cairn-mcp/src/lib.rs` — register federation extension
- `crates/cairn-idl/schema/index.json` — register new verb schemas

---

## Conventions used in this plan

- Every code step shows the actual content. Tests run first and must fail; implementation follows.
- Existing types referenced without re-defining: `SignedShareLink`, `PromotionConsentReceipt`, `ShareLinkPayload`, `SharingDecisionKind`, `Identity`, `MemoryRecord`, `MemoryVisibility`, `ScopeTuple`, `Ed25519Signature`, `ConsentEvent`, `RebacContext`, `RebacAction`, `PolicyTraceEntry`. All live in `cairn-core` already.
- All Rust paths are absolute from the workspace root.
- Commits use Conventional Commits, ≤72-char subject. Body cites brief sections per CLAUDE.md §9.
- **Shared test helpers.** Tasks 7, 8, 9, and 16 reference `mod common;` — these are per-test-binary shim files at `crates/cairn-core/tests/common/mod.rs` exposing a `TestCtx` builder. The helper itself is built incrementally as the tasks land (Task 7 introduces the issuer-side variant, Task 8 adds the receiver-side variant, Task 9 adds the revoke-side helpers). Each helper method called from tests in this plan must exist on `TestCtx` before the test compiles. Pattern model: copy `crates/cairn-core/tests/share_link.rs` setup helpers (which were added by issue #122) and extend.
- **`test_store()` / `test_consent()` in Tasks 13–15.** These build an in-memory `SqliteStore` and pre-seed the consent_journal with the events listed in each test's comments. Concrete pattern: `SqliteStore::in_memory().await.unwrap()` for the store; the consent lookup is `store.consent_lookup()` once Task 12 lands.

---

## Task 1 — IDL: federation verb schemas

**Files:**
- Create: `crates/cairn-idl/schema/verbs/propose_share.json`
- Create: `crates/cairn-idl/schema/verbs/accept_share.json`
- Create: `crates/cairn-idl/schema/verbs/revoke_share.json`
- Modify: `crates/cairn-idl/schema/index.json`

- [ ] **Step 1 — Write `propose_share.json`** with the args shape:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://cairn.dev/schema/cairn.mcp.v1/verbs/propose_share.json",
  "title": "Cairn verb: propose_share",
  "x-cairn-contract": "cairn.mcp.v1",
  "x-cairn-verb-id": "propose_share",
  "x-cairn-capability": "cairn.mcp.v1.extension.federation",
  "x-cairn-auth": "human-signed",
  "x-cairn-extension": "cairn.federation.v1",
  "type": "object",
  "$defs": {
    "Args": {
      "type": "object",
      "additionalProperties": false,
      "required": ["record_ids", "scope", "grant_tier", "expires_at"],
      "properties": {
        "record_ids":      { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1, "maxItems": 64 },
        "grantee":         { "type": "string", "minLength": 1 },
        "scope":           { "$ref": "../common/scope.json#/$defs/ScopeTuple" },
        "grant_tier":      { "$ref": "../common/visibility.json#/$defs/MemoryVisibility" },
        "expires_at":      { "type": "string", "format": "date-time" },
        "peer":            { "type": "string", "minLength": 1 }
      }
    },
    "Result": {
      "type": "object",
      "additionalProperties": false,
      "required": ["link", "operation_id"],
      "properties": {
        "link":         { "$ref": "../common/signed_share_link.json" },
        "operation_id": { "type": "string", "minLength": 1 }
      }
    }
  }
}
```

- [ ] **Step 2 — Write `accept_share.json`**:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://cairn.dev/schema/cairn.mcp.v1/verbs/accept_share.json",
  "title": "Cairn verb: accept_share",
  "x-cairn-contract": "cairn.mcp.v1",
  "x-cairn-verb-id": "accept_share",
  "x-cairn-capability": "cairn.mcp.v1.extension.federation",
  "x-cairn-auth": "human-signed",
  "x-cairn-extension": "cairn.federation.v1",
  "type": "object",
  "$defs": {
    "Args": {
      "type": "object",
      "additionalProperties": false,
      "required": ["envelope"],
      "properties": {
        "envelope":   { "$ref": "../common/federation_envelope.json" }
      }
    },
    "Result": {
      "type": "object",
      "additionalProperties": false,
      "required": ["outcome", "applied_records"],
      "properties": {
        "outcome":         { "enum": ["accepted", "duplicate"] },
        "applied_records": { "type": "array", "items": { "type": "string" } }
      }
    }
  }
}
```

- [ ] **Step 3 — Write `revoke_share.json`**:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://cairn.dev/schema/cairn.mcp.v1/verbs/revoke_share.json",
  "title": "Cairn verb: revoke_share",
  "x-cairn-contract": "cairn.mcp.v1",
  "x-cairn-verb-id": "revoke_share",
  "x-cairn-capability": "cairn.mcp.v1.extension.federation",
  "x-cairn-auth": "human-signed",
  "x-cairn-extension": "cairn.federation.v1",
  "type": "object",
  "$defs": {
    "Args": {
      "type": "object",
      "additionalProperties": false,
      "required": ["link_id"],
      "properties": {
        "link_id":   { "type": "string", "minLength": 1 }
      }
    },
    "Result": {
      "type": "object",
      "additionalProperties": false,
      "required": ["operation_id"],
      "properties": {
        "operation_id": { "type": "string", "minLength": 1 }
      }
    }
  }
}
```

- [ ] **Step 4 — Create `common/federation_envelope.json`** (referenced above):

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://cairn.dev/schema/cairn.mcp.v1/common/federation_envelope.json",
  "title": "FederationEnvelope",
  "type": "object",
  "additionalProperties": false,
  "required": ["kind", "issuer_key_id"],
  "properties": {
    "kind":          { "enum": ["propose", "revoke"] },
    "issuer_key_id": { "type": "string", "minLength": 1 },
    "link":          { "$ref": "./signed_share_link.json" },
    "revocation":    { "$ref": "./signed_revocation.json" },
    "manifest":      { "type": "array", "items": { "$ref": "./memory_record.json" } }
  }
}
```

- [ ] **Step 5 — Create `common/signed_revocation.json`** with the same Ed25519 envelope shape used by `signed_share_link.json` plus `link_id` + `revoked_at`.

- [ ] **Step 6 — Register the three verb schemas in `crates/cairn-idl/schema/index.json`** (append entries to the `verbs:` array; copy an existing entry as the template).

- [ ] **Step 7 — Run codegen**:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

Expected: writes new generated types under `crates/cairn-core/src/generated/`. `git status` shows the regenerated files.

- [ ] **Step 8 — Run codegen `--check` to confirm clean diff**:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: exit 0, no diff.

- [ ] **Step 9 — Commit**:

```bash
git add crates/cairn-idl/schema crates/cairn-core/src/generated
git commit -m "feat(idl): add cairn.federation.v1 verb schemas (brief §8.0.a)"
```

---

## Task 2 — Domain types: `FederationEnvelope`, `PeerEndpoint`, `SignedRevocation`

**Files:**
- Create: `crates/cairn-core/src/domain/federation.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`

- [ ] **Step 1 — Write the failing test** at `crates/cairn-core/tests/federation_envelope_roundtrip.rs`:

```rust
use cairn_core::domain::federation::{FederationEnvelope, FederationKind};

#[test]
fn propose_envelope_roundtrips_through_canonical_json() {
    let fixture = include_str!("fixtures/federation/propose_envelope.json");
    let env: FederationEnvelope = serde_json::from_str(fixture).expect("parse");
    assert_eq!(env.kind, FederationKind::Propose);
    let re = serde_json::to_string(&env).expect("serialize");
    let env2: FederationEnvelope = serde_json::from_str(&re).expect("reparse");
    assert_eq!(env, env2);
}
```

- [ ] **Step 2 — Run it; verify it fails** with "no module `federation`":

```bash
cargo nextest run -p cairn-core --test federation_envelope_roundtrip --no-fail-fast
```

Expected: compile error mentioning `cairn_core::domain::federation`.

- [ ] **Step 3 — Create `crates/cairn-core/src/domain/federation.rs`**:

```rust
//! Federation envelopes (brief §12.a). Body-free protocol types that
//! wrap the signed primitives in `domain::sharing`.

use serde::{Deserialize, Serialize};

use crate::domain::sharing::SignedShareLink;
use crate::domain::{Ed25519Signature, Identity, MemoryRecord, Rfc3339Timestamp};

/// Discriminator for federation envelopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FederationKind {
    /// New share offer.
    Propose,
    /// Revoke a previously-issued share.
    Revoke,
}

/// Opaque peer address. The `FederationTransport` interprets this; core
/// only stores and forwards it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerEndpoint(pub String);

/// Signed revocation message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRevocation {
    pub link_id: String,
    pub revoked_at: Rfc3339Timestamp,
    pub issuer: Identity,
    pub key_version: u32,
    pub signature: Ed25519Signature,
}

/// Federation envelope. Carries one of: a propose with the signed link
/// + record manifest, or a revoke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FederationEnvelope {
    pub kind: FederationKind,
    pub issuer_key_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<SignedShareLink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revocation: Option<SignedRevocation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manifest: Vec<MemoryRecord>,
}

impl FederationEnvelope {
    /// Stable id for idempotency lookup: `(issuer_key_id, link_id, nonce)`.
    #[must_use]
    pub fn dedup_key(&self) -> Option<(&str, &str, &str)> {
        let link = self.link.as_ref()?;
        Some((&self.issuer_key_id, &link.link_id, &link.payload.nonce))
    }
}
```

- [ ] **Step 4 — Add to `crates/cairn-core/src/domain/mod.rs`**:

```rust
pub mod federation;
```

- [ ] **Step 5 — Create the fixture file** `crates/cairn-core/tests/fixtures/federation/propose_envelope.json` with a minimal valid propose envelope. Use deterministic test identities and nonces. (Sketch: copy a `SignedShareLink` from an existing test in `crates/cairn-core/tests/share_link.rs`, wrap as `{ "kind": "propose", "issuer_key_id": "...", "link": { ... }, "manifest": [] }`.)

- [ ] **Step 6 — Run the test**:

```bash
cargo nextest run -p cairn-core --test federation_envelope_roundtrip --no-fail-fast
```

Expected: PASS.

- [ ] **Step 7 — Commit**:

```bash
git add crates/cairn-core/src/domain crates/cairn-core/tests/federation_envelope_roundtrip.rs crates/cairn-core/tests/fixtures/federation/propose_envelope.json
git commit -m "feat(domain): add FederationEnvelope + PeerEndpoint (brief §12.a)"
```

---

## Task 3 — `FederationTransport` contract trait

**Files:**
- Create: `crates/cairn-core/src/contract/federation_transport.rs`
- Modify: `crates/cairn-core/src/contract/mod.rs`

- [ ] **Step 1 — Write the failing test** at `crates/cairn-core/tests/federation_transport_trait.rs`:

```rust
use cairn_core::contract::federation_transport::{FederationTransport, SendOutcome};
use cairn_core::domain::federation::{FederationEnvelope, FederationKind, PeerEndpoint};

struct AckOnly;

#[async_trait::async_trait]
impl FederationTransport for AckOnly {
    async fn send(&self, _: &FederationEnvelope, _: &PeerEndpoint) -> SendOutcome {
        SendOutcome::Ack
    }
}

#[tokio::test]
async fn ack_transport_returns_ack() {
    let env = FederationEnvelope {
        kind: FederationKind::Propose,
        issuer_key_id: "k1".into(),
        link: None,
        revocation: None,
        manifest: vec![],
    };
    let peer = PeerEndpoint("loopback".into());
    assert_eq!(AckOnly.send(&env, &peer).await, SendOutcome::Ack);
}
```

- [ ] **Step 2 — Run it; verify it fails**:

```bash
cargo nextest run -p cairn-core --test federation_transport_trait --no-fail-fast
```

Expected: compile error mentioning `federation_transport`.

- [ ] **Step 3 — Create `crates/cairn-core/src/contract/federation_transport.rs`**:

```rust
//! Pluggable transport for federation envelopes. Default implementation
//! is the in-memory loopback in `cairn-test-fixtures`; production
//! deployments wire an HTTP adapter (separate crate, future issue).

use crate::domain::federation::{FederationEnvelope, PeerEndpoint};

/// Reason carried with transport outcomes. Body-free for audit safety.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportReason(pub String);

/// Result of one `send` attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SendOutcome {
    /// Peer acknowledged successful apply.
    Ack,
    /// Retryable failure (timeouts, 5xx, network blip).
    Transient(TransportReason),
    /// Non-retryable failure (4xx, signature reject, ReBAC deny).
    Permanent(TransportReason),
}

/// Pluggable transport. Implementations must be cancel-safe — the
/// scheduler may drop the future after lease expiry.
#[async_trait::async_trait]
pub trait FederationTransport: Send + Sync {
    /// Send one envelope to one peer.
    async fn send(&self, envelope: &FederationEnvelope, peer: &PeerEndpoint) -> SendOutcome;
}
```

- [ ] **Step 4 — Add to `crates/cairn-core/src/contract/mod.rs`**:

```rust
pub mod federation_transport;
```

- [ ] **Step 5 — Run the test**:

```bash
cargo nextest run -p cairn-core --test federation_transport_trait --no-fail-fast
```

Expected: PASS.

- [ ] **Step 6 — Commit**:

```bash
git add crates/cairn-core/src/contract crates/cairn-core/tests/federation_transport_trait.rs
git commit -m "feat(contract): add FederationTransport trait (brief §12.a)"
```

---

## Task 4 — `FederationError` enum + policy-trace mapping

**Files:**
- Create: `crates/cairn-core/src/error/federation.rs`
- Modify: `crates/cairn-core/src/error/mod.rs`

- [ ] **Step 1 — Write the failing test** at `crates/cairn-core/tests/federation_error_trace.rs`:

```rust
use cairn_core::domain::sharing::SharingDecisionKind;
use cairn_core::error::federation::FederationError;

#[test]
fn each_error_maps_to_sharing_decision_kind() {
    use FederationError as E;
    assert_eq!(E::Expired.sharing_decision(), Some(SharingDecisionKind::Expired));
    assert_eq!(E::TargetMismatch.sharing_decision(), Some(SharingDecisionKind::TargetMismatch));
    assert_eq!(E::ScopeMismatch.sharing_decision(), Some(SharingDecisionKind::ScopeMismatch));
    assert_eq!(E::TierMismatch.sharing_decision(), Some(SharingDecisionKind::TierMismatch));
    assert_eq!(E::BadSignature.sharing_decision(), Some(SharingDecisionKind::BadSignature));
    assert_eq!(E::Revoked.sharing_decision(), Some(SharingDecisionKind::Revoked));
    assert_eq!(E::NotHuman.sharing_decision(), Some(SharingDecisionKind::NotHuman));
    assert_eq!(E::NoRebacRelation.sharing_decision(), Some(SharingDecisionKind::NoRebacRelation));
    assert_eq!(E::InvalidShape.sharing_decision(), Some(SharingDecisionKind::InvalidShape));
    assert_eq!(E::DuplicateNonce.sharing_decision(), None);
    assert_eq!(E::UnknownLink.sharing_decision(), None);
    assert_eq!(E::CapabilityDisabled.sharing_decision(), None);
}
```

- [ ] **Step 2 — Run it; verify it fails**:

```bash
cargo nextest run -p cairn-core --test federation_error_trace --no-fail-fast
```

Expected: compile error.

- [ ] **Step 3 — Create `crates/cairn-core/src/error/federation.rs`**:

```rust
//! Typed errors for `propose_share` / `accept_share` / `revoke_share`.
//! Body-free; emits `PolicyTraceEntry` via existing `SharingDecisionKind`.

use thiserror::Error;

use crate::domain::sharing::SharingDecisionKind;

/// Federation verb error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FederationError {
    #[error("receipt or link expired")]
    Expired,
    #[error("target hash mismatch")]
    TargetMismatch,
    #[error("scope outside grant")]
    ScopeMismatch,
    #[error("visibility tier outside grant")]
    TierMismatch,
    #[error("signature verification failed")]
    BadSignature,
    #[error("link revoked")]
    Revoked,
    #[error("signer is not a human identity")]
    NotHuman,
    #[error("rebac denied")]
    NoRebacRelation,
    #[error("envelope shape invalid")]
    InvalidShape,
    #[error("duplicate nonce")]
    DuplicateNonce,
    #[error("unknown share link")]
    UnknownLink,
    #[error("federation capability not advertised")]
    CapabilityDisabled,
}

impl FederationError {
    /// Mapping to the policy-trace decision kind shared with `domain::sharing`.
    /// `None` for federation-only errors (no `SharingDecisionKind` exists).
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
```

- [ ] **Step 4 — Add to `crates/cairn-core/src/error/mod.rs`**:

```rust
pub mod federation;
```

- [ ] **Step 5 — Run the test**:

```bash
cargo nextest run -p cairn-core --test federation_error_trace --no-fail-fast
```

Expected: PASS.

- [ ] **Step 6 — Commit**:

```bash
git add crates/cairn-core/src/error crates/cairn-core/tests/federation_error_trace.rs
git commit -m "feat(error): add FederationError mapping to SharingDecisionKind"
```

---

## Task 5 — `ConsentEvent` kinds for federation

**Files:**
- Modify: `crates/cairn-core/src/domain/consent.rs`

- [ ] **Step 1 — Read** `crates/cairn-core/src/domain/consent.rs` lines 1-200 to see how existing `ConsentEventKind` variants are defined and validated.

- [ ] **Step 2 — Write the failing test** at the bottom of `crates/cairn-core/src/domain/consent.rs` inside the existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn federation_grant_kind_validates() {
    let event = ConsentEvent::new(
        /* id */ "ulid-federation-grant".into(),
        ConsentEventKind::FederationGrant,
        /* subject */ "share-link-123".into(),
        // ... fill remaining fields per the existing test for ConsentEventKind::Grant ...
    );
    event.validate().expect("federation_grant should validate");
}

#[test]
fn federation_accept_kind_validates() {
    let event = /* same pattern with FederationAccept */;
    event.validate().expect("federation_accept should validate");
}

#[test]
fn federation_revoke_kind_validates() {
    let event = /* same pattern with FederationRevoke */;
    event.validate().expect("federation_revoke should validate");
}
```

- [ ] **Step 3 — Run them; verify they fail** with "no variant `FederationGrant`":

```bash
cargo nextest run -p cairn-core consent::tests::federation
```

Expected: compile error.

- [ ] **Step 4 — Add three variants** to the `ConsentEventKind` enum:

```rust
pub enum ConsentEventKind {
    // ... existing variants ...
    /// Outbound share offer minted.
    FederationGrant,
    /// Inbound share applied.
    FederationAccept,
    /// Share revoked (issuer side or receiver side).
    FederationRevoke,
}
```

Update the `as_str()` impl and any code-validator match arms (`validate_kind_code_pair`, `validate_payload_for_kind`) to accept the new kinds with the appropriate payload shape (`ShareLinkJournalDecision`-style). Pattern: follow `Grant` / `Revoke` arms already present.

- [ ] **Step 5 — Run the tests**:

```bash
cargo nextest run -p cairn-core consent::tests::federation
```

Expected: PASS.

- [ ] **Step 6 — Run the full crate to confirm no regressions**:

```bash
cargo nextest run -p cairn-core --no-fail-fast
```

Expected: all green.

- [ ] **Step 7 — Commit**:

```bash
git add crates/cairn-core/src/domain/consent.rs
git commit -m "feat(consent): add Federation{Grant,Accept,Revoke} event kinds"
```

---

## Task 6 — Wiring constants + readiness fn

**Files:**
- Modify: `crates/cairn-core/src/status/wiring.rs`
- Modify: `crates/cairn-core/src/status/mod.rs`
- Modify: `crates/cairn-core/src/status/remediation.rs`

- [ ] **Step 1 — Write the failing test** in `crates/cairn-core/src/status/tests.rs` (next to existing tests):

```rust
#[test]
fn federation_extension_ready_off_by_default() {
    assert!(!crate::status::wiring::federation_extension_ready());
}

#[test]
fn federation_capability_omitted_when_unwired() {
    let advertised = crate::status::advertise(/* same args as existing tests */);
    assert!(!advertised.capabilities.contains(&Capabilities::CairnMcpV1ExtensionFederation));
}
```

- [ ] **Step 2 — Run them; verify failure** ("function not found").

- [ ] **Step 3 — Append to `crates/cairn-core/src/status/wiring.rs`** (mirror the coord pattern):

```rust
/// `cairn.federation.v1` extension capability registration (issue #123).
pub const FEDERATION_EXTENSION_WIRED: bool = false;

/// `propose_share` / `revoke_share` verb dispatch is wired (CLI/MCP/SDK).
pub const FEDERATION_PROPOSE_DISPATCH_WIRED: bool = false;

/// `accept_share` verb dispatch is wired (inbound receive path).
pub const FEDERATION_ACCEPT_DISPATCH_WIRED: bool = false;

/// `PropagationWorkflow` handler is registered on the scheduler.
pub const FEDERATION_WORKFLOW_WIRED: bool = false;

/// MCP tool declarations for `cairn.federation.v1` are wired.
pub const FEDERATION_MCP_TOOLS_WIRED: bool = false;

/// Single readiness source for advertising `cairn.federation.v1`.
#[must_use]
pub const fn federation_extension_ready() -> bool {
    FEDERATION_EXTENSION_WIRED
        && FEDERATION_PROPOSE_DISPATCH_WIRED
        && FEDERATION_ACCEPT_DISPATCH_WIRED
        && FEDERATION_WORKFLOW_WIRED
        && FEDERATION_MCP_TOOLS_WIRED
}
```

- [ ] **Step 4 — Gate the existing federation advertisement in `crates/cairn-core/src/status/mod.rs`**. Find the block (search for `CairnMcpV1ExtensionFederation`) and wrap its emission in `if phase >= Phase::V0_3 && wiring::federation_extension_ready() { ... }`. Mirror the pattern used for `coord_extension_ready()`.

- [ ] **Step 5 — Add remediation entry in `crates/cairn-core/src/status/remediation.rs`** for `CairnMcpV1ExtensionFederation`:

```rust
Capabilities::CairnMcpV1ExtensionFederation => Some(
    "enable federation: set federation.enabled = true and configure a peer endpoint",
),
```

(Match exact pattern of existing rows.)

- [ ] **Step 6 — Run the tests**:

```bash
cargo nextest run -p cairn-core status::
```

Expected: PASS.

- [ ] **Step 7 — Commit**:

```bash
git add crates/cairn-core/src/status
git commit -m "feat(status): add federation wiring gates (initially false)"
```

---

## Task 7 — `propose_share` verb

**Files:**
- Create: `crates/cairn-core/src/verbs/propose_share.rs`
- Create: `crates/cairn-core/tests/federation_propose.rs`
- Modify: `crates/cairn-core/src/verbs/mod.rs`

- [ ] **Step 1 — Read** the existing pattern in `crates/cairn-core/src/verbs/ingest.rs` and `crates/cairn-core/src/domain/sharing.rs` lines 240–620 to see how the share-link signing + ReBAC gating is wrapped.

- [ ] **Step 2 — Write the failing test** at `crates/cairn-core/tests/federation_propose.rs`:

```rust
use cairn_core::domain::MemoryVisibility;
use cairn_core::error::federation::FederationError;
use cairn_core::verbs::propose_share::{propose_share, ProposeShareRequest};

mod common;
use common::*; // test fixture helpers (build_record, build_rebac_ctx, build_keystore)

#[tokio::test]
async fn propose_share_happy_path_signs_link_and_enqueues_job() {
    let ctx = TestCtx::new().await;
    let record = ctx.upsert_test_record(MemoryVisibility::Project).await;

    let req = ProposeShareRequest {
        record_ids: vec![record.id.clone()],
        grantee: Some(ctx.peer_identity()),
        scope: ctx.scope(),
        grant_tier: MemoryVisibility::Team,
        expires_at: ctx.in_one_hour(),
        peer: Some(ctx.peer_endpoint()),
    };

    let result = propose_share(req, &ctx.deps()).await.expect("ok");

    // Signed link minted.
    assert_eq!(result.link.payload.grant_tier, MemoryVisibility::Team);
    // Job row inserted.
    let jobs = ctx.list_pending_jobs("federation.propagate.outbound_share").await;
    assert_eq!(jobs.len(), 1);
    // ConsentEvent written.
    let events = ctx.consent_events_for(&result.link.link_id).await;
    assert!(events.iter().any(|e| matches!(e.kind, cairn_core::domain::consent::ConsentEventKind::FederationGrant)));
}

#[tokio::test]
async fn propose_share_denies_when_capability_unwired() {
    // Override wiring gate via test hook (Task 6 added const; expose
    // a `#[cfg(test)]` setter in wiring.rs if needed, or use a feature
    // flag — pick whichever the existing `coord_extension_ready` tests use).
    let ctx = TestCtx::new().await;
    let err = propose_share(/* … */, &ctx.deps_with_federation_off()).await.unwrap_err();
    assert_eq!(err, FederationError::CapabilityDisabled);
}

#[tokio::test]
async fn propose_share_denies_when_rebac_blocks_tier() {
    let ctx = TestCtx::new().await;
    let record = ctx.upsert_test_record(MemoryVisibility::Project).await;
    let err = propose_share(
        ProposeShareRequest {
            record_ids: vec![record.id.clone()],
            grant_tier: MemoryVisibility::Public, // issuer has no share grant for Public
            // …
        },
        &ctx.deps(),
    ).await.unwrap_err();
    assert_eq!(err, FederationError::NoRebacRelation);
}
```

- [ ] **Step 3 — Create the verb stub** at `crates/cairn-core/src/verbs/propose_share.rs`:

```rust
//! `propose_share` verb (brief §12.a).

use serde::{Deserialize, Serialize};

use crate::domain::federation::PeerEndpoint;
use crate::domain::sharing::{ShareLinkPayload, SignedShareLink};
use crate::domain::{Identity, MemoryVisibility, Rfc3339Timestamp, ScopeTuple};
use crate::error::federation::FederationError;

#[derive(Debug, Clone)]
pub struct ProposeShareRequest {
    pub record_ids: Vec<String>,
    pub grantee: Option<Identity>,
    pub scope: ScopeTuple,
    pub grant_tier: MemoryVisibility,
    pub expires_at: Rfc3339Timestamp,
    pub peer: Option<PeerEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposeShareResponse {
    pub link: SignedShareLink,
    pub operation_id: String,
}

/// Dispatch deps (memory store, keystore, rebac, job store, clock).
/// Concrete struct passed by the call site; pure-function over its inputs.
pub struct ProposeShareDeps<'a> {
    pub store: &'a dyn crate::contract::memory_store::MemoryStore,
    pub jobs: &'a dyn crate::contract::job_store::JobStore,
    pub keystore: &'a dyn crate::contract::keystore::Keystore,
    pub rebac: &'a crate::rebac::RebacContext,
    pub clock: &'a dyn crate::contract::clock::Clock,
    pub federation_ready: bool,
}

pub async fn propose_share(
    req: ProposeShareRequest,
    deps: &ProposeShareDeps<'_>,
) -> Result<ProposeShareResponse, FederationError> {
    if !deps.federation_ready {
        return Err(FederationError::CapabilityDisabled);
    }

    // 1. Validate shape (record_ids non-empty, expires_at in future, etc.).
    // 2. Load records, compute canonical hashes, build ShareLinkPayload.
    // 3. ReBAC check: RebacAction::Share for issuer on each record at grant_tier.
    //    (Reuse the existing predicate from domain::sharing.)
    // 4. Sign the payload via deps.keystore (existing primitive used by #122).
    // 5. Open WAL tx; append ConsentEvent::FederationGrant; enqueue
    //    federation.propagate.outbound_share job; commit.
    // 6. Return SignedShareLink + operation_id.

    todo!("implement per spec §5 outbound propose")
}
```

- [ ] **Step 4 — Run the tests; verify failure** at the `todo!`:

```bash
cargo nextest run -p cairn-core --test federation_propose --no-fail-fast
```

Expected: PASS for compile, FAIL at `todo!` panic.

- [ ] **Step 5 — Implement** `propose_share` per the comments. Reuse `domain::sharing::sign_share_link` (or whatever helper #122 exposed) for step 4, and `RebacContext::check` for step 3. The atomic step 5 goes through `deps.jobs.enqueue_with_consent(operation_id, kind = JobKind::new("federation.propagate.outbound_share"), payload, consent_event)` — if no such combined API exists, do the two operations inside an explicit SQLite transaction acquired from the job store.

- [ ] **Step 6 — Run the tests**:

```bash
cargo nextest run -p cairn-core --test federation_propose --no-fail-fast
```

Expected: all three tests PASS.

- [ ] **Step 7 — Register the verb** in `crates/cairn-core/src/verbs/mod.rs`:

```rust
pub mod propose_share;
```

- [ ] **Step 8 — Commit**:

```bash
git add crates/cairn-core/src/verbs/propose_share.rs crates/cairn-core/src/verbs/mod.rs crates/cairn-core/tests/federation_propose.rs
git commit -m "feat(verbs): add propose_share (brief §12.a)"
```

---

## Task 8 — `accept_share` verb (idempotent receive path)

**Files:**
- Create: `crates/cairn-core/src/verbs/accept_share.rs`
- Create: `crates/cairn-core/tests/federation_accept.rs`
- Modify: `crates/cairn-core/src/verbs/mod.rs`

- [ ] **Step 1 — Write the failing test** at `crates/cairn-core/tests/federation_accept.rs`:

```rust
use cairn_core::domain::federation::{FederationEnvelope, FederationKind};
use cairn_core::error::federation::FederationError;
use cairn_core::verbs::accept_share::{accept_share, AcceptShareRequest, AcceptOutcome};

mod common;
use common::*;

#[tokio::test]
async fn accept_share_applies_records_with_inbound_provenance() {
    let ctx = TestCtx::new_receiver().await;
    let envelope = ctx.build_signed_propose_envelope_with_one_record();

    let resp = accept_share(AcceptShareRequest { envelope: envelope.clone() }, &ctx.deps())
        .await
        .expect("ok");

    assert_eq!(resp.outcome, AcceptOutcome::Accepted);
    assert_eq!(resp.applied_records.len(), 1);

    // Provenance + tier cap applied.
    let stored = ctx.fetch(&resp.applied_records[0]).await;
    assert_eq!(stored.visibility, envelope.link.unwrap().payload.grant_tier);
    assert!(matches!(stored.provenance.source, cairn_core::domain::record::ProvenanceSource::ShareLink { .. }));

    // ConsentEvent::FederationAccept written.
    let events = ctx.consent_events_for_envelope(&envelope).await;
    assert!(events.iter().any(|e| matches!(e.kind, cairn_core::domain::consent::ConsentEventKind::FederationAccept)));
}

#[tokio::test]
async fn accept_share_is_idempotent_under_replay() {
    let ctx = TestCtx::new_receiver().await;
    let envelope = ctx.build_signed_propose_envelope_with_one_record();

    let first = accept_share(AcceptShareRequest { envelope: envelope.clone() }, &ctx.deps()).await.unwrap();
    let again = accept_share(AcceptShareRequest { envelope: envelope.clone() }, &ctx.deps()).await.unwrap();

    assert_eq!(first.outcome, AcceptOutcome::Accepted);
    assert_eq!(again.outcome, AcceptOutcome::Duplicate);
    assert_eq!(first.applied_records, again.applied_records);

    // Only one consent_journal Accept row.
    let events = ctx.consent_events_for_envelope(&envelope).await;
    let accepts = events.iter().filter(|e| matches!(e.kind, cairn_core::domain::consent::ConsentEventKind::FederationAccept)).count();
    assert_eq!(accepts, 1);
}

#[tokio::test]
async fn accept_share_rejects_expired_link() {
    let ctx = TestCtx::new_receiver().await;
    let envelope = ctx.build_envelope_expired_1h_ago();
    let err = accept_share(AcceptShareRequest { envelope }, &ctx.deps()).await.unwrap_err();
    assert_eq!(err, FederationError::Expired);
}

#[tokio::test]
async fn accept_share_rejects_bad_signature() {
    let ctx = TestCtx::new_receiver().await;
    let envelope = ctx.build_envelope_with_tampered_payload();
    let err = accept_share(AcceptShareRequest { envelope }, &ctx.deps()).await.unwrap_err();
    assert_eq!(err, FederationError::BadSignature);
}

#[tokio::test]
async fn accept_share_rejects_when_receiver_has_no_rebac_relation() {
    let ctx = TestCtx::new_receiver_without_write_relation().await;
    let envelope = ctx.build_signed_propose_envelope_with_one_record();
    let err = accept_share(AcceptShareRequest { envelope }, &ctx.deps()).await.unwrap_err();
    assert_eq!(err, FederationError::NoRebacRelation);
}

#[tokio::test]
async fn accept_share_rejects_after_revocation() {
    let ctx = TestCtx::new_receiver().await;
    let envelope = ctx.build_signed_propose_envelope_with_one_record();
    let _ = accept_share(AcceptShareRequest { envelope: envelope.clone() }, &ctx.deps()).await.unwrap();
    // Receiver-side revoke event was written by Task 9; for now simulate it
    // by writing a FederationRevoke ConsentEvent directly to the journal.
    ctx.mark_link_revoked(&envelope.link.as_ref().unwrap().link_id).await;
    let err = accept_share(AcceptShareRequest { envelope }, &ctx.deps()).await.unwrap_err();
    assert_eq!(err, FederationError::Revoked);
}
```

- [ ] **Step 2 — Run; verify failure**:

```bash
cargo nextest run -p cairn-core --test federation_accept --no-fail-fast
```

Expected: compile error.

- [ ] **Step 3 — Create the verb** at `crates/cairn-core/src/verbs/accept_share.rs`:

```rust
//! `accept_share` verb (brief §12.a). Inbound apply with idempotency.

use serde::{Deserialize, Serialize};

use crate::domain::federation::{FederationEnvelope, FederationKind};
use crate::error::federation::FederationError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptShareRequest {
    pub envelope: FederationEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AcceptOutcome {
    Accepted,
    Duplicate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptShareResponse {
    pub outcome: AcceptOutcome,
    pub applied_records: Vec<String>,
}

pub struct AcceptShareDeps<'a> {
    pub store: &'a dyn crate::contract::memory_store::MemoryStore,
    pub consent: &'a dyn crate::contract::consent_lookup::ConsentLookup,
    pub jobs: &'a dyn crate::contract::job_store::JobStore,
    pub rebac: &'a crate::rebac::RebacContext,
    pub clock: &'a dyn crate::contract::clock::Clock,
    pub federation_ready: bool,
}

pub async fn accept_share(
    req: AcceptShareRequest,
    deps: &AcceptShareDeps<'_>,
) -> Result<AcceptShareResponse, FederationError> {
    if !deps.federation_ready {
        return Err(FederationError::CapabilityDisabled);
    }

    match req.envelope.kind {
        FederationKind::Propose => accept_propose(req.envelope, deps).await,
        FederationKind::Revoke => accept_revoke(req.envelope, deps).await,
    }
}

async fn accept_propose(
    envelope: FederationEnvelope,
    deps: &AcceptShareDeps<'_>,
) -> Result<AcceptShareResponse, FederationError> {
    let link = envelope.link.as_ref().ok_or(FederationError::InvalidShape)?;

    // 1. Verify signature, expires_at, target hashes, scope, tier. Use
    //    domain::sharing::verify_share_link helper from #122.
    domain::sharing::verify_share_link(link, deps.clock.now())
        .map_err(map_sharing_to_federation)?;

    // 2. Check receiver ReBAC for RebacAction::Write at link.payload.grant_tier.
    deps.rebac
        .check(RebacAction::Write, &link.payload.scope, link.payload.grant_tier)
        .map_err(|_| FederationError::NoRebacRelation)?;

    // 3. Idempotency: lookup (issuer_key_id, link_id, nonce) in consent_journal.
    let dedup = envelope.dedup_key().ok_or(FederationError::InvalidShape)?;
    if let Some(prior) = deps.consent.find_federation_accept(dedup).await {
        return Ok(AcceptShareResponse {
            outcome: AcceptOutcome::Duplicate,
            applied_records: prior.applied_records,
        });
    }

    // 4. Revocation check: if a FederationRevoke event exists for link_id, reject.
    if deps.consent.is_link_revoked(&link.link_id).await {
        return Err(FederationError::Revoked);
    }

    // 5. Atomic WAL tx: upsert each record in envelope.manifest with
    //    provenance::ShareLink { link_id, issuer }, visibility capped at
    //    grant_tier, trust_status = inbound_shared. Append
    //    ConsentEvent::FederationAccept.
    let applied = apply_inbound_records(&envelope, deps).await?;

    Ok(AcceptShareResponse {
        outcome: AcceptOutcome::Accepted,
        applied_records: applied,
    })
}

async fn accept_revoke(
    envelope: FederationEnvelope,
    deps: &AcceptShareDeps<'_>,
) -> Result<AcceptShareResponse, FederationError> {
    let revocation = envelope.revocation.as_ref().ok_or(FederationError::InvalidShape)?;

    // 1. Verify revocation signature against issuer's key.
    domain::sharing::verify_revocation(revocation, deps.clock.now())
        .map_err(map_sharing_to_federation)?;

    // 2. Find projected records by link_id; if none, idempotently succeed.
    let projected = deps.store.find_records_by_share_link(&revocation.link_id).await
        .map_err(|_| FederationError::UnknownLink)?;

    // 3. Atomic WAL tx: tombstone each projected record via existing
    //    forget --record Phase A+B state machine. Append
    //    ConsentEvent::FederationRevoke.
    let removed = tombstone_inbound_records(&projected, &revocation.link_id, deps).await?;

    Ok(AcceptShareResponse {
        outcome: AcceptOutcome::Accepted,
        applied_records: removed,
    })
}

fn map_sharing_to_federation(kind: crate::domain::sharing::SharingDecisionKind) -> FederationError {
    use crate::domain::sharing::SharingDecisionKind as S;
    match kind {
        S::Expired => FederationError::Expired,
        S::TargetMismatch => FederationError::TargetMismatch,
        S::ScopeMismatch => FederationError::ScopeMismatch,
        S::TierMismatch => FederationError::TierMismatch,
        S::BadSignature => FederationError::BadSignature,
        S::Revoked => FederationError::Revoked,
        S::NotHuman => FederationError::NotHuman,
        S::NoRebacRelation => FederationError::NoRebacRelation,
        S::InvalidShape => FederationError::InvalidShape,
        S::Allowed => unreachable!("Allowed is not an error"),
    }
}

// Helpers `apply_inbound_records` and `tombstone_inbound_records` are
// implemented as private functions in this module; see Step 4 below.
```

- [ ] **Step 4 — Implement the helpers** `apply_inbound_records` and `tombstone_inbound_records` directly below the public functions. Pattern: open a `MemoryStore` transaction, fan-out per-record upserts/tombstones, append the `ConsentEvent` in the same tx, commit. Reuse the existing `forget --record` Phase A function for tombstoning (search `crates/cairn-core/src/verbs/` for the entry point).

If `ConsentLookup::find_federation_accept` and `is_link_revoked` don't exist, add them to `contract::consent_lookup`. Implementation lives in `cairn-store-sqlite` (Task 12).

- [ ] **Step 5 — Run the tests**:

```bash
cargo nextest run -p cairn-core --test federation_accept --no-fail-fast
```

Expected: all six PASS.

- [ ] **Step 6 — Register the verb** in `crates/cairn-core/src/verbs/mod.rs`:

```rust
pub mod accept_share;
```

- [ ] **Step 7 — Commit**:

```bash
git add crates/cairn-core/src/verbs/accept_share.rs crates/cairn-core/src/verbs/mod.rs crates/cairn-core/tests/federation_accept.rs crates/cairn-core/src/contract/consent_lookup.rs
git commit -m "feat(verbs): add accept_share with nonce dedup (brief §12.a)"
```

---

## Task 9 — `revoke_share` verb

**Files:**
- Create: `crates/cairn-core/src/verbs/revoke_share.rs`
- Create: `crates/cairn-core/tests/federation_revoke.rs`
- Modify: `crates/cairn-core/src/verbs/mod.rs`

- [ ] **Step 1 — Write the failing test** at `crates/cairn-core/tests/federation_revoke.rs`:

```rust
use cairn_core::error::federation::FederationError;
use cairn_core::verbs::revoke_share::{revoke_share, RevokeShareRequest};

mod common;
use common::*;

#[tokio::test]
async fn revoke_share_marks_link_revoked_and_enqueues_outbound() {
    let ctx = TestCtx::new().await;
    let link = ctx.mint_propose_share().await;

    let resp = revoke_share(
        RevokeShareRequest { link_id: link.link_id.clone() },
        &ctx.deps(),
    ).await.expect("ok");

    assert!(!resp.operation_id.is_empty());

    // ConsentEvent FederationRevoke present.
    let events = ctx.consent_events_for(&link.link_id).await;
    assert!(events.iter().any(|e| matches!(e.kind, cairn_core::domain::consent::ConsentEventKind::FederationRevoke)));

    // Outbound revoke job enqueued.
    let jobs = ctx.list_pending_jobs("federation.propagate.outbound_revoke").await;
    assert_eq!(jobs.len(), 1);
}

#[tokio::test]
async fn revoke_share_rejects_unknown_link() {
    let ctx = TestCtx::new().await;
    let err = revoke_share(RevokeShareRequest { link_id: "no-such-link".into() }, &ctx.deps())
        .await.unwrap_err();
    assert_eq!(err, FederationError::UnknownLink);
}

#[tokio::test]
async fn revoke_share_is_idempotent() {
    let ctx = TestCtx::new().await;
    let link = ctx.mint_propose_share().await;
    let _ = revoke_share(RevokeShareRequest { link_id: link.link_id.clone() }, &ctx.deps()).await.unwrap();
    let again = revoke_share(RevokeShareRequest { link_id: link.link_id.clone() }, &ctx.deps()).await.unwrap();
    // Same operation_id pinned to the first revoke; no second journal entry.
    let revokes = ctx.consent_events_for(&link.link_id).await.iter()
        .filter(|e| matches!(e.kind, cairn_core::domain::consent::ConsentEventKind::FederationRevoke)).count();
    assert_eq!(revokes, 1);
    let jobs = ctx.list_pending_jobs("federation.propagate.outbound_revoke").await;
    assert_eq!(jobs.len(), 1);
}
```

- [ ] **Step 2 — Run; verify failure**.

- [ ] **Step 3 — Create the verb** at `crates/cairn-core/src/verbs/revoke_share.rs`:

```rust
//! `revoke_share` verb (brief §12.a).

use serde::{Deserialize, Serialize};

use crate::domain::federation::SignedRevocation;
use crate::error::federation::FederationError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeShareRequest {
    pub link_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeShareResponse {
    pub operation_id: String,
    pub revocation: SignedRevocation,
}

pub struct RevokeShareDeps<'a> {
    pub store: &'a dyn crate::contract::memory_store::MemoryStore,
    pub jobs: &'a dyn crate::contract::job_store::JobStore,
    pub keystore: &'a dyn crate::contract::keystore::Keystore,
    pub consent: &'a dyn crate::contract::consent_lookup::ConsentLookup,
    pub clock: &'a dyn crate::contract::clock::Clock,
    pub federation_ready: bool,
}

pub async fn revoke_share(
    req: RevokeShareRequest,
    deps: &RevokeShareDeps<'_>,
) -> Result<RevokeShareResponse, FederationError> {
    if !deps.federation_ready {
        return Err(FederationError::CapabilityDisabled);
    }

    // 1. Lookup the original ShareLinkPayload by link_id. Missing → UnknownLink.
    let original = deps.consent.find_share_link(&req.link_id).await
        .ok_or(FederationError::UnknownLink)?;

    // 2. Idempotency: if a FederationRevoke event already exists for link_id,
    //    return its operation_id without minting a new revocation.
    if let Some(prior) = deps.consent.find_revocation(&req.link_id).await {
        return Ok(RevokeShareResponse {
            operation_id: prior.operation_id,
            revocation: prior.revocation,
        });
    }

    // 3. Sign a SignedRevocation { link_id, revoked_at, issuer, signature }.
    let revocation = sign_revocation(&original, deps).await?;

    // 4. Atomic WAL tx: append ConsentEvent::FederationRevoke, enqueue
    //    federation.propagate.outbound_revoke job.
    let operation_id = enqueue_outbound_revoke(&revocation, &original.peer, deps).await?;

    Ok(RevokeShareResponse { operation_id, revocation })
}

// Helpers sign_revocation, enqueue_outbound_revoke implemented below.
```

- [ ] **Step 4 — Implement the helpers**. Reuse keystore primitives from #122 for the signature.

- [ ] **Step 5 — Run the tests**:

```bash
cargo nextest run -p cairn-core --test federation_revoke --no-fail-fast
```

Expected: PASS.

- [ ] **Step 6 — Register** in `crates/cairn-core/src/verbs/mod.rs`:

```rust
pub mod revoke_share;
```

- [ ] **Step 7 — Commit**:

```bash
git add crates/cairn-core/src/verbs/revoke_share.rs crates/cairn-core/src/verbs/mod.rs crates/cairn-core/tests/federation_revoke.rs
git commit -m "feat(verbs): add revoke_share (brief §12.a)"
```

---

## Task 10 — `LoopbackTransport` in `cairn-test-fixtures`

**Files:**
- Create: `crates/cairn-test-fixtures/src/federation.rs`
- Modify: `crates/cairn-test-fixtures/src/lib.rs`

- [ ] **Step 1 — Write the failing test** at `crates/cairn-test-fixtures/tests/loopback_transport.rs`:

```rust
use cairn_core::contract::federation_transport::{FederationTransport, SendOutcome, TransportReason};
use cairn_core::domain::federation::{FederationEnvelope, FederationKind, PeerEndpoint};
use cairn_test_fixtures::federation::{LoopbackTransport, ProgrammedOutcome};

#[tokio::test]
async fn loopback_returns_programmed_outcomes_in_order() {
    let t = LoopbackTransport::new();
    t.program([
        ProgrammedOutcome::Transient("boom".into()),
        ProgrammedOutcome::Transient("boom".into()),
        ProgrammedOutcome::Ack,
    ]);

    let env = FederationEnvelope {
        kind: FederationKind::Propose, issuer_key_id: "k".into(),
        link: None, revocation: None, manifest: vec![],
    };
    let peer = PeerEndpoint("loopback".into());

    assert!(matches!(t.send(&env, &peer).await, SendOutcome::Transient(_)));
    assert!(matches!(t.send(&env, &peer).await, SendOutcome::Transient(_)));
    assert_eq!(t.send(&env, &peer).await, SendOutcome::Ack);
}

#[tokio::test]
async fn loopback_records_envelopes_for_inspection() {
    let t = LoopbackTransport::new();
    t.program([ProgrammedOutcome::Ack]);
    let env = FederationEnvelope {
        kind: FederationKind::Propose, issuer_key_id: "k".into(),
        link: None, revocation: None, manifest: vec![],
    };
    t.send(&env, &PeerEndpoint("p".into())).await;
    assert_eq!(t.sent().len(), 1);
}
```

- [ ] **Step 2 — Run; verify failure**.

- [ ] **Step 3 — Create the adapter** at `crates/cairn-test-fixtures/src/federation.rs`:

```rust
//! In-memory `FederationTransport` for tests.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cairn_core::contract::federation_transport::{FederationTransport, SendOutcome, TransportReason};
use cairn_core::domain::federation::{FederationEnvelope, PeerEndpoint};

#[derive(Debug, Clone)]
pub enum ProgrammedOutcome {
    Ack,
    Transient(String),
    Permanent(String),
}

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
    #[must_use]
    pub fn new() -> Self { Self::default() }

    pub fn program(&self, outcomes: impl IntoIterator<Item = ProgrammedOutcome>) {
        let mut g = self.inner.lock().expect("poisoned");
        g.queued.extend(outcomes);
    }

    #[must_use]
    pub fn sent(&self) -> Vec<(FederationEnvelope, PeerEndpoint)> {
        self.inner.lock().expect("poisoned").sent.clone()
    }
}

#[async_trait]
impl FederationTransport for LoopbackTransport {
    async fn send(&self, env: &FederationEnvelope, peer: &PeerEndpoint) -> SendOutcome {
        let mut g = self.inner.lock().expect("poisoned");
        g.sent.push((env.clone(), peer.clone()));
        match g.queued.first().cloned() {
            None => SendOutcome::Ack,
            Some(o) => {
                g.queued.remove(0);
                match o {
                    ProgrammedOutcome::Ack => SendOutcome::Ack,
                    ProgrammedOutcome::Transient(r) => SendOutcome::Transient(TransportReason(r)),
                    ProgrammedOutcome::Permanent(r) => SendOutcome::Permanent(TransportReason(r)),
                }
            }
        }
    }
}
```

- [ ] **Step 4 — Add to `crates/cairn-test-fixtures/src/lib.rs`**:

```rust
pub mod federation;
```

- [ ] **Step 5 — Run the tests**:

```bash
cargo nextest run -p cairn-test-fixtures --no-fail-fast
```

Expected: PASS.

- [ ] **Step 6 — Commit**:

```bash
git add crates/cairn-test-fixtures
git commit -m "feat(test-fixtures): add LoopbackTransport for federation tests"
```

---

## Task 11 — Propagation job payloads

**Files:**
- Create: `crates/cairn-workflows/src/propagation/mod.rs`
- Create: `crates/cairn-workflows/src/propagation/payload.rs`
- Modify: `crates/cairn-workflows/src/lib.rs`

- [ ] **Step 1 — Write the failing test** at `crates/cairn-workflows/tests/propagation_payload.rs`:

```rust
use cairn_workflows::propagation::payload::{PropagationJob, OutboundSharePayload, OutboundRevokePayload};

#[test]
fn payload_roundtrips_through_serde_json() {
    let job = PropagationJob::OutboundShare(OutboundSharePayload {
        operation_id: "op1".into(),
        link_id: "lk1".into(),
        peer: "loopback://receiver".into(),
        attempts: 0,
    });
    let bytes = serde_json::to_vec(&job).unwrap();
    let round: PropagationJob = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(job, round);
}

#[test]
fn outbound_revoke_distinct_from_outbound_share() {
    let r = PropagationJob::OutboundRevoke(OutboundRevokePayload {
        operation_id: "op2".into(),
        link_id: "lk2".into(),
        peer: "loopback://receiver".into(),
        attempts: 0,
    });
    let s = PropagationJob::OutboundShare(OutboundSharePayload {
        operation_id: "op1".into(),
        link_id: "lk1".into(),
        peer: "loopback://receiver".into(),
        attempts: 0,
    });
    assert_ne!(r, s);
}
```

- [ ] **Step 2 — Run; verify failure**.

- [ ] **Step 3 — Create the payload module** at `crates/cairn-workflows/src/propagation/payload.rs`:

```rust
//! `PropagationWorkflow` job payloads.

use serde::{Deserialize, Serialize};

/// Discriminated union over the two propagation job kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PropagationJob {
    OutboundShare(OutboundSharePayload),
    OutboundRevoke(OutboundRevokePayload),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundSharePayload {
    pub operation_id: String,
    pub link_id: String,
    pub peer: String,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundRevokePayload {
    pub operation_id: String,
    pub link_id: String,
    pub peer: String,
    pub attempts: u32,
}

/// Stable `JobKind` string for the outbound-share queue.
pub const OUTBOUND_SHARE_KIND: &str = "federation.propagate.outbound_share";
/// Stable `JobKind` string for the outbound-revoke queue.
pub const OUTBOUND_REVOKE_KIND: &str = "federation.propagate.outbound_revoke";
```

- [ ] **Step 4 — Create `crates/cairn-workflows/src/propagation/mod.rs`**:

```rust
pub mod payload;
pub mod handler;
pub mod trigger;
```

- [ ] **Step 5 — Add to `crates/cairn-workflows/src/lib.rs`**:

```rust
pub mod propagation;
```

- [ ] **Step 6 — Run the tests**:

```bash
cargo nextest run -p cairn-workflows --test propagation_payload --no-fail-fast
```

Expected: PASS.

- [ ] **Step 7 — Commit**:

```bash
git add crates/cairn-workflows/src/propagation crates/cairn-workflows/src/lib.rs crates/cairn-workflows/tests/propagation_payload.rs
git commit -m "feat(workflows): add propagation job payloads"
```

---

## Task 12 — `ConsentLookup` storage methods

**Files:**
- Modify: `crates/cairn-core/src/contract/consent_lookup.rs`
- Modify: `crates/cairn-store-sqlite/src/...` (find the `ConsentLookup` impl)
- Create: `crates/cairn-store-sqlite/tests/federation_consent_lookup.rs`

- [ ] **Step 1 — Locate** the existing `ConsentLookup` impl in `cairn-store-sqlite`:

```bash
rg -n "impl.*ConsentLookup" crates/cairn-store-sqlite/src/
```

- [ ] **Step 2 — Write the failing tests** at `crates/cairn-store-sqlite/tests/federation_consent_lookup.rs`:

```rust
use cairn_core::contract::consent_lookup::ConsentLookup;
use cairn_store_sqlite::SqliteStore; // or the equivalent type

#[tokio::test]
async fn find_federation_accept_returns_none_before_any_accept() {
    let store = SqliteStore::in_memory().await.unwrap();
    let lookup = store.consent_lookup();
    let result = lookup.find_federation_accept(("k1", "lk1", "nonce1")).await;
    assert!(result.is_none());
}

#[tokio::test]
async fn find_federation_accept_returns_existing_after_insert() {
    let store = SqliteStore::in_memory().await.unwrap();
    let lookup = store.consent_lookup();
    // Insert a FederationAccept ConsentEvent directly.
    store.append_consent_event(/* …FederationAccept(k1, lk1, nonce1, records=["r1","r2"])… */).await.unwrap();
    let result = lookup.find_federation_accept(("k1", "lk1", "nonce1")).await.unwrap();
    assert_eq!(result.applied_records, vec!["r1".to_string(), "r2".into()]);
}

#[tokio::test]
async fn is_link_revoked_flips_after_revoke_event() {
    let store = SqliteStore::in_memory().await.unwrap();
    let lookup = store.consent_lookup();
    assert!(!lookup.is_link_revoked("lk1").await);
    store.append_consent_event(/* FederationRevoke for lk1 */).await.unwrap();
    assert!(lookup.is_link_revoked("lk1").await);
}

#[tokio::test]
async fn find_share_link_returns_payload_after_grant_event() {
    let store = SqliteStore::in_memory().await.unwrap();
    let lookup = store.consent_lookup();
    store.append_consent_event(/* FederationGrant with full ShareLinkPayload for lk1 */).await.unwrap();
    let payload = lookup.find_share_link("lk1").await.unwrap();
    assert_eq!(payload.link_id, "lk1");
}
```

- [ ] **Step 3 — Run; verify failure**.

- [ ] **Step 4 — Extend the trait** in `crates/cairn-core/src/contract/consent_lookup.rs`:

```rust
pub trait ConsentLookup: Send + Sync {
    // ... existing methods ...

    /// Returns the prior `FederationAccept` outcome for `(issuer_key_id, link_id, nonce)`,
    /// or `None` if no Accept has been recorded.
    async fn find_federation_accept(
        &self,
        dedup: (&str, &str, &str),
    ) -> Option<FederationAcceptRecord>;

    /// Returns `true` if a `FederationRevoke` `ConsentEvent` exists for `link_id`.
    async fn is_link_revoked(&self, link_id: &str) -> bool;

    /// Returns the `ShareLinkPayload` recorded under a `FederationGrant` event,
    /// or `None` if none exists.
    async fn find_share_link(&self, link_id: &str) -> Option<StoredShareLink>;

    /// Returns the prior `FederationRevoke` outcome for `link_id`, or `None`.
    async fn find_revocation(&self, link_id: &str) -> Option<StoredRevocation>;
}

#[derive(Debug, Clone)]
pub struct FederationAcceptRecord {
    pub operation_id: String,
    pub applied_records: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StoredShareLink {
    pub link_id: String,
    pub payload: crate::domain::sharing::ShareLinkPayload,
    pub peer: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoredRevocation {
    pub operation_id: String,
    pub revocation: crate::domain::federation::SignedRevocation,
}
```

- [ ] **Step 5 — Implement** the four methods in `cairn-store-sqlite` by querying the existing `consent_journal` table filtered by the new `ConsentEventKind` variants.

- [ ] **Step 6 — Run the tests**:

```bash
cargo nextest run -p cairn-store-sqlite --test federation_consent_lookup --no-fail-fast
```

Expected: PASS.

- [ ] **Step 7 — Commit**:

```bash
git add crates/cairn-core/src/contract/consent_lookup.rs crates/cairn-store-sqlite
git commit -m "feat(store): add federation ConsentLookup methods"
```

---

## Task 13 — `PropagationHandler`

**Files:**
- Create: `crates/cairn-workflows/src/propagation/handler.rs`
- Create: `crates/cairn-workflows/src/propagation/trigger.rs`
- Create: `crates/cairn-workflows/tests/propagation_handler.rs`

- [ ] **Step 1 — Read** `crates/cairn-workflows/src/consolidation/handler.rs` to see how an existing `JobHandler` is structured.

- [ ] **Step 2 — Write the failing test** at `crates/cairn-workflows/tests/propagation_handler.rs`:

```rust
use std::sync::Arc;

use cairn_core::contract::federation_transport::SendOutcome;
use cairn_workflows::propagation::handler::PropagationHandler;
use cairn_workflows::propagation::payload::{OutboundSharePayload, PropagationJob};
use cairn_workflows::scheduler::handler::{HandlerOutcome, JobHandler};
use cairn_test_fixtures::federation::{LoopbackTransport, ProgrammedOutcome};

#[tokio::test]
async fn ack_yields_done() {
    let transport = Arc::new(LoopbackTransport::new());
    transport.program([ProgrammedOutcome::Ack]);
    let handler = PropagationHandler::new(transport.clone(), test_store(), test_consent());

    let payload = serde_json::to_vec(&PropagationJob::OutboundShare(OutboundSharePayload {
        operation_id: "op1".into(),
        link_id: "lk1".into(),
        peer: "loopback".into(),
        attempts: 0,
    })).unwrap();

    let outcome = handler.handle(&payload).await;
    assert_eq!(outcome, HandlerOutcome::Done);
}

#[tokio::test]
async fn transient_yields_retry_with_transient_class() {
    let transport = Arc::new(LoopbackTransport::new());
    transport.program([ProgrammedOutcome::Transient("net".into())]);
    let handler = PropagationHandler::new(transport, test_store(), test_consent());

    let payload = /* same OutboundSharePayload */;
    let outcome = handler.handle(&payload).await;
    assert!(matches!(outcome, HandlerOutcome::Retry { class: cairn_core::contract::job_store::FailureClass::Transient, .. }));
}

#[tokio::test]
async fn permanent_yields_permanent_failure() {
    let transport = Arc::new(LoopbackTransport::new());
    transport.program([ProgrammedOutcome::Permanent("rejected".into())]);
    let handler = PropagationHandler::new(transport, test_store(), test_consent());

    let payload = /* same OutboundSharePayload */;
    let outcome = handler.handle(&payload).await;
    assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));
}
```

(Helpers `test_store()` and `test_consent()` build minimal in-memory `MemoryStore` + `ConsentLookup` impls and pre-seed a `FederationGrant` event for `lk1`. Use existing `cairn-store-sqlite::SqliteStore::in_memory()`.)

- [ ] **Step 3 — Run; verify failure**.

- [ ] **Step 4 — Create the handler** at `crates/cairn-workflows/src/propagation/handler.rs`:

```rust
//! `PropagationHandler` — drains the federation outbound queue.

use std::sync::Arc;

use async_trait::async_trait;
use cairn_core::contract::consent_lookup::ConsentLookup;
use cairn_core::contract::federation_transport::{FederationTransport, SendOutcome};
use cairn_core::contract::job_store::{FailureClass, JobKind, JobPayload};
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::federation::{FederationEnvelope, FederationKind, PeerEndpoint};

use crate::scheduler::handler::{HandlerOutcome, JobHandler};

use super::payload::{
    OutboundRevokePayload, OutboundSharePayload, PropagationJob, OUTBOUND_REVOKE_KIND,
    OUTBOUND_SHARE_KIND,
};

pub struct PropagationHandler {
    transport: Arc<dyn FederationTransport>,
    store: Arc<dyn MemoryStore>,
    consent: Arc<dyn ConsentLookup>,
    kind: JobKind,
}

impl PropagationHandler {
    /// Build the outbound-share handler.
    #[must_use]
    pub fn outbound_share(
        transport: Arc<dyn FederationTransport>,
        store: Arc<dyn MemoryStore>,
        consent: Arc<dyn ConsentLookup>,
    ) -> Self {
        Self { transport, store, consent, kind: JobKind::new(OUTBOUND_SHARE_KIND) }
    }

    /// Build the outbound-revoke handler.
    #[must_use]
    pub fn outbound_revoke(
        transport: Arc<dyn FederationTransport>,
        store: Arc<dyn MemoryStore>,
        consent: Arc<dyn ConsentLookup>,
    ) -> Self {
        Self { transport, store, consent, kind: JobKind::new(OUTBOUND_REVOKE_KIND) }
    }
}

#[async_trait]
impl JobHandler for PropagationHandler {
    fn kind(&self) -> JobKind {
        self.kind.clone()
    }

    async fn handle(&self, payload: &JobPayload) -> HandlerOutcome {
        let job: PropagationJob = match serde_json::from_slice(payload) {
            Ok(j) => j,
            Err(e) => return HandlerOutcome::validation_permanent(format!("bad payload: {e}")),
        };

        let envelope = match build_envelope_for(&job, &*self.store, &*self.consent).await {
            Ok(env) => env,
            Err(e) => return HandlerOutcome::validation_permanent(e),
        };

        let peer = PeerEndpoint(match &job {
            PropagationJob::OutboundShare(p) => p.peer.clone(),
            PropagationJob::OutboundRevoke(p) => p.peer.clone(),
        });

        match self.transport.send(&envelope, &peer).await {
            SendOutcome::Ack => HandlerOutcome::Done,
            SendOutcome::Transient(reason) => HandlerOutcome::Retry {
                reason: reason.0,
                class: FailureClass::Transient,
            },
            SendOutcome::Permanent(reason) => HandlerOutcome::Permanent {
                reason: reason.0,
                class: FailureClass::Validation,
            },
        }
    }
}

async fn build_envelope_for(
    job: &PropagationJob,
    store: &dyn MemoryStore,
    consent: &dyn ConsentLookup,
) -> Result<FederationEnvelope, String> {
    match job {
        PropagationJob::OutboundShare(p) => {
            let stored = consent.find_share_link(&p.link_id).await
                .ok_or_else(|| format!("unknown link {}", p.link_id))?;
            let manifest = store.bulk_fetch_for_share(&stored.payload).await
                .map_err(|e| format!("manifest: {e}"))?;
            Ok(FederationEnvelope {
                kind: FederationKind::Propose,
                issuer_key_id: stored.payload.human_identity.key_id().to_string(),
                link: Some(stored.into_signed_share_link()), // helper on StoredShareLink
                revocation: None,
                manifest,
            })
        }
        PropagationJob::OutboundRevoke(p) => {
            let stored = consent.find_revocation(&p.link_id).await
                .ok_or_else(|| format!("no revocation for {}", p.link_id))?;
            Ok(FederationEnvelope {
                kind: FederationKind::Revoke,
                issuer_key_id: stored.revocation.issuer.key_id().to_string(),
                link: None,
                revocation: Some(stored.revocation),
                manifest: vec![],
            })
        }
    }
}
```

- [ ] **Step 5 — Create the trigger helper** at `crates/cairn-workflows/src/propagation/trigger.rs`:

```rust
//! Helpers to enqueue outbound jobs atomically from the verb path.

use cairn_core::contract::job_store::{JobKind, JobStore};

use super::payload::{
    OutboundRevokePayload, OutboundSharePayload, PropagationJob, OUTBOUND_REVOKE_KIND,
    OUTBOUND_SHARE_KIND,
};

/// Enqueue an outbound-share job. Returns the `operation_id` recorded.
pub async fn enqueue_outbound_share(
    jobs: &dyn JobStore,
    op_id: &str,
    link_id: &str,
    peer: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let payload = PropagationJob::OutboundShare(OutboundSharePayload {
        operation_id: op_id.into(),
        link_id: link_id.into(),
        peer: peer.into(),
        attempts: 0,
    });
    let bytes = serde_json::to_vec(&payload)?;
    jobs.enqueue(JobKind::new(OUTBOUND_SHARE_KIND), op_id, bytes).await?;
    Ok(())
}

/// Enqueue an outbound-revoke job.
pub async fn enqueue_outbound_revoke(
    jobs: &dyn JobStore,
    op_id: &str,
    link_id: &str,
    peer: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let payload = PropagationJob::OutboundRevoke(OutboundRevokePayload {
        operation_id: op_id.into(),
        link_id: link_id.into(),
        peer: peer.into(),
        attempts: 0,
    });
    let bytes = serde_json::to_vec(&payload)?;
    jobs.enqueue(JobKind::new(OUTBOUND_REVOKE_KIND), op_id, bytes).await?;
    Ok(())
}
```

(Adjust the exact `JobStore::enqueue` signature to whatever the existing trait defines — look at `crates/cairn-core/src/contract/job_store/mod.rs` and call from one of the existing workflow `trigger.rs` files for the canonical call pattern.)

- [ ] **Step 6 — Wire propose/revoke verbs to use the trigger helpers**: update `verbs::propose_share` and `verbs::revoke_share` so step 5 of each verb calls `propagation::trigger::enqueue_outbound_share` / `enqueue_outbound_revoke` inside the WAL tx instead of an inline `JobStore::enqueue`.

(Cross-crate dep note: `cairn-workflows` already depends on `cairn-core`, not the other way around. Move the `trigger.rs` helpers to `cairn-core` if the verb modules can't import them — they are pure functions over `JobStore`, so they fit in core.)

- [ ] **Step 7 — Run the handler tests**:

```bash
cargo nextest run -p cairn-workflows --test propagation_handler --no-fail-fast
```

Expected: PASS.

- [ ] **Step 8 — Commit**:

```bash
git add crates/cairn-workflows/src/propagation crates/cairn-workflows/tests/propagation_handler.rs crates/cairn-core/src/verbs/propose_share.rs crates/cairn-core/src/verbs/revoke_share.rs
git commit -m "feat(workflows): add PropagationHandler + trigger helpers"
```

---

## Task 14 — End-to-end integration: propose → accept → record visible

**Files:**
- Create: `crates/cairn-workflows/tests/propagation_e2e.rs`

- [ ] **Step 1 — Write the failing test**:

```rust
//! End-to-end: two in-memory stores connected by LoopbackTransport.

use std::sync::Arc;

use cairn_core::domain::MemoryVisibility;
use cairn_core::verbs::accept_share::{accept_share, AcceptShareRequest, AcceptOutcome};
use cairn_core::verbs::propose_share::{propose_share, ProposeShareRequest};
use cairn_test_fixtures::federation::{LoopbackTransport, ProgrammedOutcome};
use cairn_workflows::propagation::handler::PropagationHandler;
use cairn_workflows::scheduler::handler::JobHandler;

fn build_pair() -> (TestNode, TestNode, Arc<LoopbackTransport>) { /* … */ }

#[tokio::test]
async fn propose_then_drain_then_accept_makes_record_visible_on_receiver() {
    let (issuer, receiver, transport) = build_pair();
    transport.program([ProgrammedOutcome::Ack]);

    // 1. Issuer mints a propose.
    let record = issuer.upsert_record(MemoryVisibility::Project).await;
    let propose = propose_share(
        ProposeShareRequest {
            record_ids: vec![record.id.clone()],
            grantee: Some(receiver.identity()),
            scope: issuer.scope(),
            grant_tier: MemoryVisibility::Team,
            expires_at: issuer.in_one_hour(),
            peer: Some(receiver.peer_endpoint()),
        },
        &issuer.deps(),
    ).await.unwrap();

    // 2. Scheduler drains the outbound queue.
    let handler = PropagationHandler::outbound_share(
        transport.clone() as Arc<dyn _>,
        issuer.store_arc(),
        issuer.consent_arc(),
    );
    let job_payload = issuer.next_pending_payload("federation.propagate.outbound_share").await;
    let outcome = handler.handle(&job_payload).await;
    assert!(matches!(outcome, cairn_workflows::scheduler::handler::HandlerOutcome::Done));

    // 3. Receiver applies the envelope via accept_share.
    let (envelope, _peer) = transport.sent().into_iter().next().unwrap();
    let accepted = accept_share(AcceptShareRequest { envelope }, &receiver.deps()).await.unwrap();
    assert_eq!(accepted.outcome, AcceptOutcome::Accepted);

    // 4. Record visible on receiver with capped tier + share-link provenance.
    let projected = receiver.fetch(&record.id).await;
    assert_eq!(projected.visibility, MemoryVisibility::Team);
    assert!(matches!(projected.provenance.source, cairn_core::domain::record::ProvenanceSource::ShareLink { .. }));
}

#[tokio::test]
async fn transient_retry_eventually_succeeds_with_one_applied_record() {
    let (issuer, receiver, transport) = build_pair();
    transport.program([
        ProgrammedOutcome::Transient("net".into()),
        ProgrammedOutcome::Transient("net".into()),
        ProgrammedOutcome::Ack,
    ]);

    issuer.mint_share_to(&receiver).await;
    let handler = PropagationHandler::outbound_share(transport.clone() as Arc<dyn _>, issuer.store_arc(), issuer.consent_arc());
    let payload = issuer.next_pending_payload(/* … */).await;

    let r1 = handler.handle(&payload).await; // Transient
    let r2 = handler.handle(&payload).await; // Transient
    let r3 = handler.handle(&payload).await; // Ack

    assert!(matches!(r1, HandlerOutcome::Retry { .. }));
    assert!(matches!(r2, HandlerOutcome::Retry { .. }));
    assert!(matches!(r3, HandlerOutcome::Done));

    // Drive all sent envelopes through the receiver's accept_share.
    for (env, _) in transport.sent() {
        let _ = accept_share(AcceptShareRequest { envelope: env }, &receiver.deps()).await.unwrap();
    }

    // Exactly one accepted record on the receiver.
    assert_eq!(receiver.count_records_with_share_link_provenance().await, 1);
}

#[tokio::test]
async fn permanent_send_failure_marks_job_dead_after_one_attempt() {
    let (issuer, _receiver, transport) = build_pair();
    transport.program([ProgrammedOutcome::Permanent("rebac denied".into())]);
    issuer.mint_share_to(/* … */).await;
    let handler = PropagationHandler::outbound_share(transport.clone() as Arc<dyn _>, issuer.store_arc(), issuer.consent_arc());
    let payload = issuer.next_pending_payload(/* … */).await;

    let r = handler.handle(&payload).await;
    assert!(matches!(r, HandlerOutcome::Permanent { .. }));
}
```

- [ ] **Step 2 — Run; verify failure**.

- [ ] **Step 3 — Implement the `TestNode` helper** at the top of the test file. It bundles a `SqliteStore::in_memory()`, the `JobStore`, `ConsentLookup`, `Keystore`, `RebacContext` for one principal. Reuse helpers from `cairn-test-fixtures`.

- [ ] **Step 4 — Run the tests**:

```bash
cargo nextest run -p cairn-workflows --test propagation_e2e --no-fail-fast
```

Expected: all three PASS.

- [ ] **Step 5 — Commit**:

```bash
git add crates/cairn-workflows/tests/propagation_e2e.rs
git commit -m "test(workflows): e2e propose→accept + retry + permanent"
```

---

## Task 15 — Revoke end-to-end test

**Files:**
- Modify: `crates/cairn-workflows/tests/propagation_e2e.rs` (append) **or** create `crates/cairn-workflows/tests/propagation_revoke_e2e.rs`

- [ ] **Step 1 — Write the failing test**:

```rust
#[tokio::test]
async fn revoke_propagates_and_tombstones_receiver_projection() {
    let (issuer, receiver, transport) = build_pair();
    transport.program([ProgrammedOutcome::Ack, ProgrammedOutcome::Ack]);

    // Mint, drain, accept.
    let propose = issuer.mint_share_to(&receiver).await;
    drive_one_send(&issuer, &transport).await;
    apply_first_sent(&receiver, &transport).await;

    // Revoke.
    let _ = cairn_core::verbs::revoke_share::revoke_share(
        cairn_core::verbs::revoke_share::RevokeShareRequest { link_id: propose.link.link_id.clone() },
        &issuer.deps(),
    ).await.unwrap();

    // Drain the revoke job.
    let payload = issuer.next_pending_payload("federation.propagate.outbound_revoke").await;
    let handler = PropagationHandler::outbound_revoke(
        transport.clone() as Arc<dyn _>, issuer.store_arc(), issuer.consent_arc(),
    );
    let outcome = handler.handle(&payload).await;
    assert!(matches!(outcome, HandlerOutcome::Done));

    // Receiver applies the revoke envelope.
    let (revoke_env, _) = transport.sent().into_iter().nth(1).unwrap();
    let _ = cairn_core::verbs::accept_share::accept_share(
        cairn_core::verbs::accept_share::AcceptShareRequest { envelope: revoke_env },
        &receiver.deps(),
    ).await.unwrap();

    // Receiver's projection is tombstoned.
    let stored = receiver.try_fetch(&propose.applied_records[0]).await;
    assert!(stored.is_none(), "record should be tombstoned");

    // consent_journal on receiver shows Grant + Accept + Revoke + Forget rows.
    let kinds = receiver.consent_event_kinds_for_link(&propose.link.link_id).await;
    assert!(kinds.contains(&ConsentEventKind::FederationAccept));
    assert!(kinds.contains(&ConsentEventKind::FederationRevoke));
}
```

- [ ] **Step 2 — Run; verify failure** (likely fails on `MemoryStore::find_records_by_share_link` or `apply_revoke_tombstones`).

- [ ] **Step 3 — Implement** any missing primitives: `MemoryStore::find_records_by_share_link(link_id)` (adds an index lookup on the provenance column) plus the tombstone fan-out used by `accept_revoke` in Task 8. Reuse the existing `forget --record` Phase A function — search for it in `crates/cairn-core/src/verbs/`.

- [ ] **Step 4 — Run**:

```bash
cargo nextest run -p cairn-workflows --test propagation_e2e --no-fail-fast
```

Expected: PASS.

- [ ] **Step 5 — Commit**:

```bash
git add crates/cairn-workflows/tests/propagation_e2e.rs crates/cairn-core crates/cairn-store-sqlite
git commit -m "test(workflows): e2e revoke tombstones receiver projection"
```

---

## Task 16 — Property tests: idempotency, retry safety, revoke ordering

**Files:**
- Create: `crates/cairn-core/tests/federation_idempotency.rs`

- [ ] **Step 1 — Write the property tests**:

```rust
use proptest::prelude::*;
use proptest::test_runner::TestRunner;

use cairn_core::verbs::accept_share::{accept_share, AcceptShareRequest, AcceptOutcome};

mod common;
use common::*;

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    #[test]
    fn accept_share_is_idempotent_under_arbitrary_replay(
        replay_count in 1usize..=10,
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ctx = TestCtx::new_receiver().await;
            let envelope = ctx.build_signed_propose_envelope_with_one_record();

            let mut outcomes = Vec::new();
            for _ in 0..replay_count {
                let r = accept_share(AcceptShareRequest { envelope: envelope.clone() }, &ctx.deps()).await.unwrap();
                outcomes.push(r.outcome);
            }

            prop_assert_eq!(outcomes[0], AcceptOutcome::Accepted);
            for o in &outcomes[1..] {
                prop_assert_eq!(*o, AcceptOutcome::Duplicate);
            }

            // Exactly one FederationAccept event in the consent_journal.
            let accepts = ctx.consent_events_for_envelope(&envelope).await.iter()
                .filter(|e| matches!(e.kind, ConsentEventKind::FederationAccept)).count();
            prop_assert_eq!(accepts, 1);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn propose_revoke_accept_ordering_holds(
        scenario in prop::sample::select(vec!["pra", "par", "rap"]),
    ) {
        // pra: propose, revoke, accept → reject with Revoked
        // par: propose, accept, revoke → accept succeeds, then receiver-side tombstone
        // rap: revoke (unknown), accept, propose → revoke = UnknownLink, accept = Revoked
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // … exercise the appropriate path …
            Ok(())
        }).unwrap();
    }
}
```

- [ ] **Step 2 — Run; verify they fail/pass appropriately** (proptest finds failures via shrinking; expected at first pass: PASS only if Task 8 is correct).

- [ ] **Step 3 — Add proptest config + commit any regression fixtures** under `crates/cairn-core/proptest-regressions/`.

- [ ] **Step 4 — Commit**:

```bash
git add crates/cairn-core/tests/federation_idempotency.rs crates/cairn-core/proptest-regressions
git commit -m "test(core): proptest idempotency + revoke ordering"
```

---

## Task 17 — MCP tool registration for `cairn.federation.v1`

**Files:**
- Create: `crates/cairn-mcp/src/extensions/federation.rs`
- Create: `crates/cairn-mcp/tests/federation_tools.rs`
- Modify: `crates/cairn-mcp/src/lib.rs`

- [ ] **Step 1 — Read** `crates/cairn-mcp/src/lib.rs` and any existing extension registration (search for `aggregate` since `cairn.aggregate.v1` is already wired) to identify the registration pattern.

- [ ] **Step 2 — Write the failing test** at `crates/cairn-mcp/tests/federation_tools.rs`:

```rust
use cairn_mcp::test_helpers::build_server_with_federation;

#[tokio::test]
async fn federation_tools_register_three_verbs() {
    let server = build_server_with_federation();
    let tools = server.list_tools().await;
    let names: Vec<_> = tools.iter().map(|t| t.name.clone()).collect();
    assert!(names.contains(&"propose_share".to_string()));
    assert!(names.contains(&"accept_share".to_string()));
    assert!(names.contains(&"revoke_share".to_string()));
}

#[tokio::test]
async fn federation_tool_schemas_match_idl() {
    let server = build_server_with_federation();
    let tools = server.list_tools().await;
    insta::assert_json_snapshot!("federation_tools", tools.iter().filter(|t| t.name.starts_with("propose_share") || t.name.starts_with("accept_share") || t.name.starts_with("revoke_share")).collect::<Vec<_>>());
}
```

- [ ] **Step 3 — Run; verify failure**.

- [ ] **Step 4 — Create** `crates/cairn-mcp/src/extensions/federation.rs` mirroring the aggregate/admin extension pattern. Each tool delegates to the corresponding `cairn-core` verb function. Inputs/outputs use the generated types from Task 1.

- [ ] **Step 5 — Register the extension** in `crates/cairn-mcp/src/lib.rs` gated by `wiring::federation_extension_ready()`.

- [ ] **Step 6 — Run the tests**:

```bash
cargo nextest run -p cairn-mcp --test federation_tools --no-fail-fast
```

Expected: PASS. If snapshot test creates a new file, run `cargo insta review`, accept, and commit.

- [ ] **Step 7 — Commit**:

```bash
git add crates/cairn-mcp crates/cairn-mcp/tests/snapshots
git commit -m "feat(mcp): register cairn.federation.v1 tools (gated)"
```

---

## Task 18 — `cairn lint` check for dead propagation jobs

**Files:**
- Modify: `crates/cairn-core/src/verbs/lint/checks/`
- Create: `crates/cairn-core/src/verbs/lint/checks/federation.rs`
- Create: `crates/cairn-core/tests/lint_federation_dead.rs`

- [ ] **Step 1 — Read** an existing lint check (e.g. `consent.rs`) for shape.

- [ ] **Step 2 — Write the failing test** at `crates/cairn-core/tests/lint_federation_dead.rs`:

```rust
use cairn_core::verbs::lint;

#[tokio::test]
async fn lint_surfaces_dead_propagation_jobs() {
    let store = build_store_with_dead_job("federation.propagate.outbound_share", "rebac denied").await;
    let report = lint::run(&store).await.unwrap();
    let findings: Vec<_> = report.findings.iter()
        .filter(|f| f.kind == "federation_dead_propagation")
        .collect();
    assert_eq!(findings.len(), 1);
    assert!(findings[0].message.contains("rebac denied"));
}
```

- [ ] **Step 3 — Run; verify failure**.

- [ ] **Step 4 — Implement** the new check at `crates/cairn-core/src/verbs/lint/checks/federation.rs`. Reads `workflow_jobs` for rows with `kind LIKE 'federation.propagate.%' AND state = 'Failed'` and emits one finding per row with last_error.

Register it in the lint dispatch list (search for the check registration site in the existing lint module).

- [ ] **Step 5 — Run the test**:

```bash
cargo nextest run -p cairn-core --test lint_federation_dead --no-fail-fast
```

Expected: PASS.

- [ ] **Step 6 — Commit**:

```bash
git add crates/cairn-core/src/verbs/lint crates/cairn-core/tests/lint_federation_dead.rs
git commit -m "feat(lint): add federation_dead_propagation check"
```

---

## Task 19 — Wire the gates on + capability snapshot test

**Files:**
- Modify: `crates/cairn-core/src/status/wiring.rs`
- Modify: `crates/cairn-core/src/status/tests.rs`
- Modify: `crates/cairn-mcp/src/lib.rs` (scheduler boot registers `PropagationHandler`)

- [ ] **Step 1 — Verify** all preceding tasks land: run the full workspace:

```bash
cargo nextest run --workspace --locked --no-fail-fast
```

Expected: all green.

- [ ] **Step 2 — Flip the wiring gates** in `crates/cairn-core/src/status/wiring.rs`:

```rust
pub const FEDERATION_EXTENSION_WIRED: bool = true;
pub const FEDERATION_PROPOSE_DISPATCH_WIRED: bool = true;
pub const FEDERATION_ACCEPT_DISPATCH_WIRED: bool = true;
pub const FEDERATION_WORKFLOW_WIRED: bool = true;
pub const FEDERATION_MCP_TOOLS_WIRED: bool = true;
```

- [ ] **Step 3 — Update the scheduler boot** in `cairn-mcp` (or wherever `cairn mcp serve` registers handlers — search `rg "HandlerRegistryBuilder::new\(\)" crates/`) to add:

```rust
.with(Arc::new(PropagationHandler::outbound_share(transport.clone(), store.clone(), consent.clone())))
.with(Arc::new(PropagationHandler::outbound_revoke(transport, store, consent)))
```

(Construct `transport` from config; default to a `NullTransport` that always returns `Permanent("no transport configured")` until a real adapter ships — this preserves fail-closed semantics.)

- [ ] **Step 4 — Add the capability snapshot test** in `crates/cairn-core/src/status/tests.rs`:

```rust
#[test]
fn federation_advertised_when_all_gates_on() {
    assert!(wiring::federation_extension_ready());
    let advertised = advertise_for_phase(Phase::V0_3, /* gates ready */);
    assert!(advertised.capabilities.contains(&Capabilities::CairnMcpV1ExtensionFederation));
    let ns = advertised.extensions.iter().find(|n| n["name"] == "cairn.federation.v1").unwrap();
    assert_eq!(ns["x-cairn-since"], "v0.3");
}

#[test]
fn federation_snapshot() {
    insta::assert_json_snapshot!(advertise_for_phase(Phase::V0_3, /* all gates ready */));
}
```

- [ ] **Step 5 — Run + accept the snapshot**:

```bash
cargo nextest run -p cairn-core status:: --no-fail-fast
cargo insta review
```

- [ ] **Step 6 — Run full verification suite**:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

Expected: all green. If docgen reports a diff, run `cargo run -p cairn-cli --bin cairn-docgen -- --write` and commit the regenerated markdown under `docs/site/src/reference/generated/`.

- [ ] **Step 7 — Commit**:

```bash
git add -A
git commit -m "feat(status): wire cairn.federation.v1 end-to-end (issue #123)

Brief §12.a, §10, §19. Flips federation_extension_ready() on after
landing propose/accept/revoke verbs, PropagationHandler, MCP tools,
and lint check. Default deployment ships with a NullTransport that
fails closed — real HTTP adapter is a follow-up."
```

---

## Task 20 — Update traceability + close the gap

**Files:**
- Modify: `docs/design/traceability.md`

- [ ] **Step 1 — Read** `docs/design/traceability.md` lines 85-95 to find the §12.a row.

- [ ] **Step 2 — Update the row** to remove "transport boundary still pending":

```diff
- | §12.a Distribution model | #26, #29, #121–#123, #130–#132 | #149 (open) | ReBAC, share links, propagation, connectors, aggregate memory; transport boundary still pending. |
+ | §12.a Distribution model | #26, #29, #121–#123, #130–#132 | #149 (open) | ReBAC, share links, propagation workflow, federation protocol; HTTP transport adapter is a follow-up. |
```

- [ ] **Step 3 — Commit**:

```bash
git add docs/design/traceability.md
git commit -m "docs(traceability): mark §12.a propagation landed (#123)"
```

---

## Verification (final)

Run the full CI command set from CLAUDE.md §8. All commands must pass:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-bench --release --locked -- all
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked
cargo deny check
cargo audit --deny warnings
cargo machete
```

Issue acceptance criteria:
- [x] **Federation only sends records allowed by consent and ReBAC** — Tasks 7 (propose_share validates ReBAC + signs receipt), 13 (handler reads from authoritative consent_journal), 14 (e2e).
- [x] **Propagation is retryable, auditable, and idempotent** — Tasks 13 (retry classes), 8 (nonce dedup), 5 (ConsentEvent kinds for full audit), 16 (proptest).
- [x] **Inbound shared records preserve provenance and trust status** — Task 8 (provenance::ShareLink + trust_status = inbound_shared), Task 14 (e2e asserts visibility cap + provenance).

Verification block from the issue:
- [x] **Federation protocol fixture tests** — Task 2 (envelope round-trip), Task 17 (MCP tool schemas), Task 19 (capability snapshot).
- [x] **Propagation retry/idempotency tests** — Task 13 (handler unit), Task 14 (e2e), Task 16 (proptest).
- [x] **Consent/ReBAC blocking tests** — Task 7 (propose deny), Task 8 (accept deny), Task 14 (permanent failure path).
