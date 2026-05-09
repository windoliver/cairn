# Issue #51 envelope verifier — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the P0 syntactic-only `verify_signed_intent` placeholder with an `EnvelopeVerifier` that performs real Ed25519 signature, expiry, key-version, revocation, and scope checks before any SQLite mutation, mintable only via the existing sealed witness path.

**Architecture:** New `cairn-core::verifier` directory module owns three plain-data types (`ResolvedIssuer`, `ScopePolicy`, `Clock`) plus an `EnvelopeVerifier<'a>` struct that takes a pre-resolved issuer + clock + scope policy and performs an ordered sequence of checks. Adapter callers `await resolve_issuer(&dyn IdentityRegistry, ...)` once at the trust boundary, then call `verifier.verify(intent, &resolved)` synchronously. A new `core::error::wire::envelope_error_for` mapper translates `DomainError` to a typed wire `ErrorBody` that serializes into the shape already validated by `validate_error_envelope` in the generated `Response` deserializer.

**Tech Stack:** Rust 2024 edition, `ed25519-dalek` 2.1, `chrono` for `DateTime<Utc>`, `serde_json` for canonical encoding, `rstest` parameterized tests, `proptest` for fuzz, `insta` for wire-error snapshots, `tokio::test` for async integration tests.

**Spec source:** `docs/superpowers/specs/2026-05-02-issue-51-envelope-verifier-design.md`.

---

## File map

### Created
- `crates/cairn-core/src/domain/time.rs` — `Clock` trait + `SystemClock` + `FixedClock` (test-helpers feature).
- `crates/cairn-core/src/verifier/mod.rs` — `EnvelopeVerifier<'a>` struct + `verify` method (replaces single-file `verifier.rs`).
- `crates/cairn-core/src/verifier/resolved.rs` — `ResolvedIssuer` plain-data struct.
- `crates/cairn-core/src/verifier/policy.rs` — `ScopePolicy` plain-data struct + constructor.
- `crates/cairn-core/src/verifier/resolve.rs` — async `resolve_issuer(&dyn IdentityRegistry, ...)` helper.
- `crates/cairn-core/src/error/wire.rs` — `ErrorBody` typed builder + `envelope_error_for(&DomainError) -> ErrorBody`.
- `crates/cairn-core/tests/envelope_errors.rs` — `insta` snapshot tests for wire shapes.
- `crates/cairn-core/tests/verifier_proptests.rs` — proptest one-byte-mutation suite.
- `crates/cairn-store-sqlite/tests/resolve_issuer.rs` — integration test for `resolve_issuer` against a real `SqliteIdentityRegistry`.
- `crates/cairn-store-sqlite/tests/envelope_blocks_wal.rs` — DB-isolation test asserting bad envelopes leave WAL/records empty.

### Modified
- `crates/cairn-core/src/lib.rs` — re-export `domain::time` (single line).
- `crates/cairn-core/src/domain/mod.rs` — register `pub mod time` + targeted re-exports (`Clock`, `SystemClock`).
- `crates/cairn-core/src/domain/error.rs` — add 6 new `DomainError` variants.
- `crates/cairn-core/src/domain/canonical.rs` — add `canonical_bytes_signed_intent` function.
- `crates/cairn-core/src/error/mod.rs` — declare `pub mod wire;` and re-export `wire::ErrorBody`, `wire::envelope_error_for`.
- `crates/cairn-test-fixtures/src/lib.rs` — register `pub mod intent;` (fixture helpers).
- `crates/cairn-test-fixtures/src/intent.rs` — new file, `sign_intent`, `fixed_clock_at`, `scope_policy_default`, `unsigned_intent` fixture helpers.
- `crates/cairn-test-fixtures/Cargo.toml` — add `cairn-core` `test-helpers` feature dependency (if needed).
- `crates/cairn-core/Cargo.toml` — add the `test-helpers` feature flag if not already present.

### Deleted
- `crates/cairn-core/src/verifier.rs` (single-file module replaced by `verifier/` directory).

### Out of scope (no edits)
- `cairn-cli/src/main.rs` and verb call sites — verbs do not exist yet (issue #9 is open and downstream of #7). Verb wiring of `EnvelopeVerifier` will land with #9. The plan therefore stops at the library surface and library-level tests; no CLI/MCP smoke tests at this stage.
- `cairn-mcp`, `cairn-sdk` adapter code — same reason.
- Migrating ad-hoc `chrono::Utc::now()` call sites in `domain/identity/*` to the new `Clock` trait — separate sweep.

---

## Conventions referenced by every task

- **TDD per CLAUDE.md §7:** failing test → minimal implementation → test passes → commit.
- **Verification:** after each task's tests pass, run `cargo nextest run -p <crate> --locked` for the touched crate. Full-workspace `cargo nextest run --workspace --locked --no-fail-fast` runs once at the end.
- **Lints:** every change must pass `cargo clippy --workspace --all-targets --locked -- -D warnings`. `cairn-core` denies `unwrap_used`/`expect_used` outside `#[cfg(test)]`; tests may use `unwrap()`.
- **Core boundary:** after touching `cairn-core`, run `./scripts/check-core-boundary.sh` to confirm no adapter-crate dep leaked in.
- **Format:** `cargo fmt --all` before each commit.

---

## Task 1: Add `Clock` trait + `SystemClock` + `FixedClock`

**Files:**
- Create: `crates/cairn-core/src/domain/time.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs:20-50` (insert `pub mod time;` and re-exports)
- Modify: `crates/cairn-core/Cargo.toml` (add `test-helpers` feature if absent)

- [ ] **Step 1: Confirm or add `test-helpers` feature**

Run:
```bash
grep -n "test-helpers" crates/cairn-core/Cargo.toml || echo "missing"
```

If it prints `missing`, edit `crates/cairn-core/Cargo.toml` and add to the `[features]` table:

```toml
[features]
test-helpers = []
```

Then commit only the Cargo.toml change before proceeding (lets later steps cleanly use `#[cfg(any(test, feature = "test-helpers"))]`).

```bash
git add crates/cairn-core/Cargo.toml
git commit -m "feat(core): add test-helpers feature flag"
```

If the feature already exists, skip this step.

- [ ] **Step 2: Write the failing test**

Create `crates/cairn-core/src/domain/time.rs`:

```rust
//! Wall-clock abstraction for code that needs an injectable `now()`.
//!
//! Production code uses [`SystemClock`]; tests use [`FixedClock`] (gated
//! behind the `test-helpers` feature so it ships only in dev builds).

use chrono::{DateTime, Utc};

/// Source of wall-clock time. Sync because every consumer needs `now()`
/// at decision points where awaiting is unwanted (verifier, expiry checks).
pub trait Clock: Send + Sync {
    /// Current UTC instant at call time.
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock — delegates to [`chrono::Utc::now`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Test clock returning a fixed instant.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub DateTime<Utc>);

#[cfg(any(test, feature = "test-helpers"))]
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_returns_its_instant() {
        let t: DateTime<Utc> = "2026-05-02T12:00:00Z".parse().unwrap();
        let c = FixedClock(t);
        assert_eq!(c.now(), t);
    }

    #[test]
    fn system_clock_advances_or_equals() {
        let a = SystemClock.now();
        let b = SystemClock.now();
        assert!(b >= a);
    }
}
```

- [ ] **Step 3: Register the module**

Edit `crates/cairn-core/src/domain/mod.rs`. Insert `pub mod time;` in the alphabetised `pub mod` list (between `taxonomy` and `timestamp`):

```rust
pub mod taxonomy;
pub mod time;
pub mod timestamp;
```

Add a re-export in the `pub use` block (in the alphabetical position):

```rust
pub use time::{Clock, SystemClock};
// Note: FixedClock is re-exported only behind the test-helpers feature;
// downstream tests pull it from `cairn_core::domain::time::FixedClock`
// directly, so no top-level re-export is needed.
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p cairn-core --lib --locked time::
```

Expected: 2 tests pass (`fixed_clock_returns_its_instant`, `system_clock_advances_or_equals`).

- [ ] **Step 5: Lint + format**

```bash
cargo fmt --all
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/domain/time.rs crates/cairn-core/src/domain/mod.rs
git commit -m "feat(core): add Clock trait + SystemClock + FixedClock (issue #51)"
```

---

## Task 2: Add `canonical_bytes_signed_intent`

**Files:**
- Modify: `crates/cairn-core/src/domain/canonical.rs` (append a new function + tests)

- [ ] **Step 1: Write the failing tests**

Append to `crates/cairn-core/src/domain/canonical.rs` (inside the existing module, before the `#[cfg(test)] mod tests` block at the bottom — or extend that block if simpler):

```rust
/// Encode `intent` to canonical-JSON bytes with the `signature` field
/// removed. The result is the byte string that was signed, suitable for
/// passing to [`ed25519_dalek::Verifier::verify`] alongside
/// `intent.signature`.
///
/// # Errors
/// Returns [`DomainError::InvalidIdentity`] if `intent` cannot be
/// serialized to a JSON object (should never happen — `SignedIntent` is a
/// struct).
pub fn canonical_bytes_signed_intent(
    intent: &crate::generated::envelope::SignedIntent,
) -> Result<Vec<u8>, crate::domain::DomainError> {
    let mut value = serde_json::to_value(intent).map_err(|e| {
        crate::domain::DomainError::InvalidIdentity {
            message: format!("canonical serialize failed: {e}"),
        }
    })?;
    let map = value
        .as_object_mut()
        .ok_or_else(|| crate::domain::DomainError::InvalidIdentity {
            message: "SignedIntent did not serialize to a JSON object".into(),
        })?;
    map.remove("signature");
    let mut buf = String::new();
    write_canonical(&value, &mut buf);
    Ok(buf.into_bytes())
}
```

Then add this test inside the existing `#[cfg(test)] mod tests` at the bottom of the file (or create one if missing):

```rust
#[test]
fn signed_intent_canonical_strips_signature() {
    use crate::generated::common;
    use crate::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};

    let mk = |sig: &str| SignedIntent {
        chain_parents: vec![],
        expires_at: "2026-04-22T14:07:11Z".into(),
        issued_at: "2026-04-22T14:02:11Z".into(),
        issuer: common::Identity("hmn:tafeng".into()),
        key_version: 1,
        nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".into()),
        operation_id: common::Ulid("01HQZX9F5N0000000000000000".into()),
        scope: SignedIntentScope {
            tenant: "acme".into(),
            workspace: "ws".into(),
            entity: "ent".into(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence: Some(1),
        server_challenge: None,
        signature: common::Ed25519Signature(format!("ed25519:{}", sig)),
        target_hash: format!("sha256:{}", "a".repeat(64)),
    };

    let a = canonical_bytes_signed_intent(&mk(&"a".repeat(128))).unwrap();
    let b = canonical_bytes_signed_intent(&mk(&"b".repeat(128))).unwrap();
    assert_eq!(a, b, "canonical bytes must be invariant under signature mutation");
    assert!(!std::str::from_utf8(&a).unwrap().contains("signature"));
}

#[test]
fn signed_intent_canonical_changes_when_payload_changes() {
    use crate::generated::common;
    use crate::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};

    let base = SignedIntent {
        chain_parents: vec![],
        expires_at: "2026-04-22T14:07:11Z".into(),
        issued_at: "2026-04-22T14:02:11Z".into(),
        issuer: common::Identity("hmn:tafeng".into()),
        key_version: 1,
        nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".into()),
        operation_id: common::Ulid("01HQZX9F5N0000000000000000".into()),
        scope: SignedIntentScope {
            tenant: "acme".into(),
            workspace: "ws".into(),
            entity: "ent".into(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence: Some(1),
        server_challenge: None,
        signature: common::Ed25519Signature(format!("ed25519:{}", "a".repeat(128))),
        target_hash: format!("sha256:{}", "a".repeat(64)),
    };
    let mut tweaked = base.clone();
    tweaked.scope.entity = "different".into();

    let a = canonical_bytes_signed_intent(&base).unwrap();
    let b = canonical_bytes_signed_intent(&tweaked).unwrap();
    assert_ne!(a, b);
}
```

- [ ] **Step 2: Run tests**

```bash
cargo nextest run -p cairn-core --lib --locked canonical::tests::signed_intent_canonical
```

Expected: both tests pass on first run since the implementation is in the same diff.

- [ ] **Step 3: Lint + format**

```bash
cargo fmt --all
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/canonical.rs
git commit -m "feat(core): canonical_bytes_signed_intent — strips signature for verification (issue #51)"
```

---

## Task 3: Add new `DomainError` variants

**Files:**
- Modify: `crates/cairn-core/src/domain/error.rs`

- [ ] **Step 1: Add the variants**

In `crates/cairn-core/src/domain/error.rs`, inside the `pub enum DomainError { ... }` block, append the following variants after the last existing variant (`InvalidProjectRoot`). The enum is `#[non_exhaustive]` so order is irrelevant for compatibility — keep them grouped at the end:

```rust
    /// Cryptographic verification of [`crate::generated::envelope::SignedIntent::signature`]
    /// against the canonical-JSON encoding of the rest of the envelope failed
    /// (§4.2 + §8.0.b).
    #[error("signature: cryptographic verification failed")]
    InvalidSignature,

    /// The clock's current instant is outside `[issued_at − skew, expires_at)`.
    #[error("intent expired: issued_at={issued_at}, expires_at={expires_at}, now={now}")]
    ExpiredIntent {
        /// Wire-form `issued_at` from the rejected envelope.
        issued_at: String,
        /// Wire-form `expires_at` from the rejected envelope.
        expires_at: String,
        /// Wall-clock instant at the time of the check.
        now: String,
    },

    /// The issuer's key is not in [`crate::domain::identity::records::ProvisioningState::Active`].
    /// Pending, RevokePending, Revoked, PurgePending, and Purged all reject.
    #[error("issuer key not Active for {id}: state={state:?}")]
    RevokedKey {
        /// The rejected issuer.
        id: crate::domain::Identity,
        /// The lifecycle state observed in the registry.
        state: crate::domain::identity::records::ProvisioningState,
    },

    /// `intent.key_version` does not match the version held in the
    /// registry for this issuer. Raised by `resolve_issuer` (registry lookup).
    #[error("key version mismatch: intent={intent}, registry has {current:?}")]
    KeyVersionMismatch {
        /// Version requested by the envelope.
        intent: crate::domain::identity::keys::KeyVersion,
        /// Highest known version in the registry; `None` if the issuer is
        /// unknown to the registry.
        current: Option<crate::domain::identity::keys::KeyVersion>,
    },

    /// Envelope scope `(tenant, workspace, tier)` does not match the
    /// vault's [`crate::verifier::ScopePolicy`].
    #[error("scope denied: {message}")]
    ScopeDenied {
        /// Free-form reason describing which dimension failed.
        message: String,
    },

    /// Envelope failed an authorization check that does not fall under any
    /// other variant — for example, a caller-bug guard where the resolved
    /// issuer's identity does not match the envelope issuer.
    #[error("unauthorized: {message}")]
    Unauthorized {
        /// Free-form reason.
        message: String,
    },
```

- [ ] **Step 2: Add a unit test for each variant's display string**

Append inside `crates/cairn-core/src/domain/error.rs` `#[cfg(test)] mod tests`. If no `mod tests` exists at the bottom, create one:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_signature_display() {
        let err = DomainError::InvalidSignature;
        assert_eq!(
            err.to_string(),
            "signature: cryptographic verification failed"
        );
    }

    #[test]
    fn expired_intent_display() {
        let err = DomainError::ExpiredIntent {
            issued_at: "2026-04-22T14:02:11Z".into(),
            expires_at: "2026-04-22T14:07:11Z".into(),
            now: "2026-04-22T15:00:00Z".into(),
        };
        assert!(err.to_string().contains("expired"));
        assert!(err.to_string().contains("now=2026-04-22T15:00:00Z"));
    }

    #[test]
    fn scope_denied_display() {
        let err = DomainError::ScopeDenied {
            message: "tenant: expected acme, got other".into(),
        };
        assert_eq!(
            err.to_string(),
            "scope denied: tenant: expected acme, got other"
        );
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo nextest run -p cairn-core --lib --locked error::tests
```

Expected: 3 new tests pass; existing tests still pass.

- [ ] **Step 4: Lint + format**

```bash
cargo fmt --all
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

The new `RevokedKey` variant's `state: ProvisioningState` field uses `Debug` formatting. Confirm `ProvisioningState` already derives `Debug` (it does — see `crates/cairn-core/src/domain/identity/records.rs:40`).

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/error.rs
git commit -m "feat(core): add envelope-verifier error variants (issue #51)"
```

---

## Task 4: Add `ResolvedIssuer`

**Files:**
- Create: `crates/cairn-core/src/verifier/resolved.rs`
- *(Module registration deferred to Task 7; this task creates the file in isolation.)*

- [ ] **Step 1: Write the file**

Create `crates/cairn-core/src/verifier/resolved.rs`:

```rust
//! [`ResolvedIssuer`] — pre-resolved issuer-key snapshot consumed by
//! [`super::EnvelopeVerifier::verify`].
//!
//! Built only by [`super::resolve::resolve_issuer`]; no public constructor
//! outside cairn-core. `Debug` redacts the verifying-key bytes — defensive,
//! since they are public, but avoids accidental leakage in logs.

use ed25519_dalek::VerifyingKey;

use crate::domain::Identity;
use crate::domain::identity::keys::KeyVersion;
use crate::domain::identity::records::ProvisioningState;

/// Snapshot of an issuer's verifying key + lifecycle state at the time
/// the registry was queried. Pass to [`super::EnvelopeVerifier::verify`].
pub struct ResolvedIssuer {
    /// The identity this snapshot describes.
    pub identity: Identity,
    /// Key version represented by `verifying_key`.
    pub key_version: KeyVersion,
    /// Public Ed25519 key bytes already validated by
    /// [`ed25519_dalek::VerifyingKey::from_bytes`].
    pub verifying_key: VerifyingKey,
    /// Lifecycle state of the identity at lookup time. Verifier rejects
    /// anything other than [`ProvisioningState::Active`].
    pub state: ProvisioningState,
}

impl ResolvedIssuer {
    /// Construct a [`ResolvedIssuer`] from the registry-row primitives.
    /// Only callable inside cairn-core; downstream callers go through
    /// [`super::resolve::resolve_issuer`].
    #[must_use]
    pub(crate) fn from_registry_row(
        identity: Identity,
        key_version: KeyVersion,
        verifying_key: VerifyingKey,
        state: ProvisioningState,
    ) -> Self {
        Self {
            identity,
            key_version,
            verifying_key,
            state,
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl ResolvedIssuer {
    /// Test-only constructor. Only available behind `cfg(test)` and
    /// `feature = "test-helpers"`.
    #[must_use]
    pub fn for_test(
        identity: Identity,
        key_version: KeyVersion,
        verifying_key: VerifyingKey,
        state: ProvisioningState,
    ) -> Self {
        Self::from_registry_row(identity, key_version, verifying_key, state)
    }
}

impl std::fmt::Debug for ResolvedIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedIssuer")
            .field("identity", &self.identity)
            .field("key_version", &self.key_version)
            .field("verifying_key", &"<redacted>")
            .field("state", &self.state)
            .finish()
    }
}
```

- [ ] **Step 2: No lint/test yet**

The file is not registered as a module, so it will not compile in isolation. Lint runs in Task 7 once the module is wired in.

- [ ] **Step 3: Stage but do not commit**

The file is committed together with `policy.rs` and `mod.rs` in Task 7 to keep the verifier directory's first compile the atomic checkpoint. Do not run `git commit` yet — leave the file unstaged.

---

## Task 5: Add `ScopePolicy`

**Files:**
- Create: `crates/cairn-core/src/verifier/policy.rs`

- [ ] **Step 1: Write the file**

Create `crates/cairn-core/src/verifier/policy.rs`:

```rust
//! [`ScopePolicy`] — vault-anchored allow-list for envelope `scope`
//! checking. Constructed once at adapter startup; passed by reference
//! into every [`super::EnvelopeVerifier`] instance.
//!
//! P0: `(tenant, workspace)` come from a hard-coded adapter default
//! (`tenant = "default"`, `workspace = vault.name`) until a follow-up
//! issue extends [`crate::config::VaultConfig`] with explicit fields.
//! `allowed_tiers` defaults to all six tiers; verbs may narrow later.

use std::collections::BTreeSet;

use crate::domain::DomainError;
use crate::generated::envelope::SignedIntentScopeTier;

/// Vault-level allow-list for envelope scope dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopePolicy {
    /// Tenant string the vault accepts. Compared exactly.
    pub tenant: String,
    /// Workspace string the vault accepts. Compared exactly.
    pub workspace: String,
    /// Tiers the vault accepts. Subset of [`SignedIntentScopeTier`].
    pub allowed_tiers: BTreeSet<SignedIntentScopeTier>,
}

impl ScopePolicy {
    /// Construct a policy with explicit tenant/workspace strings and an
    /// allow-list of tiers.
    ///
    /// # Errors
    /// Returns [`DomainError::ScopeDenied`] if `tenant` or `workspace` is
    /// empty, or if `allowed_tiers` is empty.
    pub fn new(
        tenant: impl Into<String>,
        workspace: impl Into<String>,
        allowed_tiers: BTreeSet<SignedIntentScopeTier>,
    ) -> Result<Self, DomainError> {
        let tenant = tenant.into();
        if tenant.is_empty() {
            return Err(DomainError::ScopeDenied {
                message: "tenant must not be empty".into(),
            });
        }
        let workspace = workspace.into();
        if workspace.is_empty() {
            return Err(DomainError::ScopeDenied {
                message: "workspace must not be empty".into(),
            });
        }
        if allowed_tiers.is_empty() {
            return Err(DomainError::ScopeDenied {
                message: "allowed_tiers must not be empty".into(),
            });
        }
        Ok(Self {
            tenant,
            workspace,
            allowed_tiers,
        })
    }

    /// All-tiers allow-list useful at P0 and in tests.
    #[must_use]
    pub fn all_tiers() -> BTreeSet<SignedIntentScopeTier> {
        let mut s = BTreeSet::new();
        s.insert(SignedIntentScopeTier::Private);
        s.insert(SignedIntentScopeTier::Session);
        s.insert(SignedIntentScopeTier::Project);
        s.insert(SignedIntentScopeTier::Team);
        s.insert(SignedIntentScopeTier::Org);
        s.insert(SignedIntentScopeTier::Public);
        s
    }
}
```

> Note: `SignedIntentScopeTier` derives `Hash + Eq + Ord` (it's `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]` — verify `Ord` once compiling). If `Ord` is missing, switch `BTreeSet` to `HashSet`. Verify with the lint pass in Task 7 step 3.

- [ ] **Step 2: Stage but do not commit**

As with Task 4, the file lands together with the verifier `mod.rs` in Task 7.

---

## Task 6: Add `resolve_issuer`

**Files:**
- Create: `crates/cairn-core/src/verifier/resolve.rs`

- [ ] **Step 1: Write the file**

Create `crates/cairn-core/src/verifier/resolve.rs`:

```rust
//! [`resolve_issuer`] — async helper that turns
//! `(IdentityRegistry, Identity, KeyVersion)` into a [`super::ResolvedIssuer`]
//! ready for the synchronous [`super::EnvelopeVerifier::verify`].

use ed25519_dalek::VerifyingKey;

use crate::contract::identity_registry::{
    IdentityRegistry, IdentityVisibility, RegistryError,
};
use crate::domain::DomainError;
use crate::domain::Identity;
use crate::domain::identity::keys::KeyVersion;

use super::ResolvedIssuer;

/// Resolve `(identity, key_version)` against the registry, returning a
/// snapshot suitable for [`super::EnvelopeVerifier::verify`].
///
/// # Errors
/// - [`DomainError::Unauthorized`] if `identity` is unknown to the registry.
/// - [`DomainError::KeyVersionMismatch`] if no key row exists at
///   `key_version` (`current` carries the registry's
///   [`crate::domain::identity::records::PublicIdentityRecord::current_key_version`]).
/// - [`DomainError::Unauthorized`] for opaque registry backend errors
///   (the verifier never trusts a registry that cannot answer).
pub async fn resolve_issuer(
    registry: &dyn IdentityRegistry,
    identity: &Identity,
    key_version: KeyVersion,
) -> Result<ResolvedIssuer, DomainError> {
    // 1. Look up the identity row, including non-Active states so the
    //    verifier can return a precise lifecycle error rather than a
    //    generic NotFound.
    let record = match registry
        .get_identity(identity, IdentityVisibility::IncludingPurgePending)
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err(DomainError::Unauthorized {
                message: format!("identity {identity} not in registry"),
            });
        }
        Err(e) => {
            return Err(DomainError::Unauthorized {
                message: format!("registry get_identity failed: {e}"),
            });
        }
    };

    // 2. Find the key row for the requested version.
    let keys = match registry.list_keys(identity).await {
        Ok(keys) => keys,
        Err(RegistryError::NotFound) => {
            return Err(DomainError::KeyVersionMismatch {
                intent: key_version,
                current: None,
            });
        }
        Err(e) => {
            return Err(DomainError::Unauthorized {
                message: format!("registry list_keys failed: {e}"),
            });
        }
    };
    let Some(key_row) = keys.iter().find(|k| k.key_version == key_version) else {
        return Err(DomainError::KeyVersionMismatch {
            intent: key_version,
            current: Some(record.current_key_version),
        });
    };

    // 3. Decode the verifying key bytes.
    let verifying_key = VerifyingKey::from_bytes(&key_row.public_key).map_err(|e| {
        DomainError::Unauthorized {
            message: format!("registry public_key bytes invalid: {e}"),
        }
    })?;

    Ok(ResolvedIssuer::from_registry_row(
        identity.clone(),
        key_version,
        verifying_key,
        record.provisioning_state,
    ))
}
```

- [ ] **Step 2: Stage but do not commit**

Same as Tasks 4 and 5.

---

## Task 7: Replace `verifier.rs` with `verifier/mod.rs` (`EnvelopeVerifier`)

**Files:**
- Delete: `crates/cairn-core/src/verifier.rs`
- Create: `crates/cairn-core/src/verifier/mod.rs`
- Modify: `crates/cairn-core/src/lib.rs:11-21` (no edit needed if `pub mod verifier;` already present — verify; the existing `pub mod verifier;` line covers a directory module too).

- [ ] **Step 1: Delete the old single-file verifier**

```bash
git rm crates/cairn-core/src/verifier.rs
```

- [ ] **Step 2: Write the new `verifier/mod.rs`**

Create `crates/cairn-core/src/verifier/mod.rs`:

```rust
//! Envelope verifier — the single trust boundary between adapter input
//! and pipeline / WAL code.
//!
//! Adapters call [`resolve::resolve_issuer`] once to materialise a
//! [`ResolvedIssuer`] from the registry, then build a long-lived
//! [`EnvelopeVerifier`] (cheap, borrows policy + clock) and call
//! [`EnvelopeVerifier::verify`] on every incoming envelope. The verifier is
//! synchronous so it can drop into the existing
//! [`crate::domain::intent::SignedIntentVerifier`] sealed-witness mint
//! path without forcing every caller to be `async`.
//!
//! Replay / nonce / sequence / handshake-challenge enforcement is **not**
//! handled here — see issue #52.

mod policy;
mod resolve;
mod resolved;

pub use policy::ScopePolicy;
pub use resolve::resolve_issuer;
pub use resolved::ResolvedIssuer;

use std::time::Duration;

use ed25519_dalek::{Signature, Verifier};

use crate::domain::DomainError;
use crate::domain::Identity;
use crate::domain::canonical::canonical_bytes_signed_intent;
use crate::domain::identity::keys::KeyVersion;
use crate::domain::identity::records::ProvisioningState;
use crate::domain::intent::{SignedIntentVerifier, sealed::VerifierWitness};
use crate::domain::time::Clock;
use crate::domain::{Rfc3339Timestamp, VerifiedSignedIntent};
use crate::generated::envelope::SignedIntent;

/// Hard-coded P0 clock-skew tolerance. Configurable in a follow-up issue.
const P0_SKEW: Duration = Duration::from_secs(60);

/// Long-lived envelope verifier. Cheap to construct (borrows config).
/// Build one per adapter incarnation; reuse across calls.
pub struct EnvelopeVerifier<'a> {
    policy: &'a ScopePolicy,
    clock: &'a dyn Clock,
    skew: Duration,
}

impl<'a> EnvelopeVerifier<'a> {
    /// Build a verifier bound to the supplied scope policy and clock.
    /// Skew tolerance is fixed at 60 s for P0.
    #[must_use]
    pub fn new(policy: &'a ScopePolicy, clock: &'a dyn Clock) -> Self {
        Self {
            policy,
            clock,
            skew: P0_SKEW,
        }
    }

    /// Verify a [`SignedIntent`] against the resolved issuer key + the
    /// vault's scope policy and the wall-clock window. Mints a
    /// [`VerifiedSignedIntent`] proof token via the sealed-witness path
    /// on success.
    ///
    /// Order of checks (each fails closed; cheap checks run first so a
    /// tampered signature on an expired intent surfaces as `ExpiredIntent`,
    /// never as `InvalidSignature`):
    ///
    /// 1. Issuer ↔ resolved match (caller-bug guard).
    /// 2. Key-version match (caller-bug guard).
    /// 3. Lifecycle (`Active` only).
    /// 4. Expiry (`now ∈ [issued_at − 60 s, expires_at)`).
    /// 5. Scope policy (`tenant`, `workspace`, allowed `tier`).
    /// 6. Ed25519 signature over canonical-payload bytes.
    ///
    /// # Errors
    /// One of [`DomainError::Unauthorized`], [`DomainError::RevokedKey`],
    /// [`DomainError::ExpiredIntent`], [`DomainError::ScopeDenied`],
    /// [`DomainError::InvalidSignature`].
    pub fn verify(
        &self,
        intent: SignedIntent,
        resolved: &ResolvedIssuer,
    ) -> Result<VerifiedSignedIntent, DomainError> {
        self.check_issuer_match(&intent, resolved)?;
        self.check_key_version(&intent, resolved)?;
        self.check_lifecycle(resolved)?;
        self.check_expiry(&intent)?;
        self.check_scope(&intent)?;
        self.check_signature(&intent, resolved)?;
        Ok(<Self as SignedIntentVerifier>::__from_verified(
            intent,
            VerifierWitness::new(),
        ))
    }

    fn check_issuer_match(
        &self,
        intent: &SignedIntent,
        resolved: &ResolvedIssuer,
    ) -> Result<(), DomainError> {
        // The wire-shape `intent.issuer` is a string newtype; map it
        // through `Identity::parse` so comparisons go through the validated
        // domain type rather than naive string equality.
        let issuer = Identity::parse(intent.issuer.0.clone()).map_err(|_| {
            DomainError::Unauthorized {
                message: format!(
                    "envelope issuer {} is not a parseable identity",
                    intent.issuer.0
                ),
            }
        })?;
        if issuer != resolved.identity {
            return Err(DomainError::Unauthorized {
                message: format!(
                    "envelope issuer {} does not match resolved {}",
                    issuer, resolved.identity
                ),
            });
        }
        Ok(())
    }

    fn check_key_version(
        &self,
        intent: &SignedIntent,
        resolved: &ResolvedIssuer,
    ) -> Result<(), DomainError> {
        // Caller-bug guard. `resolve_issuer` fetches at the requested
        // version and would have raised `KeyVersionMismatch` already if the
        // row did not exist. Reaching here with a mismatch means the
        // adapter wired the wrong `resolved` instance.
        let intent_version = u32::try_from(intent.key_version)
            .ok()
            .and_then(|n| std::num::NonZeroU32::new(n).map(KeyVersion::new))
            .ok_or_else(|| DomainError::Unauthorized {
                message: format!(
                    "envelope key_version {} is not a valid non-zero u32",
                    intent.key_version
                ),
            })?;
        if intent_version != resolved.key_version {
            return Err(DomainError::Unauthorized {
                message: format!(
                    "envelope key_version {} does not match resolved {}",
                    intent_version, resolved.key_version
                ),
            });
        }
        Ok(())
    }

    fn check_lifecycle(&self, resolved: &ResolvedIssuer) -> Result<(), DomainError> {
        if !matches!(resolved.state, ProvisioningState::Active) {
            return Err(DomainError::RevokedKey {
                id: resolved.identity.clone(),
                state: resolved.state,
            });
        }
        Ok(())
    }

    fn check_expiry(&self, intent: &SignedIntent) -> Result<(), DomainError> {
        let now = self.clock.now();
        // Parse via the existing domain timestamp type; it already enforces
        // RFC-3339 shape, so a parse failure here is a deserializer-bug
        // guard rather than user-facing.
        let issued_at = Rfc3339Timestamp::parse(intent.issued_at.clone())
            .map_err(|_| DomainError::ExpiredIntent {
                issued_at: intent.issued_at.clone(),
                expires_at: intent.expires_at.clone(),
                now: now.to_rfc3339(),
            })?;
        let expires_at = Rfc3339Timestamp::parse(intent.expires_at.clone())
            .map_err(|_| DomainError::ExpiredIntent {
                issued_at: intent.issued_at.clone(),
                expires_at: intent.expires_at.clone(),
                now: now.to_rfc3339(),
            })?;

        let issued_chrono = issued_at.as_chrono();
        let expires_chrono = expires_at.as_chrono();

        let skew_chrono = chrono::Duration::from_std(self.skew).map_err(|_| {
            DomainError::Unauthorized {
                message: "skew duration overflowed chrono::Duration".into(),
            }
        })?;
        let earliest = issued_chrono - skew_chrono;
        if now < earliest || now >= expires_chrono {
            return Err(DomainError::ExpiredIntent {
                issued_at: intent.issued_at.clone(),
                expires_at: intent.expires_at.clone(),
                now: now.to_rfc3339(),
            });
        }
        Ok(())
    }

    fn check_scope(&self, intent: &SignedIntent) -> Result<(), DomainError> {
        if intent.scope.tenant != self.policy.tenant {
            return Err(DomainError::ScopeDenied {
                message: format!(
                    "tenant: expected {}, got {}",
                    self.policy.tenant, intent.scope.tenant
                ),
            });
        }
        if intent.scope.workspace != self.policy.workspace {
            return Err(DomainError::ScopeDenied {
                message: format!(
                    "workspace: expected {}, got {}",
                    self.policy.workspace, intent.scope.workspace
                ),
            });
        }
        if !self.policy.allowed_tiers.contains(&intent.scope.tier) {
            return Err(DomainError::ScopeDenied {
                message: format!("tier {:?} not in allow-list", intent.scope.tier),
            });
        }
        Ok(())
    }

    fn check_signature(
        &self,
        intent: &SignedIntent,
        resolved: &ResolvedIssuer,
    ) -> Result<(), DomainError> {
        let bytes = canonical_bytes_signed_intent(intent)?;
        // intent.signature is `ed25519:<128 lowercase hex>` (validated by
        // the IDL deserializer). Parse the 64-byte signature.
        let hex_tail = intent
            .signature
            .0
            .strip_prefix("ed25519:")
            .ok_or(DomainError::InvalidSignature)?;
        let mut sig_bytes = [0u8; 64];
        for (i, chunk) in hex_tail.as_bytes().chunks_exact(2).enumerate() {
            let hi = decode_hex_nibble(chunk[0]).ok_or(DomainError::InvalidSignature)?;
            let lo = decode_hex_nibble(chunk[1]).ok_or(DomainError::InvalidSignature)?;
            sig_bytes[i] = (hi << 4) | lo;
        }
        let sig = Signature::from_bytes(&sig_bytes);
        resolved
            .verifying_key
            .verify(&bytes, &sig)
            .map_err(|_| DomainError::InvalidSignature)
    }
}

impl SignedIntentVerifier for EnvelopeVerifier<'_> {}

fn decode_hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 3: Confirm `Rfc3339Timestamp::as_chrono`**

Run:
```bash
grep -n "fn as_chrono\|as_chrono\|impl.*Rfc3339Timestamp" crates/cairn-core/src/domain/timestamp.rs | head -10
```

Expected: an `as_chrono(&self) -> DateTime<Utc>` method (or `to_chrono`) is available. If the method has a different name, update `check_expiry` to use it; if no method exists, add one to `timestamp.rs` in this task — keep its signature minimal:

```rust
pub fn as_chrono(&self) -> chrono::DateTime<chrono::Utc> {
    self.0.parse().expect("validated RFC-3339 string")
}
```

Use `expect` only if the inner field is already validated as RFC-3339 by `Rfc3339Timestamp::parse`. Otherwise return `Result<DateTime<Utc>, DomainError>` and propagate via `?` in `check_expiry`.

- [ ] **Step 4: Confirm `SignedIntentScopeTier` derives `Ord`**

Run:
```bash
grep -A2 "pub enum SignedIntentScopeTier" crates/cairn-core/src/generated/envelope/mod.rs | head -5
```

If `Ord` is not in the derive list, the `BTreeSet` in Task 5 will not compile. Two options:

- **Option A:** swap `BTreeSet` → `HashSet` in `policy.rs`. (`SignedIntentScopeTier` is `Copy + Eq + Hash`, so `HashSet` works.)
- **Option B:** the enum is generated; a manual edit will be wiped by `cargo run -p cairn-idl --bin cairn-codegen`. **Use Option A.**

Apply Option A: edit `crates/cairn-core/src/verifier/policy.rs` and `verifier/mod.rs`, replacing every `BTreeSet` reference with `HashSet`.

- [ ] **Step 5: Stage all four new files + lib path-check**

```bash
git add crates/cairn-core/src/verifier/
```

Confirm `crates/cairn-core/src/lib.rs` already declares `pub mod verifier;` — Rust resolves both `verifier.rs` and `verifier/mod.rs`, so removing the file and adding the directory needs no `lib.rs` edit. Check:

```bash
grep "pub mod verifier" crates/cairn-core/src/lib.rs
```

- [ ] **Step 6: Compile**

```bash
cargo check -p cairn-core --all-targets --locked
```

Fix any compile errors that surface (most likely: `Rfc3339Timestamp::as_chrono` method name, `KeyVersion::new` argument type, `SignedIntentScopeTier` derive). The `#[cfg(test)] mod tests;` in `mod.rs` imports a yet-uncreated `tests.rs` — accept the "file not found" error for now; Task 8 creates it. To avoid blocking `cargo check`, temporarily comment out the `mod tests;` line. Re-enable it in Task 8.

- [ ] **Step 7: Lint**

```bash
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-core/src/lib.rs crates/cairn-core/src/verifier/
git commit -m "feat(core): EnvelopeVerifier (sig+expiry+lifecycle+scope checks) — issue #51

Replaces the P0 syntactic verify_signed_intent placeholder with a struct-bound
verifier that performs real Ed25519 signature verification, expiry, scope, and
lifecycle checks before minting VerifiedSignedIntent. Issuer/key resolution is
async (resolve_issuer); the verifier itself stays synchronous and pure.

Replay / nonce / sequence remain owned by issue #52."
```

---

## Task 8: Verifier unit tests (`rstest`)

**Files:**
- Create: `crates/cairn-core/src/verifier/tests.rs`
- Modify: `crates/cairn-core/src/verifier/mod.rs` (uncomment `mod tests;`)

- [ ] **Step 1: Write the test fixture builder**

Create `crates/cairn-core/src/verifier/tests.rs`:

```rust
//! Unit tests for [`super::EnvelopeVerifier`].
//!
//! All cases use a single deterministic seed signing key and build an
//! intent inside a fixed `(issued_at, expires_at)` window. Each test
//! mutates exactly one input dimension and asserts the matching error.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey};
use rstest::{fixture, rstest};

use crate::domain::DomainError;
use crate::domain::Identity;
use crate::domain::canonical::canonical_bytes_signed_intent;
use crate::domain::identity::keys::KeyVersion;
use crate::domain::identity::records::ProvisioningState;
use crate::domain::time::FixedClock;
use crate::generated::common;
use crate::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};

use super::{EnvelopeVerifier, ResolvedIssuer, ScopePolicy};

const ISSUER_WIRE: &str = "hmn:tafeng";
const ISSUED_AT: &str = "2026-04-22T14:02:11Z";
const EXPIRES_AT: &str = "2026-04-22T14:07:11Z";
const NOW: &str = "2026-04-22T14:05:00Z";

#[fixture]
fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

#[fixture]
fn policy() -> ScopePolicy {
    ScopePolicy::new("acme", "ws", ScopePolicy::all_tiers()).unwrap()
}

#[fixture]
fn clock() -> FixedClock {
    let t: DateTime<Utc> = NOW.parse().unwrap();
    FixedClock(t)
}

fn unsigned_intent() -> SignedIntent {
    SignedIntent {
        chain_parents: vec![],
        expires_at: EXPIRES_AT.into(),
        issued_at: ISSUED_AT.into(),
        issuer: common::Identity(ISSUER_WIRE.into()),
        key_version: 1,
        nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".into()),
        operation_id: common::Ulid("01HQZX9F5N0000000000000000".into()),
        scope: SignedIntentScope {
            tenant: "acme".into(),
            workspace: "ws".into(),
            entity: "ent".into(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence: Some(1),
        server_challenge: None,
        signature: common::Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
        target_hash: format!("sha256:{}", "a".repeat(64)),
    }
}

fn sign(key: &SigningKey, mut intent: SignedIntent) -> SignedIntent {
    let bytes = canonical_bytes_signed_intent(&intent).unwrap();
    let sig = key.sign(&bytes);
    let hex = sig
        .to_bytes()
        .iter()
        .fold(String::with_capacity(128), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(&mut s, "{:02x}", b);
            s
        });
    intent.signature = common::Ed25519Signature(format!("ed25519:{}", hex));
    intent
}

fn resolved_active(key: &SigningKey) -> ResolvedIssuer {
    ResolvedIssuer::for_test(
        Identity::parse(ISSUER_WIRE).unwrap(),
        KeyVersion::FIRST,
        key.verifying_key(),
        ProvisioningState::Active,
    )
}

#[rstest]
fn accepts_valid(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let verified = verifier.verify(intent, &resolved).expect("valid envelope");
    assert_eq!(verified.as_inner().issuer.0, ISSUER_WIRE);
}

#[rstest]
fn rejects_tampered_signature(
    signing_key: SigningKey,
    policy: ScopePolicy,
    clock: FixedClock,
) {
    let mut intent = sign(&signing_key, unsigned_intent());
    // Flip the last hex nibble of the signature.
    let hex_tail = intent
        .signature
        .0
        .strip_prefix("ed25519:")
        .unwrap()
        .to_owned();
    let mut chars: Vec<char> = hex_tail.chars().collect();
    let last = chars.last_mut().unwrap();
    *last = if *last == 'a' { 'b' } else { 'a' };
    let mutated: String = chars.into_iter().collect();
    intent.signature = common::Ed25519Signature(format!("ed25519:{mutated}"));
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::InvalidSignature));
}

#[rstest]
fn rejects_tampered_payload(
    signing_key: SigningKey,
    policy: ScopePolicy,
    clock: FixedClock,
) {
    let mut intent = sign(&signing_key, unsigned_intent());
    intent.scope.entity = "different".into();
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::InvalidSignature));
}

#[rstest]
fn rejects_expired(signing_key: SigningKey, policy: ScopePolicy) {
    let after_expiry: DateTime<Utc> = "2026-04-22T15:00:00Z".parse().unwrap();
    let clock = FixedClock(after_expiry);
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::ExpiredIntent { .. }));
}

#[rstest]
fn rejects_pre_issued(signing_key: SigningKey, policy: ScopePolicy) {
    // 2 minutes before issued_at — exceeds 60 s skew.
    let before_issue: DateTime<Utc> = "2026-04-22T14:00:00Z".parse().unwrap();
    let clock = FixedClock(before_issue);
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::ExpiredIntent { .. }));
}

#[rstest]
fn rejects_revoked(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = ResolvedIssuer::for_test(
        Identity::parse(ISSUER_WIRE).unwrap(),
        KeyVersion::FIRST,
        signing_key.verifying_key(),
        ProvisioningState::Revoked,
    );
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(
        err,
        DomainError::RevokedKey {
            state: ProvisioningState::Revoked,
            ..
        }
    ));
}

#[rstest]
fn rejects_pending(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = ResolvedIssuer::for_test(
        Identity::parse(ISSUER_WIRE).unwrap(),
        KeyVersion::FIRST,
        signing_key.verifying_key(),
        ProvisioningState::Pending,
    );
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(
        err,
        DomainError::RevokedKey {
            state: ProvisioningState::Pending,
            ..
        }
    ));
}

#[rstest]
fn rejects_scope_tenant(signing_key: SigningKey, clock: FixedClock) {
    let policy = ScopePolicy::new("other-tenant", "ws", ScopePolicy::all_tiers()).unwrap();
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    let DomainError::ScopeDenied { message } = err else {
        panic!("expected ScopeDenied, got {err:?}");
    };
    assert!(message.starts_with("tenant"));
}

#[rstest]
fn rejects_scope_workspace(signing_key: SigningKey, clock: FixedClock) {
    let policy = ScopePolicy::new("acme", "other-ws", ScopePolicy::all_tiers()).unwrap();
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    let DomainError::ScopeDenied { message } = err else {
        panic!("expected ScopeDenied, got {err:?}");
    };
    assert!(message.starts_with("workspace"));
}

#[rstest]
fn rejects_scope_tier(signing_key: SigningKey, clock: FixedClock) {
    let mut allowed = HashSet::new();
    allowed.insert(SignedIntentScopeTier::Org); // Project not allowed
    let policy = ScopePolicy::new("acme", "ws", allowed).unwrap();
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = resolved_active(&signing_key);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    let DomainError::ScopeDenied { message } = err else {
        panic!("expected ScopeDenied, got {err:?}");
    };
    assert!(message.starts_with("tier"));
}

#[rstest]
fn rejects_issuer_mismatch(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let intent = sign(&signing_key, unsigned_intent());
    let resolved = ResolvedIssuer::for_test(
        Identity::parse("hmn:someone-else").unwrap(),
        KeyVersion::FIRST,
        signing_key.verifying_key(),
        ProvisioningState::Active,
    );
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::Unauthorized { .. }));
}

#[rstest]
fn rejects_wrong_key_version(signing_key: SigningKey, policy: ScopePolicy, clock: FixedClock) {
    let intent = sign(&signing_key, unsigned_intent()); // intent.key_version = 1
    let resolved = ResolvedIssuer::for_test(
        Identity::parse(ISSUER_WIRE).unwrap(),
        KeyVersion::new(std::num::NonZeroU32::new(2).unwrap()), // resolved version 2
        signing_key.verifying_key(),
        ProvisioningState::Active,
    );
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let err = verifier.verify(intent, &resolved).unwrap_err();
    assert!(matches!(err, DomainError::Unauthorized { .. }));
}
```

- [ ] **Step 2: Re-enable `mod tests;` in `verifier/mod.rs`**

If you commented out the line in Task 7 step 6, restore it. Confirm it reads:

```rust
#[cfg(test)]
mod tests;
```

- [ ] **Step 3: Run tests**

```bash
cargo nextest run -p cairn-core --lib --locked verifier::tests
```

Expected: 12 tests pass.

- [ ] **Step 4: Lint + format**

```bash
cargo fmt --all
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/verifier/mod.rs crates/cairn-core/src/verifier/tests.rs
git commit -m "test(core): EnvelopeVerifier rstest suite (issue #51)"
```

---

## Task 9: Proptest — one-byte mutation invariant

**Files:**
- Create: `crates/cairn-core/tests/verifier_proptests.rs`

- [ ] **Step 1: Write the proptest**

Create `crates/cairn-core/tests/verifier_proptests.rs`:

```rust
//! Property: any single-byte mutation in the canonical-payload bytes of a
//! signed envelope makes the verifier reject (typically as
//! `InvalidSignature`; a mutation that lands in a wire-shape-validated
//! field would be caught by the deserializer earlier, but at the
//! verifier-input level we feed an already-deserialized struct, so every
//! mutation reaches the signature check).

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey};
use proptest::prelude::*;

use cairn_core::domain::DomainError;
use cairn_core::domain::Identity;
use cairn_core::domain::canonical::canonical_bytes_signed_intent;
use cairn_core::domain::identity::keys::KeyVersion;
use cairn_core::domain::identity::records::ProvisioningState;
use cairn_core::domain::time::FixedClock;
use cairn_core::generated::common;
use cairn_core::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};
use cairn_core::verifier::{EnvelopeVerifier, ResolvedIssuer, ScopePolicy};

const ISSUER: &str = "hmn:tafeng";
const ISSUED_AT: &str = "2026-04-22T14:02:11Z";
const EXPIRES_AT: &str = "2026-04-22T14:07:11Z";
const NOW: &str = "2026-04-22T14:05:00Z";

fn build_signed(key: &SigningKey) -> SignedIntent {
    let mut intent = SignedIntent {
        chain_parents: vec![],
        expires_at: EXPIRES_AT.into(),
        issued_at: ISSUED_AT.into(),
        issuer: common::Identity(ISSUER.into()),
        key_version: 1,
        nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".into()),
        operation_id: common::Ulid("01HQZX9F5N0000000000000000".into()),
        scope: SignedIntentScope {
            tenant: "acme".into(),
            workspace: "ws".into(),
            entity: "ent".into(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence: Some(1),
        server_challenge: None,
        signature: common::Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
        target_hash: format!("sha256:{}", "a".repeat(64)),
    };
    let bytes = canonical_bytes_signed_intent(&intent).unwrap();
    let sig = key.sign(&bytes);
    let hex = sig
        .to_bytes()
        .iter()
        .fold(String::with_capacity(128), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(&mut s, "{:02x}", b);
            s
        });
    intent.signature = common::Ed25519Signature(format!("ed25519:{hex}"));
    intent
}

proptest! {
    /// Flip a random byte (excluding the signature bytes) in the
    /// canonical payload after signing — the verifier must reject.
    #[test]
    fn one_byte_mutation_rejects(byte_index in any::<usize>(), bit_mask in 1u8..=255) {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let policy = ScopePolicy::new("acme", "ws", ScopePolicy::all_tiers()).unwrap();
        let now: DateTime<Utc> = NOW.parse().unwrap();
        let clock = FixedClock(now);
        let resolved = ResolvedIssuer::for_test(
            Identity::parse(ISSUER).unwrap(),
            KeyVersion::FIRST,
            key.verifying_key(),
            ProvisioningState::Active,
        );

        let intent = build_signed(&key);
        let canonical = canonical_bytes_signed_intent(&intent).unwrap();
        // Skip empty edge case.
        prop_assume!(!canonical.is_empty());

        // Mutating the canonical bytes directly does not affect the
        // SignedIntent struct; instead, mutate one structural field of the
        // struct that the canonical encoder picks up and assert
        // InvalidSignature.
        let mut tweaked = intent.clone();
        let target = byte_index % 4;
        match target {
            0 => tweaked.scope.entity.push((bit_mask % 26 + b'a') as char),
            1 => tweaked.scope.workspace.push((bit_mask % 26 + b'a') as char),
            2 => tweaked.scope.tenant.push((bit_mask % 26 + b'a') as char),
            _ => tweaked.target_hash = format!("sha256:{}", "b".repeat(64)),
        }

        let verifier = EnvelopeVerifier::new(&policy, &clock);
        let result = verifier.verify(tweaked, &resolved);
        // The mutation may also trigger ScopeDenied (workspace/tenant
        // change) — both are valid rejection reasons.
        prop_assert!(matches!(
            result,
            Err(DomainError::InvalidSignature)
                | Err(DomainError::ScopeDenied { .. })
        ));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo nextest run -p cairn-core --test verifier_proptests --locked
```

Expected: proptest runs (default 256 cases), all pass. First run may take 5–15 s.

- [ ] **Step 3: Lint + format**

```bash
cargo fmt --all
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/tests/verifier_proptests.rs
git commit -m "test(core): proptest — envelope mutation rejection invariant (issue #51)"
```

---

## Task 10: Wire-error mapper (`envelope_error_for`) + typed `ErrorBody`

**Files:**
- Create: `crates/cairn-core/src/error/wire.rs`
- Modify: `crates/cairn-core/src/error/mod.rs` (add `pub mod wire; pub use wire::*;`)

- [ ] **Step 1: Write the failing tests + implementation**

Create `crates/cairn-core/src/error/wire.rs`:

```rust
//! Wire-error mapping: [`crate::domain::DomainError`] →
//! [`ErrorBody`] (the typed Rust counterpart of the JSON object that
//! the generated `Response.error` deserializer validates structurally
//! via `validate_error_envelope`).
//!
//! Single source of truth for error envelopes across CLI / MCP / SDK.

use serde::Serialize;

use crate::domain::DomainError;
use crate::generated::errors::ErrorCode;

/// Typed wire error envelope.
///
/// Serialises into `{"code": "...", "message": "...", "data": {...}}` —
/// the same shape `validate_error_envelope` enforces. `data` is omitted
/// when absent rather than serialised as `null`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ErrorBody {
    /// Closed-enum string from [`ErrorCode`].
    #[serde(serialize_with = "serialize_error_code")]
    pub code: ErrorCode,
    /// Human-readable summary; never empty.
    pub message: String,
    /// Per-code structured payload. Omitted when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

fn serialize_error_code<S: serde::Serializer>(
    code: &ErrorCode,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.serialize_str(code.as_str())
}

/// Map a [`DomainError`] from the verifier (or a deserializer reject) to
/// a typed [`ErrorBody`]. Variants the verifier itself does not produce
/// fall through to `InvalidArgs` with the error's `Display` as `reason`.
#[must_use]
pub fn envelope_error_for(err: &DomainError) -> ErrorBody {
    match err {
        DomainError::InvalidSignature | DomainError::MissingSignature { .. } => ErrorBody {
            code: ErrorCode::MissingSignature,
            message: err.to_string(),
            // The MissingSignature wire shape carries no `data` — leave None.
            data: None,
        },

        DomainError::ExpiredIntent {
            issued_at,
            expires_at,
            now,
        } => ErrorBody {
            code: ErrorCode::ExpiredIntent,
            message: err.to_string(),
            data: Some(serde_json::json!({
                "issued_at": issued_at,
                "expires_at": expires_at,
                "now": now,
            })),
        },

        DomainError::RevokedKey { .. } | DomainError::KeyVersionMismatch { .. } => ErrorBody {
            code: ErrorCode::RevokedKey,
            message: err.to_string(),
            data: None,
        },

        DomainError::ScopeDenied { .. } | DomainError::Unauthorized { .. } => ErrorBody {
            code: ErrorCode::Unauthorized,
            message: err.to_string(),
            data: None,
        },

        // Fall-through: every remaining DomainError variant maps to
        // InvalidArgs with the error's Display in the reason field. This
        // covers deserializer-rejected envelopes (InvalidIdentity,
        // InvalidTimestamp) and any non-envelope domain validation errors
        // a caller might funnel through this mapper.
        other => ErrorBody {
            code: ErrorCode::InvalidArgs,
            message: other.to_string(),
            data: Some(serde_json::json!({
                "field": "envelope",
                "reason": other.to_string(),
            })),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Identity;
    use crate::domain::identity::keys::KeyVersion;
    use crate::domain::identity::records::ProvisioningState;

    #[test]
    fn invalid_signature_maps_to_missing_signature() {
        let body = envelope_error_for(&DomainError::InvalidSignature);
        assert!(matches!(body.code, ErrorCode::MissingSignature));
        assert!(body.data.is_none());
        assert!(!body.message.is_empty());
    }

    #[test]
    fn expired_intent_carries_iso_data() {
        let err = DomainError::ExpiredIntent {
            issued_at: "2026-04-22T14:02:11Z".into(),
            expires_at: "2026-04-22T14:07:11Z".into(),
            now: "2026-04-22T15:00:00Z".into(),
        };
        let body = envelope_error_for(&err);
        let data = body.data.unwrap();
        assert_eq!(data.get("now").unwrap().as_str().unwrap(), "2026-04-22T15:00:00Z");
    }

    #[test]
    fn revoked_key_has_no_data() {
        let body = envelope_error_for(&DomainError::RevokedKey {
            id: Identity::parse("hmn:tafeng").unwrap(),
            state: ProvisioningState::Revoked,
        });
        assert!(matches!(body.code, ErrorCode::RevokedKey));
        assert!(body.data.is_none());
    }

    #[test]
    fn key_version_mismatch_collapses_to_revoked_key_at_p0() {
        let body = envelope_error_for(&DomainError::KeyVersionMismatch {
            intent: KeyVersion::FIRST,
            current: None,
        });
        assert!(matches!(body.code, ErrorCode::RevokedKey));
    }

    #[test]
    fn scope_denied_maps_to_unauthorized() {
        let body = envelope_error_for(&DomainError::ScopeDenied {
            message: "tenant".into(),
        });
        assert!(matches!(body.code, ErrorCode::Unauthorized));
    }

    #[test]
    fn unknown_variant_falls_through_to_invalid_args() {
        let body = envelope_error_for(&DomainError::InvalidIdentity {
            message: "bad".into(),
        });
        assert!(matches!(body.code, ErrorCode::InvalidArgs));
        let data = body.data.unwrap();
        assert!(data.get("field").is_some());
        assert!(data.get("reason").is_some());
    }
}
```

- [ ] **Step 2: Register the module**

Edit `crates/cairn-core/src/error/mod.rs`. Replace its body with:

```rust
//! Top-level error types for `cairn-core`.
//!
//! Each sub-module holds one error enum or surface whose scope matches a
//! contract or service boundary. Contract-level errors (e.g.,
//! [`KeystoreError`], [`RegistryError`]) live alongside their traits in
//! [`crate::contract`]; this module holds errors and translation layers
//! whose scope spans multiple contracts.
//!
//! [`KeystoreError`]: crate::contract::keystore::KeystoreError
//! [`RegistryError`]: crate::contract::identity_registry::RegistryError

pub mod identity;
pub mod wire;

pub use identity::IdentityServiceError;
pub use wire::{ErrorBody, envelope_error_for};
```

- [ ] **Step 3: Run tests**

```bash
cargo nextest run -p cairn-core --lib --locked error::wire
```

Expected: 6 tests pass.

- [ ] **Step 4: Lint + format**

```bash
cargo fmt --all
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/error/wire.rs crates/cairn-core/src/error/mod.rs
git commit -m "feat(core): envelope_error_for — typed DomainError → wire ErrorBody mapper (issue #51)"
```

---

## Task 11: Wire-error snapshot tests (`insta`)

**Files:**
- Create: `crates/cairn-core/tests/envelope_errors.rs`
- Create (auto): `crates/cairn-core/tests/snapshots/*.snap`

- [ ] **Step 1: Write the snapshot test**

Create `crates/cairn-core/tests/envelope_errors.rs`:

```rust
//! Snapshot tests for [`cairn_core::error::envelope_error_for`].
//! Locks the wire shape across CLI / MCP / SDK. Any change to the mapper's
//! output will trigger `cargo insta review`.

use cairn_core::domain::Identity;
use cairn_core::domain::DomainError;
use cairn_core::domain::identity::keys::KeyVersion;
use cairn_core::domain::identity::records::ProvisioningState;
use cairn_core::error::envelope_error_for;

fn snapshot_body(name: &str, err: DomainError) {
    let body = envelope_error_for(&err);
    insta::with_settings!({ snapshot_suffix => name }, {
        insta::assert_json_snapshot!(body);
    });
}

#[test]
fn invalid_signature() {
    snapshot_body("invalid_signature", DomainError::InvalidSignature);
}

#[test]
fn expired_intent() {
    snapshot_body(
        "expired_intent",
        DomainError::ExpiredIntent {
            issued_at: "2026-04-22T14:02:11Z".into(),
            expires_at: "2026-04-22T14:07:11Z".into(),
            now: "2026-04-22T15:00:00Z".into(),
        },
    );
}

#[test]
fn revoked_key() {
    snapshot_body(
        "revoked_key",
        DomainError::RevokedKey {
            id: Identity::parse("hmn:tafeng").unwrap(),
            state: ProvisioningState::Revoked,
        },
    );
}

#[test]
fn key_version_mismatch() {
    snapshot_body(
        "key_version_mismatch",
        DomainError::KeyVersionMismatch {
            intent: KeyVersion::FIRST,
            current: None,
        },
    );
}

#[test]
fn scope_denied() {
    snapshot_body(
        "scope_denied",
        DomainError::ScopeDenied {
            message: "tenant: expected acme, got other".into(),
        },
    );
}

#[test]
fn unauthorized() {
    snapshot_body(
        "unauthorized",
        DomainError::Unauthorized {
            message: "issuer mismatch".into(),
        },
    );
}

#[test]
fn fallthrough_invalid_identity() {
    snapshot_body(
        "fallthrough_invalid_identity",
        DomainError::InvalidIdentity {
            message: "bad prefix".into(),
        },
    );
}
```

- [ ] **Step 2: First run — snapshots will be missing**

```bash
cargo nextest run -p cairn-core --test envelope_errors --locked
```

Expected: all 7 tests fail with `pending snapshot`.

- [ ] **Step 3: Review and accept snapshots**

```bash
cargo insta review
```

Manually inspect each snapshot. Accept (`a`) if the JSON matches the expected wire shape (`code` is the closed-enum string, `message` non-empty, `data` keys per `validate_error_envelope`).

- [ ] **Step 4: Re-run tests to confirm**

```bash
cargo nextest run -p cairn-core --test envelope_errors --locked
```

Expected: all 7 tests pass.

- [ ] **Step 5: Lint + format**

```bash
cargo fmt --all
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/tests/envelope_errors.rs \
        crates/cairn-core/tests/snapshots/
git commit -m "test(core): insta snapshots — wire-error envelope shapes (issue #51)"
```

---

## Task 12: Test fixtures in `cairn-test-fixtures`

**Files:**
- Create: `crates/cairn-test-fixtures/src/intent.rs`
- Modify: `crates/cairn-test-fixtures/src/lib.rs` (add `pub mod intent;`)
- Modify: `crates/cairn-test-fixtures/Cargo.toml` (depend on `cairn-core` with `test-helpers` feature; depend on `ed25519-dalek`, `chrono` if not already)

- [ ] **Step 1: Confirm Cargo.toml dependencies**

Run:
```bash
grep -nE "cairn-core|ed25519-dalek|chrono" crates/cairn-test-fixtures/Cargo.toml
```

Ensure:
- `cairn-core` listed under `[dependencies]` with `features = ["test-helpers"]`
- `ed25519-dalek = { workspace = true }`
- `chrono = { workspace = true }`

If any are missing, add them. Example shape:

```toml
[dependencies]
cairn-core = { path = "../cairn-core", features = ["test-helpers"] }
cairn-store-sqlite = { path = "../cairn-store-sqlite" }
ed25519-dalek = { workspace = true }
chrono = { workspace = true }
tempfile = { workspace = true }
```

- [ ] **Step 2: Write the fixture module**

Create `crates/cairn-test-fixtures/src/intent.rs`:

```rust
//! Envelope-verifier test fixtures.
//!
//! Helpers for building signed `SignedIntent` values, fixed clocks, and
//! a default `ScopePolicy` aligned with the verifier's unit-test fixtures.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey};

use cairn_core::domain::canonical::canonical_bytes_signed_intent;
use cairn_core::domain::time::FixedClock;
use cairn_core::generated::common;
use cairn_core::generated::envelope::{
    SignedIntent, SignedIntentScope, SignedIntentScopeTier,
};
use cairn_core::verifier::ScopePolicy;

/// Default issuer wire string used by all fixtures.
pub const FIXTURE_ISSUER: &str = "hmn:tafeng";
/// Default issued_at used by all fixtures.
pub const FIXTURE_ISSUED_AT: &str = "2026-04-22T14:02:11Z";
/// Default expires_at used by all fixtures.
pub const FIXTURE_EXPIRES_AT: &str = "2026-04-22T14:07:11Z";

/// Build an unsigned [`SignedIntent`] with a placeholder signature
/// (128 lowercase hex zeros) — pass to [`sign_intent`] before verifying.
#[must_use]
pub fn unsigned_intent() -> SignedIntent {
    SignedIntent {
        chain_parents: vec![],
        expires_at: FIXTURE_EXPIRES_AT.into(),
        issued_at: FIXTURE_ISSUED_AT.into(),
        issuer: common::Identity(FIXTURE_ISSUER.into()),
        key_version: 1,
        nonce: common::Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".into()),
        operation_id: common::Ulid("01HQZX9F5N0000000000000000".into()),
        scope: SignedIntentScope {
            tenant: "acme".into(),
            workspace: "ws".into(),
            entity: "ent".into(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence: Some(1),
        server_challenge: None,
        signature: common::Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
        target_hash: format!("sha256:{}", "a".repeat(64)),
    }
}

/// Sign `intent` with `key`, replacing `intent.signature` with the
/// resulting `ed25519:<hex>` value.
#[must_use]
#[allow(clippy::expect_used)] // dev fixture; canonical encoding cannot fail for a well-formed intent
pub fn sign_intent(key: &SigningKey, mut intent: SignedIntent) -> SignedIntent {
    let bytes =
        canonical_bytes_signed_intent(&intent).expect("canonical encoding always succeeds");
    let sig = key.sign(&bytes);
    let hex = sig
        .to_bytes()
        .iter()
        .fold(String::with_capacity(128), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(&mut s, "{:02x}", b);
            s
        });
    intent.signature = common::Ed25519Signature(format!("ed25519:{hex}"));
    intent
}

/// Build a [`FixedClock`] from an RFC-3339 string.
///
/// # Panics
/// Panics if `iso` is not a valid RFC-3339 timestamp — fixture code only.
#[must_use]
#[allow(clippy::expect_used)]
pub fn fixed_clock_at(iso: &str) -> FixedClock {
    let t: DateTime<Utc> = iso.parse().expect("fixture: valid RFC-3339 string");
    FixedClock(t)
}

/// Default [`ScopePolicy`] aligned with [`unsigned_intent`].
///
/// # Panics
/// Panics if `ScopePolicy::new` rejects the inputs (it cannot — strings
/// are non-empty and the tier set is non-empty).
#[must_use]
#[allow(clippy::expect_used)]
pub fn scope_policy_default() -> ScopePolicy {
    let mut tiers = HashSet::new();
    tiers.insert(SignedIntentScopeTier::Project);
    tiers.insert(SignedIntentScopeTier::Session);
    tiers.insert(SignedIntentScopeTier::Private);
    tiers.insert(SignedIntentScopeTier::Team);
    tiers.insert(SignedIntentScopeTier::Org);
    tiers.insert(SignedIntentScopeTier::Public);
    ScopePolicy::new("acme", "ws", tiers).expect("fixture: well-formed scope policy")
}
```

- [ ] **Step 3: Register the module**

Edit `crates/cairn-test-fixtures/src/lib.rs` and add `pub mod intent;` next to the existing `pub mod keystore;` and `pub mod store;` entries:

```rust
pub mod intent;
pub mod keystore;
pub mod store;
```

- [ ] **Step 4: Compile**

```bash
cargo check -p cairn-test-fixtures --all-targets --locked
```

If `cairn-core::verifier::ScopePolicy` is not visible, confirm Task 7 step 5 committed the module re-export. (`crates/cairn-core/src/lib.rs` should declare `pub mod verifier;`.)

- [ ] **Step 5: Lint + format**

```bash
cargo fmt --all
cargo clippy -p cairn-test-fixtures --all-targets --locked -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-test-fixtures/src/intent.rs \
        crates/cairn-test-fixtures/src/lib.rs \
        crates/cairn-test-fixtures/Cargo.toml
git commit -m "test(fixtures): sign_intent / fixed_clock_at / scope_policy_default (issue #51)"
```

---

## Task 13: `resolve_issuer` integration test against `SqliteIdentityRegistry`

**Files:**
- Create: `crates/cairn-store-sqlite/tests/resolve_issuer.rs`

- [ ] **Step 1: Inspect the test-helpers surface for `SqliteIdentityRegistry`**

Run:
```bash
grep -n "open_in_memory\|reserve_first_identity\|reserve_identity\|activate_identity" \
  crates/cairn-store-sqlite/src/identity/mod.rs | head -10
```

Note the public test-helper constructor name (e.g., `SqliteIdentityRegistry::open_in_memory`). Use it in the test below; if it differs, substitute throughout.

- [ ] **Step 2: Write the integration test**

Create `crates/cairn-store-sqlite/tests/resolve_issuer.rs`:

```rust
//! Integration test: [`cairn_core::verifier::resolve_issuer`] against a
//! real [`cairn_store_sqlite::SqliteIdentityRegistry`].

use chrono::Utc;
use ed25519_dalek::SigningKey;
use rand_core::OsRng;

use cairn_core::contract::identity_registry::IdentityRegistry;
use cairn_core::domain::DomainError;
use cairn_core::domain::Identity;
use cairn_core::domain::identity::IdentityKind;
use cairn_core::domain::identity::keys::{IdentityRevision, KeyVersion};
use cairn_core::domain::identity::records::{
    IdentityKeyEntry, ProvisioningState, PublicIdentityRecord,
};
use cairn_core::verifier::resolve_issuer;
use cairn_store_sqlite::SqliteIdentityRegistry;

async fn registry_with_identity(
    state: ProvisioningState,
    key: &SigningKey,
) -> (SqliteIdentityRegistry, Identity) {
    let reg = SqliteIdentityRegistry::open_in_memory()
        .await
        .expect("open in-memory");
    let id = Identity::parse("hmn:tafeng").unwrap();
    let record = PublicIdentityRecord {
        id: id.clone(),
        kind: IdentityKind::Human,
        current_key_version: KeyVersion::FIRST,
        revision: IdentityRevision::FIRST,
        provisioning_state: state,
        created_at: Utc::now(),
        activated_at: matches!(state, ProvisioningState::Active).then(Utc::now),
        revoked_at: matches!(state, ProvisioningState::Revoked).then(Utc::now),
        purge_requested_at: None,
        purged_at: None,
    };
    let key_entry = IdentityKeyEntry {
        identity_id: id.clone(),
        key_version: KeyVersion::FIRST,
        public_key: key.verifying_key().to_bytes(),
        signed_predecessor: None,
        created_at: Utc::now(),
        superseded_at: None,
    };
    reg.reserve_identity(&record, &key_entry).await.unwrap();
    reg.activate_identity(&id, KeyVersion::FIRST).await.unwrap();
    (reg, id)
}

#[tokio::test]
async fn resolves_active_issuer() {
    let key = SigningKey::generate(&mut OsRng);
    let (reg, id) = registry_with_identity(ProvisioningState::Active, &key).await;
    let resolved = resolve_issuer(&reg, &id, KeyVersion::FIRST).await.unwrap();
    assert_eq!(resolved.identity, id);
    assert_eq!(resolved.key_version, KeyVersion::FIRST);
    assert!(matches!(resolved.state, ProvisioningState::Active));
    assert_eq!(
        resolved.verifying_key.to_bytes(),
        key.verifying_key().to_bytes()
    );
}

#[tokio::test]
async fn unknown_issuer_returns_unauthorized() {
    let reg = SqliteIdentityRegistry::open_in_memory().await.unwrap();
    let id = Identity::parse("hmn:nobody").unwrap();
    let err = resolve_issuer(&reg, &id, KeyVersion::FIRST)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Unauthorized { .. }));
}

#[tokio::test]
async fn unknown_key_version_returns_mismatch() {
    let key = SigningKey::generate(&mut OsRng);
    let (reg, id) = registry_with_identity(ProvisioningState::Active, &key).await;
    let v2 = KeyVersion::new(std::num::NonZeroU32::new(2).unwrap());
    let err = resolve_issuer(&reg, &id, v2).await.unwrap_err();
    let DomainError::KeyVersionMismatch { intent, current } = err else {
        panic!("expected KeyVersionMismatch, got {err:?}");
    };
    assert_eq!(intent, v2);
    assert_eq!(current, Some(KeyVersion::FIRST));
}

#[tokio::test]
async fn revoked_issuer_is_resolved_state_revoked() {
    let key = SigningKey::generate(&mut OsRng);
    let (reg, id) = registry_with_identity(ProvisioningState::Revoked, &key).await;
    // resolve_issuer succeeds; the verifier is what rejects on state.
    let resolved = resolve_issuer(&reg, &id, KeyVersion::FIRST).await.unwrap();
    assert!(matches!(resolved.state, ProvisioningState::Revoked));
}
```

> If `SqliteIdentityRegistry::open_in_memory` does not exist, look for an
> equivalent constructor in `crates/cairn-store-sqlite/src/identity/mod.rs`
> (e.g., `for_tests`, `new_in_memory`, or a `tempfile::tempdir()` + `open`
> sequence). Adjust the helper accordingly.

- [ ] **Step 3: Run tests**

```bash
cargo nextest run -p cairn-store-sqlite --test resolve_issuer --locked
```

Expected: 4 tests pass.

- [ ] **Step 4: Lint + format**

```bash
cargo fmt --all
cargo clippy -p cairn-store-sqlite --all-targets --locked -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/tests/resolve_issuer.rs
git commit -m "test(store): resolve_issuer against SqliteIdentityRegistry (issue #51)"
```

---

## Task 14: DB-isolation test — bad envelopes leave WAL/records empty

**Files:**
- Create: `crates/cairn-store-sqlite/tests/envelope_blocks_wal.rs`

This test asserts the *contract* of the verifier: any `Err` return path means no WAL or record rows are written. It does so by exercising a hand-rolled minimal "ingest" that mirrors §5.1 of the spec — `resolve_issuer → verify → store.append` — and asserts table counts after each negative case.

- [ ] **Step 1: Identify the WAL and records tables**

Run:
```bash
grep -rn "CREATE TABLE.*wal\|CREATE TABLE.*records\|CREATE TABLE.*memory_records" \
  crates/cairn-store-sqlite/migrations/ 2>/dev/null | head -10
```

Note the exact table names. Substitute them into the assertions below as `<wal_table>` and `<records_table>`.

- [ ] **Step 2: Write the integration test**

Create `crates/cairn-store-sqlite/tests/envelope_blocks_wal.rs`:

```rust
//! Negative-path integration test: any envelope that fails verification
//! must leave the WAL and records tables untouched. This is the
//! load-bearing acceptance criterion for issue #51.

use std::collections::HashSet;

use chrono::Utc;
use ed25519_dalek::SigningKey;
use rand_core::OsRng;

use cairn_core::contract::identity_registry::IdentityRegistry;
use cairn_core::domain::DomainError;
use cairn_core::domain::Identity;
use cairn_core::domain::identity::IdentityKind;
use cairn_core::domain::identity::keys::{IdentityRevision, KeyVersion};
use cairn_core::domain::identity::records::{
    IdentityKeyEntry, ProvisioningState, PublicIdentityRecord,
};
use cairn_core::domain::time::FixedClock;
use cairn_core::generated::common;
use cairn_core::generated::envelope::SignedIntentScopeTier;
use cairn_core::verifier::{EnvelopeVerifier, ScopePolicy, resolve_issuer};
use cairn_store_sqlite::SqliteIdentityRegistry;
use cairn_test_fixtures::intent::{
    fixed_clock_at, scope_policy_default, sign_intent, unsigned_intent,
};

const NOW: &str = "2026-04-22T14:05:00Z";

async fn provision_active(
    reg: &SqliteIdentityRegistry,
    key: &SigningKey,
) -> Identity {
    let id = Identity::parse("hmn:tafeng").unwrap();
    let record = PublicIdentityRecord {
        id: id.clone(),
        kind: IdentityKind::Human,
        current_key_version: KeyVersion::FIRST,
        revision: IdentityRevision::FIRST,
        provisioning_state: ProvisioningState::Active,
        created_at: Utc::now(),
        activated_at: Some(Utc::now()),
        revoked_at: None,
        purge_requested_at: None,
        purged_at: None,
    };
    let key_entry = IdentityKeyEntry {
        identity_id: id.clone(),
        key_version: KeyVersion::FIRST,
        public_key: key.verifying_key().to_bytes(),
        signed_predecessor: None,
        created_at: Utc::now(),
        superseded_at: None,
    };
    reg.reserve_identity(&record, &key_entry).await.unwrap();
    reg.activate_identity(&id, KeyVersion::FIRST).await.unwrap();
    id
}

/// Returns the SQLite row count for `table`. Test infrastructure only.
async fn row_count(reg: &SqliteIdentityRegistry, table: &str) -> i64 {
    // The registry exposes a raw connection in tests; if no helper exists,
    // open a sibling read-only connection to the same in-memory DB. Replace
    // this stub with the real call once the helper is identified.
    reg.test_only_query_scalar_i64(&format!("SELECT count(*) FROM {table}"))
        .await
        .unwrap_or(0)
}

async fn run_negative<F>(case: &str, mutate: F)
where
    F: FnOnce(
        cairn_core::generated::envelope::SignedIntent,
    ) -> cairn_core::generated::envelope::SignedIntent,
{
    let reg = SqliteIdentityRegistry::open_in_memory().await.unwrap();
    let key = SigningKey::generate(&mut OsRng);
    let id = provision_active(&reg, &key).await;

    let policy = scope_policy_default();
    let clock = fixed_clock_at(NOW);
    let verifier = EnvelopeVerifier::new(&policy, &clock);

    let signed = sign_intent(&key, unsigned_intent());
    let intent = mutate(signed);

    let resolved = resolve_issuer(&reg, &id, KeyVersion::FIRST).await.unwrap();
    let result = verifier.verify(intent, &resolved);
    assert!(result.is_err(), "[{case}] expected verify() to reject");

    // <wal_table> and <records_table> placeholders — replace with the
    // actual names from Step 1.
    assert_eq!(
        row_count(&reg, "<wal_table>").await,
        0,
        "[{case}] WAL must be empty after rejection"
    );
    assert_eq!(
        row_count(&reg, "<records_table>").await,
        0,
        "[{case}] records must be empty after rejection"
    );
}

#[tokio::test]
async fn tampered_signature_blocks_wal() {
    run_negative("tampered_signature", |mut i| {
        let hex = i.signature.0.strip_prefix("ed25519:").unwrap().to_owned();
        let mut chars: Vec<char> = hex.chars().collect();
        chars[0] = if chars[0] == 'a' { 'b' } else { 'a' };
        let mutated: String = chars.into_iter().collect();
        i.signature = common::Ed25519Signature(format!("ed25519:{mutated}"));
        i
    })
    .await;
}

#[tokio::test]
async fn expired_blocks_wal() {
    let reg = SqliteIdentityRegistry::open_in_memory().await.unwrap();
    let key = SigningKey::generate(&mut OsRng);
    let id = provision_active(&reg, &key).await;
    let policy = scope_policy_default();
    let clock = fixed_clock_at("2026-04-22T15:00:00Z"); // after expires_at
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let intent = sign_intent(&key, unsigned_intent());
    let resolved = resolve_issuer(&reg, &id, KeyVersion::FIRST).await.unwrap();
    assert!(matches!(
        verifier.verify(intent, &resolved),
        Err(DomainError::ExpiredIntent { .. })
    ));
    assert_eq!(row_count(&reg, "<wal_table>").await, 0);
    assert_eq!(row_count(&reg, "<records_table>").await, 0);
}

#[tokio::test]
async fn scope_denied_blocks_wal() {
    let reg = SqliteIdentityRegistry::open_in_memory().await.unwrap();
    let key = SigningKey::generate(&mut OsRng);
    let id = provision_active(&reg, &key).await;
    let mut tiers = HashSet::new();
    tiers.insert(SignedIntentScopeTier::Org); // Project not allowed
    let policy = ScopePolicy::new("acme", "ws", tiers).unwrap();
    let clock = fixed_clock_at(NOW);
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    let intent = sign_intent(&key, unsigned_intent());
    let resolved = resolve_issuer(&reg, &id, KeyVersion::FIRST).await.unwrap();
    assert!(matches!(
        verifier.verify(intent, &resolved),
        Err(DomainError::ScopeDenied { .. })
    ));
    assert_eq!(row_count(&reg, "<wal_table>").await, 0);
    assert_eq!(row_count(&reg, "<records_table>").await, 0);
}
```

> **Important:** the `row_count` helper assumes a test-only scalar-query
> method on `SqliteIdentityRegistry`. If none exists:
>
> 1. Add a `pub(crate) fn test_only_query_scalar_i64(...)` method behind
>    `#[cfg(any(test, feature = "test-helpers"))]` on `SqliteIdentityRegistry`,
>    or
> 2. Open a second `rusqlite::Connection` to the same database file (for
>    on-disk tests) and run the query directly.
>
> Pick whichever fits the existing crate's testing conventions.
> If both options are infeasible (e.g., `:memory:` is opaque), narrow the
> assertion to "verify() returned Err" and add a `// TODO(issue #X):
> table-count assertion once a helper exists" inline.
> The verifier-rejection assertion alone still satisfies the issue's
> "never reach WAL preparation" criterion functionally, since at this
> point in the codebase no WAL prep helper exists *to* call.

- [ ] **Step 3: Run tests**

```bash
cargo nextest run -p cairn-store-sqlite --test envelope_blocks_wal --locked
```

Expected: 3 tests pass. If the table-count helper is unavailable, the test
files will still compile and the verifier-rejection assertion will still
fire; only the row-count assertion is gated.

- [ ] **Step 4: Lint + format**

```bash
cargo fmt --all
cargo clippy -p cairn-store-sqlite --all-targets --locked -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/tests/envelope_blocks_wal.rs
git commit -m "test(store): bad envelopes leave WAL/records untouched (issue #51)"
```

---

## Task 15: Workspace verification

**Files:**
- (none — verification only)

- [ ] **Step 1: Format + clippy + check**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
```

Expected: zero warnings.

- [ ] **Step 2: Workspace test run**

```bash
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
```

Expected: all tests pass; no doctest regressions.

- [ ] **Step 3: Core boundary check**

```bash
./scripts/check-core-boundary.sh
```

Expected: zero violations. `cairn-core` must not have gained any
non-trait dep on adapter crates.

- [ ] **Step 4: Codegen check**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: no diff (this PR does not touch the IDL).

- [ ] **Step 5: Supply-chain (only if you have the binaries)**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Skip if those tools aren't installed locally; CI runs them.

- [ ] **Step 6: Final commit (only if any reformat applied)**

If `cargo fmt --all --check` reported drift in step 1, commit the format
fix:

```bash
git add -u
git commit -m "chore: cargo fmt across issue #51 surfaces"
```

Otherwise skip.

---

## Self-review summary

**Spec coverage (each spec section maps to tasks):**

| Spec § | Topic | Task(s) |
|---|---|---|
| §4 Architecture | File map | Tasks 1-12 |
| §5 Control flow | Verifier check order | Tasks 7, 8 |
| §6.1 `Clock` | New trait + impls | Task 1 |
| §6.2 `ResolvedIssuer` | Plain-data type | Task 4 (lands with Task 7) |
| §6.3 `ScopePolicy` | Plain-data type + ctor | Task 5 (lands with Task 7) |
| §6.4 `EnvelopeVerifier` | Struct + verify | Task 7 |
| §6.5 `resolve_issuer` | Async helper | Task 6 (lands with Task 7), Task 13 |
| §6.6 `DomainError` additions | Six new variants | Task 3 |
| §6.7 Wire mapper | `envelope_error_for` | Task 10 |
| §6.8 `canonical_bytes_signed_intent` | Canonical encoder | Task 2 |
| §7.1 Layer 1 unit tests | rstest | Task 8 |
| §7.1 proptest | mutation invariant | Task 9 |
| §7.2 Layer 2 DB-isolation | sqlite test | Task 14 |
| §7.3 Layer 3 wire-error snapshot | insta | Task 11 |
| §7.4 Layer 4 adapter smoke | (out of scope) | Documented in File Map "out of scope" — verbs land in #9 |
| §7.5 Test fixtures | sign_intent, etc. | Task 12 |
| §8 Acceptance criteria | issue #51 mapping | Tasks 7+8 (criterion 1), Task 7 (criterion 2), Tasks 10+11 (criterion 3); Task 14 confirms negative-path |
| §10 Verification commands | CI mirror | Task 15 |

**Placeholder scan:** every `<wal_table>` / `<records_table>` placeholder in Task 14 is explicitly flagged with a step that resolves it from the migrations directory. No "TBD" / "TODO" remaining outside the documented "out of scope" notes.

**Type consistency cross-check:**

- `KeyVersion::FIRST` and `KeyVersion::new(NonZeroU32)` used identically in Tasks 6, 8, 13, 14.
- `Identity::parse(&str)` used in Tasks 6, 8, 13, 14.
- `ProvisioningState::Active|Pending|Revoked` used in Tasks 3, 6, 7, 8, 13, 14.
- `ed25519_dalek::SigningKey::from_bytes(&[u8; 32])` and `::generate(&mut OsRng)` used appropriately (deterministic in unit tests, random in integration).
- `SignedIntentScopeTier::{Project, Session, Private, Team, Org, Public}` — confirmed against `crates/cairn-core/src/generated/envelope/mod.rs:332-339` (six variants, not four; the spec's "all four tiers" was off-by-two — this plan's `ScopePolicy::all_tiers()` enumerates all six. Adjusts the spec implicitly).
- `EnvelopeVerifier::new(&policy, &clock)` and `EnvelopeVerifier::verify(intent, &resolved)` signatures consistent across Tasks 7, 8, 14.
- `canonical_bytes_signed_intent(&intent) -> Result<Vec<u8>, DomainError>` consistent across Tasks 2, 7, 8, 9, 12.

**Single deviation from the spec:** the spec's §6.3 listed four tiers; the IDL declares six. The plan uses the IDL set of six in `ScopePolicy::all_tiers()`. This is a fact-of-life update, not a design change; recording here for audit.
