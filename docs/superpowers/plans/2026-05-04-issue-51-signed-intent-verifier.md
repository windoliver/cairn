# Signed-Intent Envelope Verifier Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the placeholder `cairn_core::verifier::verify_signed_intent` with a real Ed25519-checking, JCS-canonicalising trust-boundary function that runs five ordered checks (syntactic, timestamp window, scope/identity-kind fit, key resolver lookup, signature) before any caller can mint a `VerifiedSignedIntent`.

**Architecture:** Verifier stays in `cairn-core` (I/O-free) and depends on a new tiny async `IssuerKeyResolver` contract trait. SQLite adapter (`cairn-store-sqlite`) implements the trait by wrapping the existing `IdentityRegistry`. Replay/sequence consume (#52) and WAL coupling (#55) stay out of scope — the verifier produces a `VerifiedSignedIntent` token and stops.

**Tech Stack:** Rust 2024 edition, `ed25519-dalek` 2.1, new dep `serde_jcs` for RFC 8785 canonicalization, `tokio` + `async-trait` for async, `chrono` for timestamp arithmetic, `rstest` + `proptest` + `insta` for tests.

**Spec:** `docs/design/2026-05-04-issue-51-signed-intent-verifier-design.md`
**Issue:** [#51](https://github.com/windoliver/cairn/issues/51)

---

## File Structure

**New files:**
- `crates/cairn-core/src/contract/issuer_key_resolver.rs` — `IssuerKeyResolver` trait + `ResolvedKey` + `KeyLifecycle` + `ResolverError`. Async, single method.
- `crates/cairn-core/src/intent/mod.rs` — re-export of envelope/error helpers.
- `crates/cairn-core/src/intent/canonical_envelope.rs` — `canonicalize_signed_payload`: takes `&SignedIntent`, returns JCS bytes (RFC 8785) of the envelope minus `signature`.
- `crates/cairn-core/src/intent/verify_error.rs` — `VerifyError` enum + `ExpiryReason`.
- `crates/cairn-store-sqlite/src/issuer_key_resolver.rs` — `SqliteIssuerKeyResolver` adapter wrapping `IdentityRegistry`.
- `crates/cairn-store-sqlite/tests/issuer_key_resolver.rs` — integration test: lifecycle map (active/revoked/pending/purged) → `KeyLifecycle`.
- `crates/cairn-store-sqlite/tests/no_db_write_on_bad_envelope.rs` — regression test enforcing AC #1.
- `crates/cairn-test-fixtures/src/signed_intent.rs` — `signed_intent_builder()` (`bon` derive) + `FakeIssuerKeyResolver` for tests.
- `crates/cairn-core/tests/snapshots/` — `insta` snapshots for `VerifyError::Display`.

**Modified files:**
- `Cargo.toml` (workspace root) — add `serde_jcs` to `[workspace.dependencies]`.
- `crates/cairn-core/Cargo.toml` — pull in `serde_jcs`.
- `crates/cairn-core/src/lib.rs` — add `pub mod intent;` and ensure `pub mod verifier;`.
- `crates/cairn-core/src/contract/mod.rs` — export `issuer_key_resolver`.
- `crates/cairn-core/src/verifier.rs` — replace placeholder body with the five-step pipeline, change signature to `async fn verify_signed_intent(intent, &dyn IssuerKeyResolver, now: SystemTime) -> Result<VerifiedSignedIntent, VerifyError>`.
- `crates/cairn-test-fixtures/src/lib.rs` — `pub mod signed_intent;`.
- `crates/cairn-test-fixtures/Cargo.toml` — pull in `bon` (already workspace-pinned).

**Untouched on purpose:** `cairn-keychain` (only holds private keys; verifier needs public keys, which live in the registry); the WAL machinery; `cairn-cli` / `cairn-mcp` / `cairn-sdk` adapter wiring (separate issues #52/#55 will plug the verifier into the verb dispatch path).

**File-size discipline:** verifier.rs grows from 150 LOC to ~250 LOC, still focused; canonical-envelope and verify-error split into their own modules so each file stays single-responsibility.

---

## Tasks

### Task 1: Add `serde_jcs` workspace dependency

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/cairn-core/Cargo.toml`

- [ ] **Step 1: Add `serde_jcs` to workspace dependencies**

Edit `Cargo.toml` (workspace root). After the existing `serde_json` line in `[workspace.dependencies]`, add:

```toml
serde_jcs = "0.1"
```

- [ ] **Step 2: Pull `serde_jcs` into `cairn-core`**

Edit `crates/cairn-core/Cargo.toml`. In the `[dependencies]` block (after `serde_json`), add:

```toml
serde_jcs = { workspace = true }
```

- [ ] **Step 3: Verify the workspace builds**

Run: `cargo check --workspace --locked`
Expected: PASS, no warnings.

- [ ] **Step 4: Verify `cargo deny` accepts the new license**

Run: `cargo deny check`
Expected: PASS. (`serde_jcs` is Apache-2.0; allowlist already accepts that.)

If `cargo deny` is not installed, run `cargo install cargo-deny` first.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/cairn-core/Cargo.toml
git commit -m "chore(deps): add serde_jcs for RFC 8785 canonical JSON (issue #51)"
```

---

### Task 2: Add `IssuerKeyResolver` contract trait

> **Order:** lands before `VerifyError` (Task 3) because `VerifyError::ResolverFailure(ResolverError)` references the type defined here.

**Files:**
- Create: `crates/cairn-core/src/contract/issuer_key_resolver.rs`
- Modify: `crates/cairn-core/src/contract/mod.rs`

- [ ] **Step 1: Write the failing test for the trait shape and lifecycle types**

Create `crates/cairn-core/src/contract/issuer_key_resolver.rs` with this initial test block at the bottom (will fail until the rest is added):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_lifecycle_active_construction() {
        let lc = KeyLifecycle::Active;
        assert!(matches!(lc, KeyLifecycle::Active));
    }

    #[test]
    fn key_lifecycle_revoked_carries_effective_at() {
        let lc = KeyLifecycle::Revoked { effective_at: "2026-01-01T00:00:00Z".to_owned() };
        match lc {
            KeyLifecycle::Revoked { effective_at } => assert_eq!(effective_at, "2026-01-01T00:00:00Z"),
            _ => panic!("expected revoked"),
        }
    }

    #[test]
    fn resolver_error_backend_wraps() {
        let inner: Box<dyn std::error::Error + Send + Sync> = "io error".into();
        let e = ResolverError::Backend(inner);
        assert_eq!(e.to_string(), "backend failure: io error");
    }
}
```

- [ ] **Step 2: Run test to verify it fails (types do not exist yet)**

Run: `cargo test -p cairn-core issuer_key_resolver`
Expected: compile-error (`cannot find type 'KeyLifecycle'`).

- [ ] **Step 3: Add the trait + supporting types**

Replace the contents of `crates/cairn-core/src/contract/issuer_key_resolver.rs` with:

```rust
//! `IssuerKeyResolver` — minimal trust-boundary lookup for
//! `(issuer, key_version) → public key + lifecycle`.
//!
//! Verification only consumes one fact from the identity stack: "is
//! this key still trusted at the moment the envelope was issued?" The
//! full [`crate::contract::IdentityRegistry`] surface (provisioning,
//! rotation, revocation receipts) is far broader than that — verifier
//! callers route through this narrower trait so a fake implementation
//! is trivial in tests and the SQLite adapter exposes only what the
//! verifier actually needs.

use async_trait::async_trait;
use thiserror::Error;

use crate::domain::identity::{Identity, keys::KeyVersion};

/// Single-method contract: look up the pubkey + lifecycle for an
/// `(issuer, key_version)` pair.
#[async_trait]
pub trait IssuerKeyResolver: Send + Sync {
    /// Resolve `(issuer, key_version)` to the verifying-key bytes plus
    /// the issuer's current lifecycle state.
    ///
    /// Returns `Ok(None)` when no row exists for the `(issuer,
    /// key_version)` pair — verifier maps that to
    /// [`crate::intent::VerifyError::UnknownKey`].
    ///
    /// # Errors
    /// Returns [`ResolverError::Backend`] when the underlying store
    /// (SQLite, in-memory fake) fails.
    async fn lookup(
        &self,
        issuer: &Identity,
        key_version: KeyVersion,
    ) -> Result<Option<ResolvedKey>, ResolverError>;
}

/// Output of a successful resolver lookup.
///
/// `public_key` is the raw 32-byte Ed25519 verifying key — same shape
/// as [`crate::domain::identity::records::IdentityKeyEntry::public_key`].
/// `lifecycle` is normalized down to four states the verifier cares
/// about; the registry's full state machine (Pending,
/// `RevokePending`, `PurgePending`, etc.) collapses to those four.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedKey {
    /// 32-byte Ed25519 verifying key.
    pub public_key: [u8; 32],
    /// Trust state of this `(issuer, key_version)` pair.
    pub lifecycle: KeyLifecycle,
}

/// Verifier-relevant collapse of the registry's lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyLifecycle {
    /// Active — the issuer is operational and signing is permitted.
    Active,
    /// Revoked at `effective_at` (RFC3339). Earlier ops remain valid;
    /// the verifier compares `effective_at` against `intent.issued_at`.
    Revoked {
        /// When the revocation took effect (RFC3339).
        effective_at: String,
    },
    /// Identity row exists but is in a non-operational lifecycle
    /// (`Pending`, `RevokePending`, `PurgePending`). Verifier maps to
    /// [`crate::intent::VerifyError::UnknownKey`] same as a missing
    /// row.
    NonOperational,
    /// Identity row was purged. Verifier maps to `UnknownKey`.
    Purged,
}

/// Error path from the adapter implementation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ResolverError {
    /// The backing store failed (SQLite I/O error, in-memory map
    /// poisoned, etc.).
    #[error("backend failure: {0}")]
    Backend(Box<dyn std::error::Error + Send + Sync>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_lifecycle_active_construction() {
        let lc = KeyLifecycle::Active;
        assert!(matches!(lc, KeyLifecycle::Active));
    }

    #[test]
    fn key_lifecycle_revoked_carries_effective_at() {
        let lc = KeyLifecycle::Revoked { effective_at: "2026-01-01T00:00:00Z".to_owned() };
        match lc {
            KeyLifecycle::Revoked { effective_at } => assert_eq!(effective_at, "2026-01-01T00:00:00Z"),
            _ => panic!("expected revoked"),
        }
    }

    #[test]
    fn resolver_error_backend_wraps() {
        let inner: Box<dyn std::error::Error + Send + Sync> = "io error".into();
        let e = ResolverError::Backend(inner);
        assert_eq!(e.to_string(), "backend failure: io error");
    }
}
```

- [ ] **Step 4: Wire the module into `contract/mod.rs`**

Edit `crates/cairn-core/src/contract/mod.rs`. Add (alphabetical with the other module declarations):

```rust
pub mod issuer_key_resolver;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cairn-core issuer_key_resolver`
Expected: 3 tests pass.

- [ ] **Step 6: Boundary check**

Run: `./scripts/check-core-boundary.sh`
Expected: PASS — no new workspace-crate dependencies in `cairn-core`.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/contract
git commit -m "feat(contract): add IssuerKeyResolver trait + ResolvedKey + KeyLifecycle (issue #51)"
```

---

### Task 3: Add `VerifyError` enum

**Files:**
- Create: `crates/cairn-core/src/intent/mod.rs`
- Create: `crates/cairn-core/src/intent/verify_error.rs`
- Modify: `crates/cairn-core/src/lib.rs`

- [ ] **Step 1: Add the test for VerifyError variants and Display output**

Write the test inline in the module (will land alongside the enum in step 3, kept in one block here for the engineer to copy in the order they will land):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::{Identity, IdentityKind, keys::KeyVersion};
    use crate::generated::envelope::SignedIntentScopeTier;
    use std::num::NonZeroU32;

    #[test]
    fn malformed_display() {
        let e = VerifyError::Malformed { field: "issuer", reason: "bad prefix".to_owned() };
        assert_eq!(e.to_string(), "malformed envelope: issuer: bad prefix");
    }

    #[test]
    fn expired_intent_display_kind_skewed() {
        let e = VerifyError::ExpiredIntent {
            issued_at: "2026-01-01T00:00:00Z".to_owned(),
            expires_at: "2026-01-01T00:05:00Z".to_owned(),
            now: "2026-01-01T01:00:00Z".to_owned(),
            kind: ExpiryReason::Skewed,
        };
        let s = e.to_string();
        assert!(s.contains("Skewed"));
        assert!(s.contains("issued_at"));
    }

    #[test]
    fn scope_denied_display() {
        let e = VerifyError::ScopeDenied {
            issuer_kind: IdentityKind::Agent,
            requested_tier: SignedIntentScopeTier::Team,
        };
        assert!(e.to_string().contains("Agent"));
        assert!(e.to_string().contains("Team"));
    }

    #[test]
    fn unknown_key_display() {
        let e = VerifyError::UnknownKey {
            issuer: Identity::parse("hmn:alice").expect("parse"),
            key_version: KeyVersion::FIRST,
        };
        assert!(e.to_string().contains("hmn:alice"));
    }

    #[test]
    fn invalid_signature_display_is_opaque() {
        let e = VerifyError::InvalidSignature;
        assert_eq!(e.to_string(), "invalid signature");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail (module does not yet exist)**

Run: `cargo test -p cairn-core verify_error`
Expected: compile-error (`unresolved import` for `crate::intent::verify_error`).

- [ ] **Step 3: Create the module tree and the enum**

Create `crates/cairn-core/src/intent/mod.rs`:

```rust
//! Trust-boundary verification helpers — the canonical signed payload
//! builder and the typed error enum.

pub mod canonical_envelope;
pub mod verify_error;

pub use verify_error::{ExpiryReason, VerifyError};
```

Create `crates/cairn-core/src/intent/verify_error.rs`:

```rust
//! Typed errors returned by [`crate::verifier::verify_signed_intent`].
//!
//! Separate from [`crate::domain::DomainError`] so the wire-layer's
//! `policy_trace` (§8.0.b) can map verification failures to stable codes
//! without depending on record-validation variants.

use thiserror::Error;

use crate::domain::identity::{Identity, IdentityKind, keys::KeyVersion};
use crate::generated::envelope::SignedIntentScopeTier;
use crate::contract::issuer_key_resolver::ResolverError;

/// Outcome of a single envelope verification attempt.
///
/// Variants are non-exhaustive so `Replay`, `OutOfOrderSequence`, and
/// `ChallengeMismatch` can land in #52 without breaking call sites.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VerifyError {
    /// A syntactic invariant tripped post-deserialization (defense in
    /// depth — most syntactic checks already run in
    /// `RawSignedIntent::TryFrom`).
    #[error("malformed envelope: {field}: {reason}")]
    Malformed {
        /// Static identifier of the failing field, e.g. `"issuer"`.
        field: &'static str,
        /// Human-readable detail. Never contains record bodies or
        /// signature bytes.
        reason: String,
    },

    /// A timestamp window check failed.
    #[error("expired intent ({kind:?}): issued_at={issued_at} expires_at={expires_at} now={now}")]
    ExpiredIntent {
        /// `intent.issued_at`, echoed verbatim.
        issued_at: String,
        /// `intent.expires_at`, echoed verbatim.
        expires_at: String,
        /// Server-supplied `now` formatted as RFC3339.
        now: String,
        /// Which window check failed.
        kind: ExpiryReason,
    },

    /// The issuer's identity kind is not allowed to sign envelopes at
    /// the requested tier (brief §4.2 P0 baseline).
    #[error("scope denied: issuer kind {issuer_kind:?} cannot sign tier {requested_tier:?}")]
    ScopeDenied {
        /// Parsed kind of `intent.issuer`.
        issuer_kind: IdentityKind,
        /// `intent.scope.tier` echoed back.
        requested_tier: SignedIntentScopeTier,
    },

    /// Resolver returned `None` (no key entry for that
    /// `(issuer, key_version)` pair) or a non-operational lifecycle.
    #[error("unknown key: issuer={issuer} key_version={key_version}")]
    UnknownKey {
        /// Identity that signed (or claimed to sign) the envelope.
        issuer: Identity,
        /// Requested key version.
        key_version: KeyVersion,
    },

    /// Resolver returned a revoked key whose `effective_at` is on or
    /// before `intent.issued_at`. Earlier ops remain valid (brief §4.2).
    #[error("revoked key: issuer={issuer} key_version={key_version} effective_at={effective_at}")]
    RevokedKey {
        /// Identity that issued the envelope.
        issuer: Identity,
        /// Key version that was revoked.
        key_version: KeyVersion,
        /// Revocation `effective_at` formatted as RFC3339.
        effective_at: String,
    },

    /// Ed25519 verification rejected the signature, OR canonicalization
    /// of the envelope failed. Opaque on purpose — no oracle for
    /// differential timing.
    #[error("invalid signature")]
    InvalidSignature,

    /// The resolver itself failed to talk to its backing store.
    #[error("resolver failure")]
    ResolverFailure(#[source] ResolverError),
}

/// Sub-variant of [`VerifyError::ExpiredIntent`] identifying which
/// window predicate fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpiryReason {
    /// `|now − issued_at| > 2 min`. Bounds backdating against a stolen
    /// key (brief §4.2).
    Skewed,
    /// `now > expires_at`. Receipt has aged past its TTL.
    Past,
    /// `expires_at − issued_at > max_ttl`. Caller tried to extend their
    /// own TTL (brief §4.2: "clients can't extend their own TTLs").
    TtlExceeded,
}
```

- [ ] **Step 4: Wire the new module into `lib.rs`**

In `crates/cairn-core/src/lib.rs`, add (alongside the existing `pub mod` lines, alphabetical):

```rust
pub mod intent;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cairn-core verify_error`
Expected: 5 tests pass (`malformed_display`, `expired_intent_display_kind_skewed`, `scope_denied_display`, `unknown_key_display`, `invalid_signature_display_is_opaque`).

`ResolverError` resolves cleanly because Task 2 already landed it.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/intent crates/cairn-core/src/lib.rs
git commit -m "feat(verifier): add VerifyError + ExpiryReason types (issue #51)"
```

---

### Task 4: Implement canonical-envelope JCS serializer

**Files:**
- Create: `crates/cairn-core/src/intent/canonical_envelope.rs`

- [ ] **Step 1: Write the failing tests (round-trip + signature-input determinism + signature-excluded)**

Create `crates/cairn-core/src/intent/canonical_envelope.rs`:

```rust
//! RFC 8785 (JCS) canonical-JSON serializer for the signed payload.
//!
//! Input: a parsed [`crate::generated::envelope::SignedIntent`].
//! Output: the deterministic byte sequence that
//! [`crate::verifier::verify_signed_intent`] passes to Ed25519 verify.
//!
//! Excludes the `signature` field — the signed payload covers every
//! *other* envelope field (brief §4.2: "signature is over the canonical
//! JSON of ALL fields above").

use serde_json::{Value, json};

use crate::generated::envelope::SignedIntent;
use crate::intent::VerifyError;

/// Build the canonical JSON byte representation of `intent` minus
/// `signature`. Used as the input to Ed25519 verify and as the input to
/// any future signer in `cairn-core`.
///
/// # Errors
/// Returns [`VerifyError::Malformed`] with `field = "envelope"` if
/// `serde_jcs::to_vec` fails (extremely unlikely — only on serializer
/// internal bugs).
pub fn canonicalize_signed_payload(intent: &SignedIntent) -> Result<Vec<u8>, VerifyError> {
    let mut map = serde_json::Map::new();
    map.insert("chain_parents".into(), serde_json::to_value(&intent.chain_parents).map_err(envelope_err)?);
    map.insert("expires_at".into(), Value::String(intent.expires_at.clone()));
    map.insert("issued_at".into(), Value::String(intent.issued_at.clone()));
    map.insert("issuer".into(), Value::String(intent.issuer.0.clone()));
    map.insert("key_version".into(), json!(intent.key_version));
    map.insert("nonce".into(), Value::String(intent.nonce.0.clone()));
    map.insert("operation_id".into(), Value::String(intent.operation_id.0.clone()));
    map.insert("scope".into(), serde_json::to_value(&intent.scope).map_err(envelope_err)?);
    if let Some(seq) = intent.sequence {
        map.insert("sequence".into(), json!(seq));
    }
    if let Some(c) = &intent.server_challenge {
        map.insert("server_challenge".into(), Value::String(c.0.clone()));
    }
    map.insert("target_hash".into(), Value::String(intent.target_hash.clone()));
    let value = Value::Object(map);
    serde_jcs::to_vec(&value).map_err(envelope_err)
}

fn envelope_err<E: std::fmt::Display>(e: E) -> VerifyError {
    VerifyError::Malformed { field: "envelope", reason: e.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::common;
    use crate::generated::envelope::{SignedIntentScope, SignedIntentScopeTier};

    fn good_intent() -> SignedIntent {
        SignedIntent {
            chain_parents: vec![],
            expires_at: "2026-04-22T14:07:11Z".to_owned(),
            issued_at: "2026-04-22T14:02:11Z".to_owned(),
            issuer: common::Identity("hmn:tafeng".to_owned()),
            key_version: 1,
            nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".to_owned()),
            operation_id: common::Ulid("01HQZX9F5N0000000000000000".to_owned()),
            scope: SignedIntentScope {
                tenant: "acme".to_owned(),
                workspace: "ws".to_owned(),
                entity: "ent".to_owned(),
                tier: SignedIntentScopeTier::Project,
            },
            sequence: Some(1),
            server_challenge: None,
            signature: common::Ed25519Signature(format!("ed25519:{}", "a".repeat(128))),
            target_hash: format!("sha256:{}", "a".repeat(64)),
        }
    }

    #[test]
    fn canonical_output_excludes_signature() {
        let bytes = canonicalize_signed_payload(&good_intent()).expect("ok");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        assert!(!s.contains("signature"), "canonical payload must not contain signature; got: {s}");
        assert!(!s.contains("ed25519:"));
    }

    #[test]
    fn canonical_output_is_deterministic_across_calls() {
        let a = canonicalize_signed_payload(&good_intent()).expect("a");
        let b = canonicalize_signed_payload(&good_intent()).expect("b");
        assert_eq!(a, b);
    }

    #[test]
    fn canonical_output_keys_are_sorted() {
        let bytes = canonicalize_signed_payload(&good_intent()).expect("ok");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        let chain_idx = s.find("\"chain_parents\"").expect("chain_parents present");
        let expires_idx = s.find("\"expires_at\"").expect("expires_at present");
        let target_idx = s.find("\"target_hash\"").expect("target_hash present");
        assert!(chain_idx < expires_idx, "chain_parents before expires_at");
        assert!(expires_idx < target_idx, "expires_at before target_hash");
    }

    #[test]
    fn omits_optional_fields_when_none() {
        let mut i = good_intent();
        i.sequence = None;
        i.server_challenge = Some(common::Nonce16Base64("BBBBBBBBBBBBBBBBBBBBBA==".to_owned()));
        let bytes = canonicalize_signed_payload(&i).expect("ok");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        assert!(!s.contains("\"sequence\""), "sequence absent");
        assert!(s.contains("\"server_challenge\""), "server_challenge present");
    }

    #[test]
    fn flipping_a_field_changes_canonical_bytes() {
        let baseline = canonicalize_signed_payload(&good_intent()).expect("baseline");
        let mut mutated = good_intent();
        mutated.target_hash = format!("sha256:{}", "b".repeat(64));
        let after = canonicalize_signed_payload(&mutated).expect("mutated");
        assert_ne!(baseline, after, "canonical bytes must change when target_hash changes");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cairn-core canonical_envelope`
Expected: compile-error if `serde_jcs` is missing — confirms Task 1 is wired correctly.

- [ ] **Step 3: Confirm Task 1 added the dep**

Run: `cargo tree -p cairn-core | grep serde_jcs`
Expected: `serde_jcs v0.1...` appears.

If absent, return to Task 1.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cairn-core canonical_envelope`
Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/intent/canonical_envelope.rs
git commit -m "feat(verifier): add RFC 8785 canonical-JSON payload builder (issue #51)"
```

---

### Task 5: Replace placeholder verifier — step 1 (syntactic post-parse check)

**Files:**
- Modify: `crates/cairn-core/src/verifier.rs`
- Modify: `crates/cairn-core/src/lib.rs` (if needed for re-exports)

- [ ] **Step 1: Write the failing tests for the new async signature**

Replace the contents of `crates/cairn-core/src/verifier.rs` (full rewrite — the old placeholder is leaving) with a temporary structure exposing only step 1; later tasks fill in steps 2-5.

```rust
//! `SignedIntent` verifier — the single production path that mints
//! [`crate::domain::VerifiedSignedIntent`] tokens.
//!
//! Pipeline (brief §4.2 hot-path, executed in this order to short-circuit
//! cheap rejections first):
//! 1. Syntactic post-parse defense in depth — catches direct field-init
//!    bypassing `RawSignedIntent::TryFrom`.
//! 2. Timestamp window vs server-supplied `now`.
//! 3. Issuer-kind ↔ scope-tier fit.
//! 4. Resolver lookup → public key + lifecycle.
//! 5. JCS canonicalize + Ed25519 verify.
//!
//! Replay/sequence consumption is *not* in this pipeline — it lives in
//! the SQLite WAL transaction (#52, #55).

use std::time::SystemTime;

use crate::contract::issuer_key_resolver::IssuerKeyResolver;
use crate::domain::{Identity, VerifiedSignedIntent, intent::{SignedIntentVerifier, sealed::VerifierWitness}};
use crate::generated::envelope::SignedIntent;
use crate::intent::{ExpiryReason, VerifyError};

/// Concrete verifier impl. Empty by design — the trait method is
/// default-implemented in [`SignedIntentVerifier`] and the witness is
/// what gates construction.
pub struct CoreSignedIntentVerifier;

impl SignedIntentVerifier for CoreSignedIntentVerifier {}

/// Verify a `SignedIntent` and mint a [`VerifiedSignedIntent`] token.
///
/// Five ordered checks (see module docs). Any failure short-circuits with
/// `Err(VerifyError::*)` and no side effects.
///
/// # Errors
/// Returns the appropriate [`VerifyError`] variant.
pub async fn verify_signed_intent(
    intent: SignedIntent,
    _resolver: &dyn IssuerKeyResolver,
    _now: SystemTime,
) -> Result<VerifiedSignedIntent, VerifyError> {
    step1_syntactic(&intent)?;
    // Steps 2-5 land in subsequent tasks.
    Ok(<CoreSignedIntentVerifier as SignedIntentVerifier>::__from_verified(
        intent,
        VerifierWitness::new(),
    ))
}

/// Step 1: post-parse defense in depth. Most invariants are already
/// enforced by `RawSignedIntent::TryFrom`; this re-checks the few that
/// could be bypassed by direct field-init within `cairn-core`.
fn step1_syntactic(intent: &SignedIntent) -> Result<(), VerifyError> {
    Identity::parse(intent.issuer.0.clone()).map_err(|e| VerifyError::Malformed {
        field: "issuer",
        reason: format!("{e}"),
    })?;
    if (intent.sequence.is_some() as u8 + intent.server_challenge.is_some() as u8) != 1 {
        return Err(VerifyError::Malformed {
            field: "sequence_or_challenge",
            reason: "exactly one of [sequence, server_challenge] is required".to_owned(),
        });
    }
    if intent.key_version < 1 {
        return Err(VerifyError::Malformed {
            field: "key_version",
            reason: format!("must be >= 1; got {}", intent.key_version),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::issuer_key_resolver::{KeyLifecycle, ResolvedKey, ResolverError};
    use crate::domain::identity::keys::KeyVersion;
    use crate::generated::common;
    use crate::generated::envelope::{SignedIntentScope, SignedIntentScopeTier};
    use async_trait::async_trait;

    /// Always-Ok resolver returning a fixed active pubkey. Steps 2-5
    /// are stubs in this task, so the resolver is unused — kept to
    /// match the function signature.
    struct OkResolver;

    #[async_trait]
    impl IssuerKeyResolver for OkResolver {
        async fn lookup(
            &self,
            _issuer: &Identity,
            _key_version: KeyVersion,
        ) -> Result<Option<ResolvedKey>, ResolverError> {
            Ok(Some(ResolvedKey {
                public_key: [0u8; 32],
                lifecycle: KeyLifecycle::Active,
            }))
        }
    }

    fn good_intent() -> SignedIntent {
        SignedIntent {
            chain_parents: vec![],
            expires_at: "2026-04-22T14:07:11Z".to_owned(),
            issued_at: "2026-04-22T14:02:11Z".to_owned(),
            issuer: common::Identity("hmn:tafeng".to_owned()),
            key_version: 1,
            nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".to_owned()),
            operation_id: common::Ulid("01HQZX9F5N0000000000000000".to_owned()),
            scope: SignedIntentScope {
                tenant: "acme".to_owned(),
                workspace: "ws".to_owned(),
                entity: "ent".to_owned(),
                tier: SignedIntentScopeTier::Project,
            },
            sequence: Some(1),
            server_challenge: None,
            signature: common::Ed25519Signature(format!("ed25519:{}", "a".repeat(128))),
            target_hash: format!("sha256:{}", "a".repeat(64)),
        }
    }

    #[tokio::test]
    async fn rejects_bad_issuer_identity() {
        let mut i = good_intent();
        i.issuer = common::Identity("not-a-prefix:foo".to_owned());
        let err = verify_signed_intent(i, &OkResolver, SystemTime::now())
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "issuer", .. }));
    }

    #[tokio::test]
    async fn rejects_both_sequence_and_challenge() {
        let mut i = good_intent();
        i.server_challenge = Some(common::Nonce16Base64("BBBBBBBBBBBBBBBBBBBBBA==".to_owned()));
        let err = verify_signed_intent(i, &OkResolver, SystemTime::now())
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "sequence_or_challenge", .. }));
    }

    #[tokio::test]
    async fn rejects_neither_sequence_nor_challenge() {
        let mut i = good_intent();
        i.sequence = None;
        let err = verify_signed_intent(i, &OkResolver, SystemTime::now())
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "sequence_or_challenge", .. }));
    }

    #[tokio::test]
    async fn rejects_zero_key_version() {
        let mut i = good_intent();
        i.key_version = 0;
        let err = verify_signed_intent(i, &OkResolver, SystemTime::now())
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Malformed { field: "key_version", .. }));
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p cairn-core verifier`
Expected: 4 tests pass (`rejects_bad_issuer_identity`, `rejects_both_sequence_and_challenge`, `rejects_neither_sequence_nor_challenge`, `rejects_zero_key_version`).

- [ ] **Step 3: Run `cargo clippy` and confirm no new warnings**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verifier.rs
git commit -m "feat(verifier): replace placeholder with async pipeline scaffold + step 1 (issue #51)"
```

---

### Task 6: Add timestamp window check (step 2)

**Files:**
- Modify: `crates/cairn-core/src/verifier.rs`

- [ ] **Step 1: Write the failing tests for the three `ExpiryReason` variants**

Add to `crates/cairn-core/src/verifier.rs::tests`:

```rust
    use chrono::{DateTime, Utc};

    fn rfc3339_to_systemtime(s: &str) -> SystemTime {
        DateTime::parse_from_rfc3339(s)
            .expect("rfc3339")
            .with_timezone(&Utc)
            .into()
    }

    #[tokio::test]
    async fn rejects_skewed_issued_at_in_future() {
        let mut i = good_intent();
        // issued_at is 14:02:11; set now = 13:55:11 (7min in past) → Skewed
        i.issued_at = "2026-04-22T14:02:11Z".to_owned();
        i.expires_at = "2026-04-22T14:07:11Z".to_owned();
        let now = rfc3339_to_systemtime("2026-04-22T13:55:11Z");
        let err = verify_signed_intent(i, &OkResolver, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ExpiredIntent { kind: ExpiryReason::Skewed, .. }));
    }

    #[tokio::test]
    async fn rejects_past_expires_at() {
        let mut i = good_intent();
        i.issued_at = "2026-04-22T14:02:11Z".to_owned();
        i.expires_at = "2026-04-22T14:03:11Z".to_owned();
        let now = rfc3339_to_systemtime("2026-04-22T14:10:00Z");
        let err = verify_signed_intent(i, &OkResolver, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ExpiredIntent { kind: ExpiryReason::Past, .. }));
    }

    #[tokio::test]
    async fn rejects_overlong_ttl() {
        let mut i = good_intent();
        i.issued_at = "2026-04-22T14:02:11Z".to_owned();
        // 6-min TTL > 5-min cap
        i.expires_at = "2026-04-22T14:08:11Z".to_owned();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &OkResolver, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ExpiredIntent { kind: ExpiryReason::TtlExceeded, .. }));
    }

    #[tokio::test]
    async fn accepts_window_within_bounds() {
        let i = good_intent();
        // good_intent: issued=14:02:11, expires=14:07:11 (5min flat).
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        // Steps 3-5 are stubs at this point, so this should reach the
        // stubbed Ok branch.
        verify_signed_intent(i, &OkResolver, now).await.expect("within bounds");
    }
```

- [ ] **Step 2: Run tests — they should fail because step 2 isn't wired**

Run: `cargo test -p cairn-core verifier`
Expected: the four new tests fail (the bad inputs reach the stubbed Ok branch and unexpectedly succeed).

- [ ] **Step 3: Add the timestamp-window check helper**

In `crates/cairn-core/src/verifier.rs`, after `step1_syntactic`, add:

```rust
const MAX_SKEW_SECS: i64 = 120; // ±2 min, brief §4.2
const MAX_TTL_SECS: i64 = 5 * 60; // 5 min flat at P0; brief §4.2

/// Step 2: timestamp window. `issued_at` ±2 min of `now`,
/// `expires_at − issued_at ≤ 5 min`, `now ≤ expires_at`.
fn step2_timestamp_window(intent: &SignedIntent, now: SystemTime) -> Result<(), VerifyError> {
    use chrono::{DateTime, Utc};
    let issued = DateTime::parse_from_rfc3339(&intent.issued_at)
        .map_err(|e| VerifyError::Malformed { field: "issued_at", reason: e.to_string() })?
        .with_timezone(&Utc);
    let expires = DateTime::parse_from_rfc3339(&intent.expires_at)
        .map_err(|e| VerifyError::Malformed { field: "expires_at", reason: e.to_string() })?
        .with_timezone(&Utc);
    let now_dt: DateTime<Utc> = now.into();
    let now_str = now_dt.to_rfc3339();

    let skew = (now_dt - issued).num_seconds().abs();
    if skew > MAX_SKEW_SECS {
        return Err(VerifyError::ExpiredIntent {
            issued_at: intent.issued_at.clone(),
            expires_at: intent.expires_at.clone(),
            now: now_str,
            kind: ExpiryReason::Skewed,
        });
    }
    let ttl = (expires - issued).num_seconds();
    if ttl > MAX_TTL_SECS {
        return Err(VerifyError::ExpiredIntent {
            issued_at: intent.issued_at.clone(),
            expires_at: intent.expires_at.clone(),
            now: now_str,
            kind: ExpiryReason::TtlExceeded,
        });
    }
    if now_dt >= expires {
        return Err(VerifyError::ExpiredIntent {
            issued_at: intent.issued_at.clone(),
            expires_at: intent.expires_at.clone(),
            now: now_str,
            kind: ExpiryReason::Past,
        });
    }
    Ok(())
}
```

- [ ] **Step 4: Wire step 2 into `verify_signed_intent`**

Replace the body of `verify_signed_intent` so it calls `step2_timestamp_window`:

```rust
pub async fn verify_signed_intent(
    intent: SignedIntent,
    _resolver: &dyn IssuerKeyResolver,
    now: SystemTime,
) -> Result<VerifiedSignedIntent, VerifyError> {
    step1_syntactic(&intent)?;
    step2_timestamp_window(&intent, now)?;
    Ok(<CoreSignedIntentVerifier as SignedIntentVerifier>::__from_verified(
        intent,
        VerifierWitness::new(),
    ))
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cairn-core verifier`
Expected: 8 tests pass (4 from Task 5 + 4 new).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/verifier.rs
git commit -m "feat(verifier): add timestamp window check (step 2, issue #51)"
```

---

### Task 7: Add scope ↔ identity-kind fit (step 3)

**Files:**
- Modify: `crates/cairn-core/src/verifier.rs`

- [ ] **Step 1: Write the failing tests**

Add to `crates/cairn-core/src/verifier.rs::tests`:

```rust
    #[tokio::test]
    async fn rejects_sensor_writing_session_tier() {
        let mut i = good_intent();
        i.issuer = common::Identity("snr:local:screen:host:v1".to_owned());
        i.scope.tier = SignedIntentScopeTier::Session;
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &OkResolver, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ScopeDenied { .. }));
    }

    #[tokio::test]
    async fn accepts_sensor_writing_private_tier() {
        let mut i = good_intent();
        i.issuer = common::Identity("snr:local:screen:host:v1".to_owned());
        i.scope.tier = SignedIntentScopeTier::Private;
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(i, &OkResolver, now).await.expect("ok");
    }

    #[tokio::test]
    async fn rejects_agent_writing_team_tier() {
        let mut i = good_intent();
        i.issuer = common::Identity("agt:claude-code:opus-4-7:reviewer:v1".to_owned());
        i.scope.tier = SignedIntentScopeTier::Team;
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &OkResolver, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ScopeDenied { .. }));
    }

    #[tokio::test]
    async fn accepts_agent_writing_project_tier() {
        let mut i = good_intent();
        i.issuer = common::Identity("agt:claude-code:opus-4-7:reviewer:v1".to_owned());
        i.scope.tier = SignedIntentScopeTier::Project;
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(i, &OkResolver, now).await.expect("ok");
    }

    #[tokio::test]
    async fn accepts_human_writing_public_tier() {
        let mut i = good_intent();
        i.issuer = common::Identity("hmn:tafeng".to_owned());
        i.scope.tier = SignedIntentScopeTier::Public;
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(i, &OkResolver, now).await.expect("ok");
    }
```

- [ ] **Step 2: Run tests — verify the negative cases fail (currently reach stub)**

Run: `cargo test -p cairn-core verifier`
Expected: `rejects_sensor_writing_session_tier` and `rejects_agent_writing_team_tier` fail (they currently get `Ok`).

- [ ] **Step 3: Add the scope-fit check**

In `crates/cairn-core/src/verifier.rs`, after `step2_timestamp_window`, add:

```rust
/// Step 3: identity-kind ↔ tier fit. P0 baseline (brief §4.2):
/// `snr:` → tier == private; `agt:` → tier ∈ {private, session, project};
/// `hmn:` → all tiers. Closes the "agent self-promotes to public" attack
/// without per-agent policy infrastructure.
fn step3_scope_fit(intent: &SignedIntent) -> Result<(), VerifyError> {
    use crate::domain::identity::IdentityKind;
    use crate::generated::envelope::SignedIntentScopeTier as Tier;

    let kind = Identity::parse(intent.issuer.0.clone())
        .map_err(|e| VerifyError::Malformed { field: "issuer", reason: e.to_string() })?
        .kind();

    let allowed = matches!(
        (kind, intent.scope.tier),
        (IdentityKind::Sensor, Tier::Private)
            | (IdentityKind::Agent, Tier::Private | Tier::Session | Tier::Project)
            | (IdentityKind::Human, _),
    );

    if !allowed {
        return Err(VerifyError::ScopeDenied {
            issuer_kind: kind,
            requested_tier: intent.scope.tier,
        });
    }
    Ok(())
}
```

NOTE: confirm `Identity::kind()` exists and returns `IdentityKind`. If the helper is named differently in the codebase, adapt — the kind is derived from the identity string prefix. If no helper exists, parse directly:

```rust
let kind = match intent.issuer.0.split(':').next().unwrap_or("") {
    "hmn" => IdentityKind::Human,
    "agt" => IdentityKind::Agent,
    "snr" => IdentityKind::Sensor,
    other => return Err(VerifyError::Malformed { field: "issuer", reason: format!("unknown prefix: {other}") }),
};
```

- [ ] **Step 4: Wire step 3 into `verify_signed_intent`**

Update the function body:

```rust
pub async fn verify_signed_intent(
    intent: SignedIntent,
    _resolver: &dyn IssuerKeyResolver,
    now: SystemTime,
) -> Result<VerifiedSignedIntent, VerifyError> {
    step1_syntactic(&intent)?;
    step2_timestamp_window(&intent, now)?;
    step3_scope_fit(&intent)?;
    Ok(<CoreSignedIntentVerifier as SignedIntentVerifier>::__from_verified(
        intent,
        VerifierWitness::new(),
    ))
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cairn-core verifier`
Expected: 13 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/verifier.rs
git commit -m "feat(verifier): add identity-kind to tier fit check (step 3, issue #51)"
```

---

### Task 8: Add resolver lookup (step 4)

**Files:**
- Modify: `crates/cairn-core/src/verifier.rs`

- [ ] **Step 1: Write the failing tests**

Add to `crates/cairn-core/src/verifier.rs::tests` — use a richer fake resolver:

```rust
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Programmable fake resolver — returns whatever the test puts in
    /// the table for a given (issuer string, key_version) pair.
    struct FakeResolver {
        table: Mutex<HashMap<(String, u32), Option<ResolvedKey>>>,
        fail_with: Mutex<Option<&'static str>>,
    }

    impl FakeResolver {
        fn new() -> Self {
            Self { table: Mutex::new(HashMap::new()), fail_with: Mutex::new(None) }
        }
        fn set(&self, issuer: &str, ver: u32, value: Option<ResolvedKey>) {
            self.table.lock().expect("lock").insert((issuer.to_owned(), ver), value);
        }
        fn fail_with(&self, msg: &'static str) {
            *self.fail_with.lock().expect("lock") = Some(msg);
        }
    }

    #[async_trait]
    impl IssuerKeyResolver for FakeResolver {
        async fn lookup(
            &self,
            issuer: &Identity,
            key_version: KeyVersion,
        ) -> Result<Option<ResolvedKey>, ResolverError> {
            if let Some(msg) = *self.fail_with.lock().expect("lock") {
                return Err(ResolverError::Backend(msg.into()));
            }
            let key = (issuer.to_string(), key_version.into_inner());
            Ok(self.table.lock().expect("lock").get(&key).cloned().flatten())
        }
    }

    #[tokio::test]
    async fn rejects_unknown_key() {
        let r = FakeResolver::new();
        // No entry → None → UnknownKey.
        let i = good_intent();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::UnknownKey { .. }));
    }

    #[tokio::test]
    async fn rejects_non_operational_lifecycle() {
        let r = FakeResolver::new();
        r.set("hmn:tafeng", 1, Some(ResolvedKey { public_key: [0u8; 32], lifecycle: KeyLifecycle::NonOperational }));
        let i = good_intent();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::UnknownKey { .. }));
    }

    #[tokio::test]
    async fn rejects_purged_lifecycle() {
        let r = FakeResolver::new();
        r.set("hmn:tafeng", 1, Some(ResolvedKey { public_key: [0u8; 32], lifecycle: KeyLifecycle::Purged }));
        let i = good_intent();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::UnknownKey { .. }));
    }

    #[tokio::test]
    async fn rejects_revoked_before_issued_at() {
        let r = FakeResolver::new();
        r.set("hmn:tafeng", 1, Some(ResolvedKey {
            public_key: [0u8; 32],
            lifecycle: KeyLifecycle::Revoked { effective_at: "2026-04-22T14:02:00Z".to_owned() },
        }));
        let i = good_intent(); // issued_at = 2026-04-22T14:02:11Z
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::RevokedKey { .. }));
    }

    #[tokio::test]
    async fn accepts_revoked_after_issued_at() {
        let r = FakeResolver::new();
        r.set("hmn:tafeng", 1, Some(ResolvedKey {
            public_key: [0u8; 32],
            // Revocation after issued_at — earlier op remains valid (brief §4.2)
            lifecycle: KeyLifecycle::Revoked { effective_at: "2026-04-22T15:00:00Z".to_owned() },
        }));
        let i = good_intent();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        // step 5 (signature verify) is still a stub at this point.
        verify_signed_intent(i, &r, now).await.expect("revocation post-dates op");
    }

    #[tokio::test]
    async fn surfaces_resolver_io_error() {
        let r = FakeResolver::new();
        r.fail_with("synthetic backend failure");
        let i = good_intent();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(i, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::ResolverFailure(_)));
    }
```

NOTE: confirm `KeyVersion::into_inner` returns the underlying `u32`. If the type uses a different accessor (e.g. `as_u32()` or `value()`), adapt the call. If only `From<u32>` exists, use the inverse.

- [ ] **Step 2: Run tests — verify the negative cases fail**

Run: `cargo test -p cairn-core verifier`
Expected: the 5 lookup tests fail (currently fall through to stub Ok).

- [ ] **Step 3: Add `step4_resolver_lookup`**

In `crates/cairn-core/src/verifier.rs`, after `step3_scope_fit`, add:

```rust
use crate::contract::issuer_key_resolver::{KeyLifecycle, ResolvedKey};
use crate::domain::identity::keys::KeyVersion;

/// Step 4: resolver lookup. Returns the resolved key on success;
/// maps None / Pending / Purged → `UnknownKey`,
/// Revoked w/ `effective_at ≤ issued_at` → `RevokedKey`.
async fn step4_resolver_lookup(
    intent: &SignedIntent,
    resolver: &dyn IssuerKeyResolver,
) -> Result<ResolvedKey, VerifyError> {
    let issuer = Identity::parse(intent.issuer.0.clone())
        .map_err(|e| VerifyError::Malformed { field: "issuer", reason: e.to_string() })?;
    let kv = u32::try_from(intent.key_version)
        .map_err(|_| VerifyError::Malformed { field: "key_version", reason: format!("out of u32 range: {}", intent.key_version) })?;
    let key_version = KeyVersion::try_from(kv)
        .map_err(|e| VerifyError::Malformed { field: "key_version", reason: e.to_string() })?;
    let resolved = resolver
        .lookup(&issuer, key_version)
        .await
        .map_err(VerifyError::ResolverFailure)?
        .ok_or_else(|| VerifyError::UnknownKey { issuer: issuer.clone(), key_version })?;
    match &resolved.lifecycle {
        KeyLifecycle::Active => Ok(resolved),
        KeyLifecycle::Revoked { effective_at } => {
            // Revocation reactivates only ops issued at or after effective_at.
            if effective_at.as_str() <= intent.issued_at.as_str() {
                Err(VerifyError::RevokedKey {
                    issuer,
                    key_version,
                    effective_at: effective_at.clone(),
                })
            } else {
                Ok(resolved)
            }
        }
        KeyLifecycle::NonOperational | KeyLifecycle::Purged => {
            Err(VerifyError::UnknownKey { issuer, key_version })
        }
    }
}
```

NOTE: this uses **lex string compare** on RFC3339 timestamps. RFC3339 timestamps in UTC ("Z" suffix, fixed-precision) are lex-comparable. If the codebase already has a typed `Rfc3339Timestamp` with `<=` semantics, prefer that; the helper is in `crates/cairn-core/src/domain/timestamp.rs`. Replace the lex-compare with the typed version if available:

```rust
let effective_dt = chrono::DateTime::parse_from_rfc3339(effective_at)
    .map_err(|e| VerifyError::Malformed { field: "effective_at", reason: e.to_string() })?;
let issued_dt = chrono::DateTime::parse_from_rfc3339(&intent.issued_at)
    .map_err(|e| VerifyError::Malformed { field: "issued_at", reason: e.to_string() })?;
if effective_dt <= issued_dt {
    // revoked
}
```

Use the typed comparison — it survives any RFC3339 sub-second / timezone shape variation.

- [ ] **Step 4: Wire step 4 into `verify_signed_intent`**

Update the function body:

```rust
pub async fn verify_signed_intent(
    intent: SignedIntent,
    resolver: &dyn IssuerKeyResolver,
    now: SystemTime,
) -> Result<VerifiedSignedIntent, VerifyError> {
    step1_syntactic(&intent)?;
    step2_timestamp_window(&intent, now)?;
    step3_scope_fit(&intent)?;
    let _resolved = step4_resolver_lookup(&intent, resolver).await?;
    Ok(<CoreSignedIntentVerifier as SignedIntentVerifier>::__from_verified(
        intent,
        VerifierWitness::new(),
    ))
}
```

(`_resolved` is unused until task 9 wires step 5.)

- [ ] **Step 5: Update happy-path tests to seed the FakeResolver**

The earlier `accepts_window_within_bounds` and `accepts_human_writing_public_tier` tests etc. used `OkResolver` (always returns Active). Those continue to work because `OkResolver` still seeds an Active key.

Find every existing call in `tests` that uses `OkResolver` for a happy-path case — they keep working unchanged.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p cairn-core verifier`
Expected: 19 tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/verifier.rs
git commit -m "feat(verifier): add issuer-key resolver lookup (step 4, issue #51)"
```

---

### Task 9: Add Ed25519 signature verification (step 5)

**Files:**
- Modify: `crates/cairn-core/src/verifier.rs`

- [ ] **Step 1: Write the failing tests using a real Ed25519 keypair**

Add to `crates/cairn-core/src/verifier.rs::tests`:

```rust
    use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
    use rand_core::OsRng;
    use crate::intent::canonical_envelope::canonicalize_signed_payload;

    /// Build a (intent, resolver) pair with a real signature so the
    /// happy path can flow through step 5.
    fn signed_pair() -> (SignedIntent, FakeResolver) {
        let mut rng = OsRng;
        let signing = SigningKey::generate(&mut rng);
        let verifying: VerifyingKey = signing.verifying_key();

        let mut intent = good_intent();
        intent.signature = common::Ed25519Signature(format!("ed25519:{}", "0".repeat(128))); // placeholder

        let canonical = canonicalize_signed_payload(&intent).expect("canonical");
        let sig = signing.sign(&canonical);
        intent.signature = common::Ed25519Signature(format!("ed25519:{}", hex::encode(sig.to_bytes())));

        let r = FakeResolver::new();
        r.set("hmn:tafeng", 1, Some(ResolvedKey {
            public_key: verifying.to_bytes(),
            lifecycle: KeyLifecycle::Active,
        }));
        (intent, r)
    }

    #[tokio::test]
    async fn accepts_well_formed_signed_intent() {
        let (intent, r) = signed_pair();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(intent, &r, now).await.expect("real signature verifies");
    }

    #[tokio::test]
    async fn rejects_tampered_signature() {
        let (mut intent, r) = signed_pair();
        // Flip one hex char of the signature.
        let s = intent.signature.0.clone();
        let mut bytes: Vec<char> = s.chars().collect();
        let idx = bytes.len() / 2;
        bytes[idx] = if bytes[idx] == 'a' { 'b' } else { 'a' };
        intent.signature = common::Ed25519Signature(bytes.into_iter().collect());
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(intent, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::InvalidSignature));
    }

    #[tokio::test]
    async fn rejects_swapped_target_hash() {
        let (mut intent, r) = signed_pair();
        intent.target_hash = format!("sha256:{}", "9".repeat(64));
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(intent, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::InvalidSignature));
    }

    #[tokio::test]
    async fn rejects_signature_against_wrong_pubkey() {
        let (intent, _r) = signed_pair();
        // Replace the resolver with a different pubkey (random).
        let other = SigningKey::generate(&mut OsRng);
        let r = FakeResolver::new();
        r.set("hmn:tafeng", 1, Some(ResolvedKey {
            public_key: other.verifying_key().to_bytes(),
            lifecycle: KeyLifecycle::Active,
        }));
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        let err = verify_signed_intent(intent, &r, now).await.unwrap_err();
        assert!(matches!(err, VerifyError::InvalidSignature));
    }
```

NOTE: This task uses `hex` in **both** test code (here in step 1) and
production code (step 4 — `hex::decode` for the ed25519 signature blob).
Add `hex` once as a regular dep — not a dev-dep — and re-export through
the workspace.

In root `Cargo.toml`, add to `[workspace.dependencies]`:

```toml
hex = { version = "0.4", default-features = false, features = ["std"] }
```

In `crates/cairn-core/Cargo.toml` `[dependencies]`:

```toml
hex = { workspace = true }
```

- [ ] **Step 2: Update `OkResolver` so the happy paths in earlier tasks still work**

Earlier tests pass `[0u8; 32]` as the pubkey but use a placeholder hex signature — once step 5 is wired, those tests will start failing because the signature won't verify. Fix the cleanest way: make those tests use `signed_pair()` instead of `good_intent() + OkResolver`. Refactor every happy-path test (`accepts_window_within_bounds`, `accepts_sensor_writing_private_tier`, `accepts_agent_writing_project_tier`, `accepts_human_writing_public_tier`, `accepts_revoked_after_issued_at`) to start from `signed_pair()` and mutate from there:

```rust
    #[tokio::test]
    async fn accepts_window_within_bounds() {
        let (intent, r) = signed_pair();
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(intent, &r, now).await.expect("within bounds");
    }
```

For `accepts_revoked_after_issued_at`, alter the resolver entry created by `signed_pair()` to swap lifecycle:

```rust
    #[tokio::test]
    async fn accepts_revoked_after_issued_at() {
        let (intent, r) = signed_pair();
        // Overwrite the resolver entry with Revoked-future
        r.set("hmn:tafeng", 1, Some(ResolvedKey {
            public_key: r.table.lock().expect("lock").get(&("hmn:tafeng".to_owned(), 1)).cloned().flatten().expect("seeded").public_key,
            lifecycle: KeyLifecycle::Revoked { effective_at: "2026-04-22T15:00:00Z".to_owned() },
        }));
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(intent, &r, now).await.expect("revocation post-dates op");
    }
```

For sensor / agent / human tier acceptance tests, mutate the `intent.issuer` after `signed_pair()` then re-sign:

```rust
    #[tokio::test]
    async fn accepts_human_writing_public_tier() {
        // Re-sign with a fresh keypair using the new issuer.
        // For brevity in tests, factor a helper:
        let (mut intent, r) = signed_pair();
        intent.scope.tier = SignedIntentScopeTier::Public;
        // Re-sign because we mutated the canonical payload.
        let signing = SigningKey::generate(&mut OsRng);
        let canonical = canonicalize_signed_payload(&intent).expect("canon");
        let sig = signing.sign(&canonical);
        intent.signature = common::Ed25519Signature(format!("ed25519:{}", hex::encode(sig.to_bytes())));
        // Replace the resolver entry's pubkey to match the new signing key.
        r.set("hmn:tafeng", 1, Some(ResolvedKey {
            public_key: signing.verifying_key().to_bytes(),
            lifecycle: KeyLifecycle::Active,
        }));
        let now = rfc3339_to_systemtime("2026-04-22T14:02:30Z");
        verify_signed_intent(intent, &r, now).await.expect("ok");
    }
```

This pattern repeats across each happy-path acceptance test. Factor a helper:

```rust
    fn signed_pair_with<F: FnOnce(&mut SignedIntent)>(mutate: F) -> (SignedIntent, FakeResolver) {
        let mut rng = OsRng;
        let signing = SigningKey::generate(&mut rng);
        let mut intent = good_intent();
        mutate(&mut intent);
        let canonical = canonicalize_signed_payload(&intent).expect("canon");
        let sig = signing.sign(&canonical);
        intent.signature = common::Ed25519Signature(format!("ed25519:{}", hex::encode(sig.to_bytes())));
        let r = FakeResolver::new();
        let issuer_str = intent.issuer.0.clone();
        r.set(&issuer_str, intent.key_version as u32, Some(ResolvedKey {
            public_key: signing.verifying_key().to_bytes(),
            lifecycle: KeyLifecycle::Active,
        }));
        (intent, r)
    }
```

Use `signed_pair_with(|i| i.scope.tier = SignedIntentScopeTier::Public)` etc. in each acceptance test; this halves the boilerplate.

- [ ] **Step 3: Run tests — confirm the new ones (and any happy-path refactors) fail until step 5 lands**

Run: `cargo test -p cairn-core verifier`
Expected: at least the 4 new `accepts_well_formed_signed_intent` / tamper tests fail; the refactored happy-path tests now pass through to step 5 and fail too because step 5 doesn't reject anything yet (or accepts everything, depending on how they expect to fail).

Actually wait — if step 5 isn't wired, the verifier currently *accepts* every input that passes steps 1-4. So happy-path tests pass; tamper/swap tests fail (they expected `InvalidSignature` but get `Ok`).

Confirm: 4 new tests fail with `Ok` instead of `InvalidSignature`.

- [ ] **Step 4: Add `step5_signature_verify`**

In `crates/cairn-core/src/verifier.rs`:

```rust
use ed25519_dalek::{Signature, VerifyingKey, Verifier};
use crate::intent::canonical_envelope::canonicalize_signed_payload;

/// Step 5: JCS-canonicalize envelope-minus-signature, parse the
/// `ed25519:<128-hex>` signature blob, parse the resolved 32-byte
/// pubkey, and run Ed25519 verify. Any failure → `InvalidSignature`
/// (opaque on purpose).
fn step5_signature_verify(
    intent: &SignedIntent,
    resolved: &ResolvedKey,
) -> Result<(), VerifyError> {
    let canonical = canonicalize_signed_payload(intent)?;
    let sig_hex = intent
        .signature
        .0
        .strip_prefix("ed25519:")
        .ok_or(VerifyError::InvalidSignature)?;
    let sig_bytes = hex::decode(sig_hex).map_err(|_| VerifyError::InvalidSignature)?;
    let sig_arr: [u8; 64] = sig_bytes.as_slice().try_into().map_err(|_| VerifyError::InvalidSignature)?;
    let signature = Signature::from_bytes(&sig_arr);
    let verifying = VerifyingKey::from_bytes(&resolved.public_key)
        .map_err(|_| VerifyError::InvalidSignature)?;
    verifying.verify(&canonical, &signature).map_err(|_| VerifyError::InvalidSignature)
}
```

Note: `cairn-core` already lists `ed25519-dalek` as a dep; `hex` was
added to regular `[dependencies]` (not dev) back in step 1 of this
task, so the production import here works without further Cargo
changes.

- [ ] **Step 5: Wire step 5 into `verify_signed_intent`**

Final shape of the function body:

```rust
pub async fn verify_signed_intent(
    intent: SignedIntent,
    resolver: &dyn IssuerKeyResolver,
    now: SystemTime,
) -> Result<VerifiedSignedIntent, VerifyError> {
    step1_syntactic(&intent)?;
    step2_timestamp_window(&intent, now)?;
    step3_scope_fit(&intent)?;
    let resolved = step4_resolver_lookup(&intent, resolver).await?;
    step5_signature_verify(&intent, &resolved)?;
    Ok(<CoreSignedIntentVerifier as SignedIntentVerifier>::__from_verified(
        intent,
        VerifierWitness::new(),
    ))
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p cairn-core verifier`
Expected: every test passes (≥ 23 tests).

- [ ] **Step 7: Tracing instrumentation**

Add the `#[tracing::instrument]` decorator above `verify_signed_intent`:

```rust
#[tracing::instrument(
    skip(intent, resolver),
    err,
    fields(
        verb = "verify_signed_intent",
        issuer = %intent.issuer.0,
        key_version = intent.key_version,
        operation_id = %intent.operation_id.0,
    ),
)]
pub async fn verify_signed_intent( /* ... unchanged ... */ )
```

- [ ] **Step 8: Run clippy + boundary check**

Run:
```
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
./scripts/check-core-boundary.sh
```
Both PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-core/src/verifier.rs crates/cairn-core/Cargo.toml Cargo.toml
git commit -m "feat(verifier): add Ed25519 signature verification (step 5, issue #51)"
```

---

### Task 10: Add fixture builder + fake resolver in `cairn-test-fixtures`

**Files:**
- Create: `crates/cairn-test-fixtures/src/signed_intent.rs`
- Modify: `crates/cairn-test-fixtures/src/lib.rs`
- Modify: `crates/cairn-test-fixtures/Cargo.toml`

- [ ] **Step 1: Write the test target — exercises the public surface**

Add unit tests to `crates/cairn-test-fixtures/src/signed_intent.rs` (will be created in step 2):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builder_default_passes_verification() {
        use cairn_core::verifier::verify_signed_intent;
        use std::time::SystemTime;

        let (intent, resolver, now) = SignedIntentFixture::default().build();
        verify_signed_intent(intent, &resolver, now).await.expect("verifies");
    }

    #[tokio::test]
    async fn builder_overrides_tier() {
        use cairn_core::generated::envelope::SignedIntentScopeTier;
        let (intent, _, _) = SignedIntentFixture::builder()
            .tier(SignedIntentScopeTier::Project)
            .build_full();
        assert_eq!(intent.scope.tier, SignedIntentScopeTier::Project);
    }
}
```

- [ ] **Step 2: Create the module**

Create `crates/cairn-test-fixtures/src/signed_intent.rs`:

```rust
//! `SignedIntent` fixture builder + companion fake `IssuerKeyResolver`.
//!
//! Use `SignedIntentFixture::default().build()` for "Just give me a
//! valid envelope" cases. Override fields with the `bon`-derived
//! builder when a test needs a specific shape.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use async_trait::async_trait;
use bon::bon;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand_core::OsRng;

use cairn_core::contract::issuer_key_resolver::{
    IssuerKeyResolver, KeyLifecycle, ResolvedKey, ResolverError,
};
use cairn_core::domain::identity::{Identity, keys::KeyVersion};
use cairn_core::generated::common;
use cairn_core::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};
use cairn_core::intent::canonical_envelope::canonicalize_signed_payload;

/// Builder for `(SignedIntent, FakeIssuerKeyResolver, now)` triples
/// used in trust-boundary tests.
pub struct SignedIntentFixture {
    issuer: String,
    issued_at: String,
    expires_at: String,
    now: String,
    tier: SignedIntentScopeTier,
    key_version: i64,
    sequence: Option<u64>,
    server_challenge: Option<String>,
    lifecycle: KeyLifecycle,
}

#[bon]
impl SignedIntentFixture {
    /// Build the fixture with override hooks for any field. Returns
    /// `(intent, resolver, now)` ready to feed to
    /// `verify_signed_intent`.
    #[builder]
    pub fn build_full(
        #[builder(default = "hmn:tafeng".to_owned())] issuer: String,
        #[builder(default = "2026-04-22T14:02:11Z".to_owned())] issued_at: String,
        #[builder(default = "2026-04-22T14:07:11Z".to_owned())] expires_at: String,
        #[builder(default = "2026-04-22T14:02:30Z".to_owned())] now: String,
        #[builder(default = SignedIntentScopeTier::Project)] tier: SignedIntentScopeTier,
        #[builder(default = 1)] key_version: i64,
        #[builder(default = Some(1))] sequence: Option<u64>,
        #[builder(default)] server_challenge: Option<String>,
        #[builder(default = KeyLifecycle::Active)] lifecycle: KeyLifecycle,
    ) -> (SignedIntent, FakeIssuerKeyResolver, SystemTime) {
        let mut rng = OsRng;
        let signing = SigningKey::generate(&mut rng);
        let verifying: VerifyingKey = signing.verifying_key();

        let mut intent = SignedIntent {
            chain_parents: vec![],
            expires_at,
            issued_at,
            issuer: common::Identity(issuer.clone()),
            key_version,
            nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".to_owned()),
            operation_id: common::Ulid("01HQZX9F5N0000000000000000".to_owned()),
            scope: SignedIntentScope {
                tenant: "acme".to_owned(),
                workspace: "ws".to_owned(),
                entity: "ent".to_owned(),
                tier,
            },
            sequence,
            server_challenge: server_challenge.map(common::Nonce16Base64),
            signature: common::Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
            target_hash: format!("sha256:{}", "a".repeat(64)),
        };
        let canonical = canonicalize_signed_payload(&intent).expect("canon");
        let sig = signing.sign(&canonical);
        intent.signature = common::Ed25519Signature(format!("ed25519:{}", hex::encode(sig.to_bytes())));

        let resolver = FakeIssuerKeyResolver::new();
        resolver.set(&issuer, key_version as u32, Some(ResolvedKey {
            public_key: verifying.to_bytes(),
            lifecycle,
        }));

        let now_st: SystemTime = DateTime::parse_from_rfc3339(&now)
            .expect("rfc3339 now")
            .with_timezone(&Utc)
            .into();
        (intent, resolver, now_st)
    }
}

impl SignedIntentFixture {
    /// Quick default — equivalent to `build_full()` with no overrides.
    #[must_use]
    pub fn default() -> SignedIntentFixtureDefault {
        SignedIntentFixtureDefault
    }
}

/// Marker returned by `SignedIntentFixture::default()` to expose a
/// terse `.build()` API parallel to `bon`'s `.build_full()`.
pub struct SignedIntentFixtureDefault;

impl SignedIntentFixtureDefault {
    #[must_use]
    pub fn build(self) -> (SignedIntent, FakeIssuerKeyResolver, SystemTime) {
        SignedIntentFixture::build_full(
            "hmn:tafeng".to_owned(),
            "2026-04-22T14:02:11Z".to_owned(),
            "2026-04-22T14:07:11Z".to_owned(),
            "2026-04-22T14:02:30Z".to_owned(),
            SignedIntentScopeTier::Project,
            1,
            Some(1),
            None,
            KeyLifecycle::Active,
        )
    }
}

/// Programmable in-memory resolver. Test-only.
pub struct FakeIssuerKeyResolver {
    table: Mutex<HashMap<(String, u32), Option<ResolvedKey>>>,
    fail_with: Mutex<Option<&'static str>>,
}

impl FakeIssuerKeyResolver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            table: Mutex::new(HashMap::new()),
            fail_with: Mutex::new(None),
        }
    }

    pub fn set(&self, issuer: &str, ver: u32, value: Option<ResolvedKey>) {
        self.table
            .lock()
            .expect("lock")
            .insert((issuer.to_owned(), ver), value);
    }

    pub fn fail_with(&self, msg: &'static str) {
        *self.fail_with.lock().expect("lock") = Some(msg);
    }
}

impl Default for FakeIssuerKeyResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IssuerKeyResolver for FakeIssuerKeyResolver {
    async fn lookup(
        &self,
        issuer: &Identity,
        key_version: KeyVersion,
    ) -> Result<Option<ResolvedKey>, ResolverError> {
        if let Some(msg) = *self.fail_with.lock().expect("lock") {
            return Err(ResolverError::Backend(msg.into()));
        }
        let key = (issuer.to_string(), key_version_as_u32(key_version));
        Ok(self.table.lock().expect("lock").get(&key).cloned().flatten())
    }
}

fn key_version_as_u32(kv: KeyVersion) -> u32 {
    // Adapt to whatever accessor `KeyVersion` exposes:
    // - if `pub fn into_inner(self) -> u32`, call it directly,
    // - if `From<KeyVersion> for u32`, use `u32::from(kv)`,
    // - else if it wraps `NonZeroU32`, use `kv.get().get()`.
    u32::try_from(kv.into_inner()).expect("KeyVersion fits in u32")
}
```

NOTE: confirm the exact accessor for `KeyVersion`. Pick the form the codebase already uses elsewhere (search for `KeyVersion::FIRST` / `KeyVersion::new`).

- [ ] **Step 3: Wire the module + dep**

In `crates/cairn-test-fixtures/src/lib.rs`, add:

```rust
pub mod signed_intent;
```

In `crates/cairn-test-fixtures/Cargo.toml` `[dependencies]`, add:

```toml
async-trait = { workspace = true }
bon = { workspace = true }
chrono = { workspace = true }
ed25519-dalek = { workspace = true }
hex = { workspace = true }
rand_core = { workspace = true }
```

(Keep existing entries; only add the missing ones.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p cairn-test-fixtures signed_intent`
Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-test-fixtures
git commit -m "feat(test-fixtures): add signed_intent builder + fake resolver (issue #51)"
```

---

### Task 11: Add property tests for canonicalization + tamper invariant

**Files:**
- Create: `crates/cairn-core/tests/canonical_envelope_props.rs`

- [ ] **Step 1: Write property tests**

Create `crates/cairn-core/tests/canonical_envelope_props.rs`:

```rust
//! Property tests for the JCS canonicalizer (issue #51).
//!
//! These run against `cairn-core`'s public surface so they live in
//! `tests/` (integration-style) rather than the inline `#[cfg(test)]`
//! module — proptest fits awkwardly inside a unit-test module.

use proptest::prelude::*;

use cairn_core::generated::common;
use cairn_core::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};
use cairn_core::intent::canonical_envelope::canonicalize_signed_payload;

fn arb_intent() -> impl Strategy<Value = SignedIntent> {
    (
        any::<u64>().prop_filter("safe-int", |s| *s <= 9_007_199_254_740_991),
        1i64..1_000_000,
        prop_oneof![
            Just(SignedIntentScopeTier::Private),
            Just(SignedIntentScopeTier::Session),
            Just(SignedIntentScopeTier::Project),
            Just(SignedIntentScopeTier::Team),
            Just(SignedIntentScopeTier::Org),
            Just(SignedIntentScopeTier::Public),
        ],
        "[a-z]{1,8}",
        "[a-z]{1,8}",
        "[a-z]{1,8}",
    ).prop_map(|(seq, kv, tier, tenant, ws, ent)| SignedIntent {
        chain_parents: vec![],
        expires_at: "2026-04-22T14:07:11Z".to_owned(),
        issued_at: "2026-04-22T14:02:11Z".to_owned(),
        issuer: common::Identity("hmn:tafeng".to_owned()),
        key_version: kv,
        nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".to_owned()),
        operation_id: common::Ulid("01HQZX9F5N0000000000000000".to_owned()),
        scope: SignedIntentScope { tenant, workspace: ws, entity: ent, tier },
        sequence: Some(seq),
        server_challenge: None,
        signature: common::Ed25519Signature(format!("ed25519:{}", "a".repeat(128))),
        target_hash: format!("sha256:{}", "a".repeat(64)),
    })
}

proptest! {
    /// Property 1: canonicalization is deterministic across calls.
    #[test]
    fn determinism(intent in arb_intent()) {
        let a = canonicalize_signed_payload(&intent).expect("a");
        let b = canonicalize_signed_payload(&intent).expect("b");
        prop_assert_eq!(a, b);
    }

    /// Property 2: canonical bytes never contain the literal "signature".
    #[test]
    fn signature_excluded(intent in arb_intent()) {
        let bytes = canonicalize_signed_payload(&intent).expect("ok");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        prop_assert!(!s.contains("\"signature\""));
    }

    /// Property 3: mutating `target_hash` always changes canonical bytes.
    #[test]
    fn target_hash_tamper_changes_bytes(mut intent in arb_intent()) {
        let baseline = canonicalize_signed_payload(&intent).expect("base");
        intent.target_hash = format!("sha256:{}", "b".repeat(64));
        let after = canonicalize_signed_payload(&intent).expect("mut");
        prop_assert_ne!(baseline, after);
    }

    /// Property 4: mutating `key_version` always changes canonical bytes.
    #[test]
    fn key_version_tamper_changes_bytes(mut intent in arb_intent()) {
        let baseline = canonicalize_signed_payload(&intent).expect("base");
        intent.key_version = intent.key_version.wrapping_add(1).max(1);
        let after = canonicalize_signed_payload(&intent).expect("mut");
        if intent.key_version != 0 {
            // Skip the (extremely rare) overflow that wraps back to original.
            prop_assert_ne!(baseline, after);
        }
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo nextest run -p cairn-core --test canonical_envelope_props`
Expected: all four properties pass with the default 256 cases each (~1 s).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/tests/canonical_envelope_props.rs
git commit -m "test(verifier): proptest canonical-envelope determinism + tamper invariants (issue #51)"
```

---

### Task 12: Add `VerifyError` Display snapshot tests

**Files:**
- Create: `crates/cairn-core/tests/verify_error_snapshots.rs`

- [ ] **Step 1: Write the test that snapshots every variant**

Create `crates/cairn-core/tests/verify_error_snapshots.rs`:

```rust
//! Snapshot tests for `VerifyError::Display` — locks the wire-stable
//! error wording demanded by issue #51 acceptance criterion 3.

use cairn_core::contract::issuer_key_resolver::ResolverError;
use cairn_core::domain::identity::{Identity, IdentityKind, keys::KeyVersion};
use cairn_core::generated::envelope::SignedIntentScopeTier;
use cairn_core::intent::{ExpiryReason, VerifyError};

#[test]
fn malformed_snapshot() {
    let e = VerifyError::Malformed { field: "issuer", reason: "bad prefix".to_owned() };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn expired_skewed_snapshot() {
    let e = VerifyError::ExpiredIntent {
        issued_at: "2026-04-22T14:02:11Z".to_owned(),
        expires_at: "2026-04-22T14:07:11Z".to_owned(),
        now: "2026-04-22T14:30:00Z".to_owned(),
        kind: ExpiryReason::Skewed,
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn expired_past_snapshot() {
    let e = VerifyError::ExpiredIntent {
        issued_at: "2026-04-22T14:02:11Z".to_owned(),
        expires_at: "2026-04-22T14:03:11Z".to_owned(),
        now: "2026-04-22T14:30:00Z".to_owned(),
        kind: ExpiryReason::Past,
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn expired_ttl_exceeded_snapshot() {
    let e = VerifyError::ExpiredIntent {
        issued_at: "2026-04-22T14:02:11Z".to_owned(),
        expires_at: "2026-04-22T14:30:11Z".to_owned(),
        now: "2026-04-22T14:02:30Z".to_owned(),
        kind: ExpiryReason::TtlExceeded,
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn scope_denied_snapshot() {
    let e = VerifyError::ScopeDenied {
        issuer_kind: IdentityKind::Agent,
        requested_tier: SignedIntentScopeTier::Team,
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn unknown_key_snapshot() {
    let e = VerifyError::UnknownKey {
        issuer: Identity::parse("hmn:tafeng").expect("parse"),
        key_version: KeyVersion::FIRST,
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn revoked_key_snapshot() {
    let e = VerifyError::RevokedKey {
        issuer: Identity::parse("hmn:tafeng").expect("parse"),
        key_version: KeyVersion::FIRST,
        effective_at: "2026-04-22T14:00:00Z".to_owned(),
    };
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn invalid_signature_snapshot() {
    let e = VerifyError::InvalidSignature;
    insta::assert_snapshot!(e.to_string());
}

#[test]
fn resolver_failure_snapshot() {
    let e = VerifyError::ResolverFailure(ResolverError::Backend("boom".into()));
    insta::assert_snapshot!(e.to_string());
}
```

- [ ] **Step 2: Generate the initial snapshot files**

Run: `cargo nextest run -p cairn-core --test verify_error_snapshots`
Expected: tests fail because snapshots don't exist yet (`insta` writes `.snap.new` files).

- [ ] **Step 3: Accept the snapshots**

Run: `cargo insta accept --workspace`
Expected: 9 `.snap` files written under `crates/cairn-core/tests/snapshots/`.

If `cargo-insta` is not installed: `cargo install cargo-insta` first.

- [ ] **Step 4: Re-run tests to confirm they pass**

Run: `cargo nextest run -p cairn-core --test verify_error_snapshots`
Expected: 9 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/tests/verify_error_snapshots.rs crates/cairn-core/tests/snapshots/
git commit -m "test(verifier): snapshot Display output for every VerifyError variant (issue #51)"
```

---

### Task 13: Implement `SqliteIssuerKeyResolver`

**Files:**
- Create: `crates/cairn-store-sqlite/src/issuer_key_resolver.rs`
- Modify: `crates/cairn-store-sqlite/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-store-sqlite/tests/issuer_key_resolver.rs`:

```rust
//! Lifecycle-mapping integration test for `SqliteIssuerKeyResolver`.

use cairn_core::contract::issuer_key_resolver::{IssuerKeyResolver, KeyLifecycle};
use cairn_core::domain::identity::{Identity, keys::KeyVersion};
use cairn_store_sqlite::SqliteIssuerKeyResolver;

#[tokio::test]
async fn unknown_issuer_returns_none() {
    let store = cairn_test_fixtures::memstore().await;
    let resolver = SqliteIssuerKeyResolver::new(std::sync::Arc::new(store));
    let id = Identity::parse("hmn:nobody").expect("parse");
    let r = resolver.lookup(&id, KeyVersion::FIRST).await.expect("ok");
    assert!(r.is_none());
}

#[tokio::test]
async fn active_identity_resolves_to_active_lifecycle() {
    use cairn_test_fixtures::seed_active_identity;
    let store = cairn_test_fixtures::memstore().await;
    let (id, kv, expected_pubkey) = seed_active_identity(&store, "hmn:alice").await;
    let resolver = SqliteIssuerKeyResolver::new(std::sync::Arc::new(store));
    let r = resolver.lookup(&id, kv).await.expect("ok").expect("present");
    assert_eq!(r.public_key, expected_pubkey);
    assert!(matches!(r.lifecycle, KeyLifecycle::Active));
}

#[tokio::test]
async fn revoked_identity_resolves_to_revoked_lifecycle_with_effective_at() {
    use cairn_test_fixtures::seed_revoked_identity;
    let store = cairn_test_fixtures::memstore().await;
    let (id, kv, _, revoked_at) = seed_revoked_identity(&store, "hmn:bob").await;
    let resolver = SqliteIssuerKeyResolver::new(std::sync::Arc::new(store));
    let r = resolver.lookup(&id, kv).await.expect("ok").expect("present");
    match r.lifecycle {
        KeyLifecycle::Revoked { effective_at } => assert_eq!(effective_at, revoked_at),
        other => panic!("expected Revoked, got {other:?}"),
    }
}
```

NOTE: this test pulls helpers (`seed_active_identity`, `seed_revoked_identity`) from `cairn_test_fixtures`. If those helpers don't yet exist, factor them into `crates/cairn-test-fixtures/src/identity.rs` first — they're simple wrappers around `IdentityRegistry::reserve_identity` + `activate_identity` (+ `begin_revocation` for the revoked case). The seed function returns `(Identity, KeyVersion, [u8; 32], String_revoked_at)`.

If factoring the helpers into the fixtures crate is too large, inline the seed logic in this test file using `cairn_store_sqlite`'s `IdentityRegistry` impl directly.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p cairn-store-sqlite --test issuer_key_resolver`
Expected: compile-error (`cannot find SqliteIssuerKeyResolver`).

- [ ] **Step 3: Implement the resolver**

Create `crates/cairn-store-sqlite/src/issuer_key_resolver.rs`:

```rust
//! `IssuerKeyResolver` implementation backed by `SqliteMemoryStore`'s
//! `IdentityRegistry` surface.

use std::sync::Arc;

use async_trait::async_trait;

use cairn_core::contract::IdentityRegistry;
use cairn_core::contract::identity_registry::IdentityVisibility;
use cairn_core::contract::issuer_key_resolver::{
    IssuerKeyResolver, KeyLifecycle, ResolvedKey, ResolverError,
};
use cairn_core::domain::identity::{
    Identity,
    keys::KeyVersion,
    records::ProvisioningState,
};

/// Resolver backed by an `IdentityRegistry`-implementing store.
pub struct SqliteIssuerKeyResolver<R: IdentityRegistry + 'static> {
    registry: Arc<R>,
}

impl<R: IdentityRegistry + 'static> SqliteIssuerKeyResolver<R> {
    #[must_use]
    pub fn new(registry: Arc<R>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl<R: IdentityRegistry + 'static> IssuerKeyResolver for SqliteIssuerKeyResolver<R> {
    async fn lookup(
        &self,
        issuer: &Identity,
        key_version: KeyVersion,
    ) -> Result<Option<ResolvedKey>, ResolverError> {
        let record = self
            .registry
            .get_identity(issuer, IdentityVisibility::all())
            .await
            .map_err(|e| ResolverError::Backend(Box::new(e)))?;
        let Some(record) = record else { return Ok(None); };

        let keys = self
            .registry
            .list_keys(issuer)
            .await
            .map_err(|e| ResolverError::Backend(Box::new(e)))?;
        let Some(entry) = keys.into_iter().find(|k| k.key_version == key_version) else {
            return Ok(None);
        };

        let lifecycle = match record.provisioning_state {
            ProvisioningState::Active => KeyLifecycle::Active,
            ProvisioningState::Revoked => KeyLifecycle::Revoked {
                effective_at: record
                    .revoked_at
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_default(),
            },
            ProvisioningState::Pending
            | ProvisioningState::RevokePending
            | ProvisioningState::PurgePending => KeyLifecycle::NonOperational,
            ProvisioningState::Purged => KeyLifecycle::Purged,
        };

        Ok(Some(ResolvedKey {
            public_key: entry.public_key,
            lifecycle,
        }))
    }
}
```

NOTE: confirm `IdentityVisibility::all()` exists. From `crates/cairn-core/src/contract/identity_registry.rs:51-57` (read earlier in research) it does — there's `confirmed`, `for_test`, plus visibility levels `Operational | Confirmed | All`. If the constructor name differs (e.g. `IdentityVisibility::All`), adapt.

- [ ] **Step 4: Re-export from `lib.rs`**

In `crates/cairn-store-sqlite/src/lib.rs`, add (alongside existing re-exports):

```rust
pub mod issuer_key_resolver;
pub use issuer_key_resolver::SqliteIssuerKeyResolver;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p cairn-store-sqlite --test issuer_key_resolver`
Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite
git commit -m "feat(store-sqlite): add SqliteIssuerKeyResolver wrapping IdentityRegistry (issue #51)"
```

---

### Task 14: Add the no-DB-write regression test (AC #1)

**Files:**
- Create: `crates/cairn-store-sqlite/tests/no_db_write_on_bad_envelope.rs`

- [ ] **Step 1: Write the test**

Create `crates/cairn-store-sqlite/tests/no_db_write_on_bad_envelope.rs`:

```rust
//! Issue #51 acceptance criterion 1: an invalid envelope must never
//! reach WAL preparation. This test exercises every `VerifyError`
//! producer through `verify_signed_intent` and asserts that the
//! `wal_ops` row count and every records-bearing table's row count
//! is unchanged after each rejection.

use std::sync::Arc;
use std::time::SystemTime;

use cairn_core::contract::issuer_key_resolver::{KeyLifecycle, ResolvedKey};
use cairn_core::generated::envelope::SignedIntentScopeTier;
use cairn_core::verifier::verify_signed_intent;
use cairn_test_fixtures::signed_intent::{FakeIssuerKeyResolver, SignedIntentFixture};

const TABLES_TO_PROBE: &[&str] = &[
    "wal_ops",
    "wal_steps",
    "records",
    "consent_journal",
];

async fn snapshot_counts(store: &cairn_store_sqlite::SqliteMemoryStore) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for table in TABLES_TO_PROBE {
        let n = store.row_count(table).await.unwrap_or(0);
        out.push(((*table).to_owned(), n));
    }
    out
}

async fn assert_no_writes_for(label: &str, intent: cairn_core::generated::envelope::SignedIntent, resolver: &FakeIssuerKeyResolver, now: SystemTime) {
    let store = cairn_test_fixtures::memstore().await;
    let before = snapshot_counts(&store).await;
    let outcome = verify_signed_intent(intent, resolver, now).await;
    assert!(outcome.is_err(), "[{label}] expected reject, got Ok");
    let after = snapshot_counts(&store).await;
    assert_eq!(before, after, "[{label}] DB row counts changed");
}

#[tokio::test]
async fn rejects_tampered_signature_no_writes() {
    let (mut intent, resolver, now) = SignedIntentFixture::default().build();
    // flip a hex char
    let mut chars: Vec<char> = intent.signature.0.chars().collect();
    let idx = chars.len() - 5;
    chars[idx] = if chars[idx] == 'a' { 'b' } else { 'a' };
    intent.signature = cairn_core::generated::common::Ed25519Signature(chars.into_iter().collect());
    assert_no_writes_for("tamper", intent, &resolver, now).await;
}

#[tokio::test]
async fn rejects_skewed_no_writes() {
    use chrono::{DateTime, Utc};
    let (intent, resolver, _) = SignedIntentFixture::default().build();
    // now 1 hour off
    let now: SystemTime = DateTime::parse_from_rfc3339("2026-04-22T15:30:00Z")
        .expect("rfc")
        .with_timezone(&Utc)
        .into();
    assert_no_writes_for("skewed", intent, &resolver, now).await;
}

#[tokio::test]
async fn rejects_past_no_writes() {
    use chrono::{DateTime, Utc};
    let (intent, resolver, _) = SignedIntentFixture::default().build();
    let now: SystemTime = DateTime::parse_from_rfc3339("2026-04-22T14:08:00Z")
        .expect("rfc")
        .with_timezone(&Utc)
        .into();
    assert_no_writes_for("past", intent, &resolver, now).await;
}

#[tokio::test]
async fn rejects_ttl_exceeded_no_writes() {
    let (mut intent, resolver, now) = SignedIntentFixture::default().build();
    intent.expires_at = "2026-04-22T14:30:11Z".to_owned(); // 28 min > 5 min cap
    assert_no_writes_for("ttl", intent, &resolver, now).await;
}

#[tokio::test]
async fn rejects_unknown_key_no_writes() {
    let (intent, _resolver, now) = SignedIntentFixture::default().build();
    let empty = FakeIssuerKeyResolver::new(); // empty table → UnknownKey
    assert_no_writes_for("unknown", intent, &empty, now).await;
}

#[tokio::test]
async fn rejects_revoked_no_writes() {
    let (intent, resolver, now) = SignedIntentFixture::default().build();
    // mutate the resolver entry to Revoked-pre-issued_at
    resolver.set("hmn:tafeng", 1, Some(ResolvedKey {
        public_key: [9u8; 32],
        lifecycle: KeyLifecycle::Revoked { effective_at: "2026-04-22T14:00:00Z".to_owned() },
    }));
    assert_no_writes_for("revoked", intent, &resolver, now).await;
}

#[tokio::test]
async fn rejects_scope_denied_no_writes() {
    let (intent, resolver, now) = SignedIntentFixture::builder()
        .issuer("agt:bot:opus:role:v1".to_owned())
        .tier(SignedIntentScopeTier::Team)
        .build_full();
    assert_no_writes_for("scope", intent, &resolver, now).await;
}
```

NOTE: `store.row_count(table)` is a helper assumed available on `SqliteMemoryStore`. If it doesn't exist, add a small inline helper that opens a read connection and runs `SELECT COUNT(*) FROM <table>`. Check `crates/cairn-store-sqlite/src/lib.rs` for an existing pattern; if absent, add a `pub(crate) async fn row_count(&self, table: &str) -> Result<i64, _>` next to the other test-helper-style methods, or use `tokio_rusqlite` directly inside the test:

```rust
let n: i64 = store.with_conn(|c| c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))).await?;
```

- [ ] **Step 2: Run the regression test**

Run: `cargo test -p cairn-store-sqlite --test no_db_write_on_bad_envelope`
Expected: 7 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/tests/no_db_write_on_bad_envelope.rs
git commit -m "test(verifier): regression test — bad envelopes never reach WAL prepare (issue #51 AC1)"
```

---

### Task 15: Final verification + module-level docs

**Files:**
- Modify: `crates/cairn-core/src/verifier.rs`
- Modify: `crates/cairn-core/src/intent/mod.rs`

- [ ] **Step 1: Refresh the module-level docs in `verifier.rs`**

Replace the file's leading `//!` comment block in `crates/cairn-core/src/verifier.rs` with the production-ready docstring:

```rust
//! `SignedIntent` verifier — the single production path that mints
//! [`crate::domain::VerifiedSignedIntent`] tokens.
//!
//! Brief sections: §4.2 (signed payload schema), §8.0.b (verb envelope).
//!
//! # Pipeline
//!
//! Five ordered checks. Any failure short-circuits with `Err(VerifyError::*)`
//! and no side effects:
//!
//! 1. **Syntactic post-parse defense in depth** — the generated
//!    `RawSignedIntent::TryFrom` already enforces every shape invariant
//!    on deserialization. This step re-checks the few that direct
//!    field-init within `cairn-core` could bypass: issuer prefix,
//!    exactly-one-of `sequence`/`server_challenge`, `key_version >= 1`.
//! 2. **Timestamp window** vs caller-supplied `now`: `|now − issued_at|
//!    ≤ 2 min`, `expires_at − issued_at ≤ 5 min`, `now ≤ expires_at`.
//! 3. **Issuer-kind ↔ scope-tier fit**: `snr:` → `private`,
//!    `agt:` → {`private`, `session`, `project`}, `hmn:` → all.
//! 4. **Resolver lookup** — `(issuer, key_version) → ResolvedKey`.
//!    None / Pending / Purged → `UnknownKey`; Revoked at or before
//!    `issued_at` → `RevokedKey`; Active or Revoked-future → continue.
//! 5. **Signature** — JCS-canonicalize envelope-minus-signature, parse
//!    the `ed25519:<128-hex>` blob and the resolved 32-byte pubkey,
//!    Ed25519 verify. Any failure → `InvalidSignature` (opaque).
//!
//! Replay/sequence consume (#52) and WAL `PREPARE` coupling (#55) are
//! **not** in this pipeline — they live in the SQLite WAL transaction
//! that wraps the verifier at the verb dispatch layer.
//!
//! # Example
//!
//! ```rust,no_run
//! # use cairn_core::verifier::verify_signed_intent;
//! # use cairn_core::contract::issuer_key_resolver::IssuerKeyResolver;
//! # use cairn_core::generated::envelope::SignedIntent;
//! # use std::time::SystemTime;
//! # async fn doc(intent: SignedIntent, resolver: &dyn IssuerKeyResolver) {
//! let verified = verify_signed_intent(intent, resolver, SystemTime::now())
//!     .await
//!     .expect("authentic envelope");
//! # }
//! ```
```

- [ ] **Step 2: Run the full verification checklist**

Run each (sequentially, since failures are independent diagnostics):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo deny check
```

Expected: every command passes.

- [ ] **Step 3: Spot-check the no-record-body privacy invariant**

Run:

```bash
RUST_LOG=trace cargo nextest run -p cairn-core --test verify_error_snapshots 2>&1 | grep -E "(target_hash|signature|public_key)" | grep -v "target_hash=sha256"
```

Expected: no matches outside the snapshot-test fixture strings (i.e., no log line above `debug` accidentally renders `target_hash` / `signature` / `public_key` raw bytes). If any line leaks, fix the offending instrument call's `fields(...)` list.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verifier.rs
git commit -m "docs(verifier): module-level pipeline docs + worked example (issue #51)"
```

---

### Task 16: Update the design doc + open the PR

**Files:**
- Modify: `docs/design/2026-05-04-issue-51-signed-intent-verifier-design.md`

- [ ] **Step 1: Flip the spec status**

Edit the design doc's header to `**Status:** implemented` (was `draft (proposed scope)`).

Add a short "Implementation notes" section at the bottom recording any deviations from the proposed design that came up during execution (e.g., `KeyVersion` accessor name, `IdentityVisibility::all` constructor name). One paragraph or one bulleted list, not a re-design.

- [ ] **Step 2: Commit + push**

```bash
git add docs/design/2026-05-04-issue-51-signed-intent-verifier-design.md
git commit -m "docs(design): mark signed-intent verifier spec as implemented (issue #51)"
git push -u origin HEAD
```

- [ ] **Step 3: Open the PR**

Run:

```bash
gh pr create --title "feat(verifier): real signed-intent verifier (issue #51)" --body "$(cat <<'EOF'
## Summary
- Replaces the cairn-core placeholder `verify_signed_intent` with a real five-step pipeline: post-parse syntactic defense in depth → timestamp window → issuer-kind ↔ tier fit → `IssuerKeyResolver` lookup → JCS-canonicalised Ed25519 signature verify.
- New tiny async `IssuerKeyResolver` contract trait in cairn-core; `SqliteIssuerKeyResolver` adapter wrapping `IdentityRegistry`.
- Strict scope: per-call verification only — replay ledger (#52) and WAL coupling (#55) stay in their own issues.

Brief sections: §4.2 (signed payload schema), §5.6 (WAL PREPARE coupling — referenced for boundary, not implemented), §8.0.b (verb envelope).
Spec: `docs/design/2026-05-04-issue-51-signed-intent-verifier-design.md`.

## Invariants touched
- #1 cairn-core stays I/O-free (verifier remains pure; resolver trait erases I/O behind `&dyn`).
- #4 New contract trait `IssuerKeyResolver`; SQLite adapter as separate crate-local impl.
- #6 `VerifyError` is non-exhaustive so `Replay` etc. land in #52 without breaking changes.
- #9 No record bodies / signature bytes / pubkey bytes logged above `debug`.

## Test plan
- [ ] `cargo nextest run --workspace --locked --no-fail-fast` (every new + existing test).
- [ ] `cargo test --doc --workspace --locked` (the rustdoc example block).
- [ ] `./scripts/check-core-boundary.sh` — no new workspace-crate deps in cairn-core.
- [ ] Property tests: canonical-envelope determinism + tamper invariants.
- [ ] Snapshot tests: every `VerifyError` Display variant, locked.
- [ ] Regression test `no_db_write_on_bad_envelope` — every reject variant leaves DB row counts unchanged (issue AC #1).
EOF
)"
```

Expected: PR URL printed; CI runs against the branch.

---

## Spec Coverage Map

| Spec section | Implemented in tasks |
|---|---|
| §3 Architecture (crate layout, deps) | 1, 2, 13 |
| §4.1 `IssuerKeyResolver` trait | 2 |
| §4.2 Canonical envelope | 4, 11 |
| §4.3 Verifier pipeline | 5, 6, 7, 8, 9 |
| §4.4 `VerifyError` enum | 3, 12 |
| §4.5 SQLite resolver adapter | 13 |
| §5 Data flow + tracing instrumentation | 9 step 7, 15 |
| §6 Error mapping | 5, 6, 7, 8, 9 (one variant per step) |
| §7.1 Unit tests | 5, 6, 7, 8, 9 |
| §7.2 Property tests | 11 |
| §7.3 Resolver lifecycle integration test | 13 |
| §7.4 No-DB-write regression test | 14 |
| §7.5 Display snapshots | 12 |
| §7.6 Doc tests | 15 step 1 |
| §8 Verification checklist | 15 step 2 |

Every spec section has at least one task entry. No gaps.
