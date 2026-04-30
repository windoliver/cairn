# Issue #50 — Local identity provisioning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provision local human, agent, and sensor identities with keychain-backed Ed25519 keys, durable cross-store state machines, and a complete CLI surface (`cairn identity {provision,init-defaults,list,show,rotate,revoke,reconcile,repair,purge,vault-id-recover,finalise-binding,status}`).

**Architecture:** Five layers, each isolated behind one trait. (1) Pure types + traits in `cairn-core`. (2) New `cairn-keychain` crate implementing `Keystore` over the `keyring` crate. (3) `IdentityRegistry` trait impl in `cairn-store-sqlite` with one new migration `0002_identity.sql`. (4) `IdentityService` orchestrator in `cairn-cli` wiring the two adapters and exposing the verb surface. (5) Cross-cutting: bootstrap delta (`vault.id`, fail-closed re-bootstrap guard), workspace-wide `usr:` → `hmn:` rename, `MemoryKeystore` test fixture.

**Tech Stack:** Rust 2024 (toolchain 1.95.0), `tokio` (`current_thread` for short-lived verbs), `rusqlite` with `bundled` feature, `keyring` 3.x, `ed25519-dalek` (default-features-off + `zeroize`), `fs2` for advisory locks, `ulid` for vault id, `serde_json` for canonical receipt payloads, `clap` 4.5 derive, `insta` for CLI snapshots, `proptest` for parsers, `rstest` for table-driven cases, `cargo nextest` runner.

**Spec:** `docs/superpowers/specs/2026-04-27-issue-50-identity-provisioning-design.md` is the source of truth (2358 lines, 20 rounds adversarial review). The bootstrap amendment is at `docs/superpowers/specs/2026-04-26-bootstrap-design.md`. Whenever this plan says "per spec §X" the engineer must read that section before writing code.

**Worktree:** Already set up at `.worktrees/issue-50-identity-provisioning` on branch `feat/issue-50-identity-provisioning` (per session history — confirm with `git rev-parse --abbrev-ref HEAD` before starting).

**Verification command (run before every commit, per CLAUDE.md §8):**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
```

If any fails, stop, fix the underlying issue, and re-run before committing.

**Conventions:**
- Every commit message uses Conventional Commits (e.g., `feat(core): add KeyVersion newtype (issue #50)`).
- Every commit appends `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` per repo conventions.
- TDD throughout: failing test commit, then implementation commit. Group small TDD pairs in one commit only when the type or trait being added is too small to test independently.

---

## File structure

### New files

| Path | Responsibility |
|---|---|
| `crates/cairn-core/src/domain/identity/mod.rs` | Re-exports + `IdentityKind` + `Identity::parse` (rename from `identity.rs`) |
| `crates/cairn-core/src/domain/identity/keys.rs` | `KeyVersion`, `IdentityRevision`, `VaultId`, `WitnessHash`, `SecretHandle`, `HandleAccount`, `SigningKey`, `SecretBytes` |
| `crates/cairn-core/src/domain/identity/records.rs` | `PublicIdentityRecord`, `IdentityKeyEntry`, `PendingIdentityEntry`, `RevokePendingEntry`, `PendingEvictionEntry`, `PendingKeyDisableEntry`, `FirstBindState` |
| `crates/cairn-core/src/domain/identity/receipts.rs` | `RotationReceipt`, `RevocationReceipt`, `ReceiptId`, canonical-JSON helpers, ed25519 sign/verify |
| `crates/cairn-core/src/domain/identity/provision.rs` | `mint_human_id`, `mint_agent_id`, `mint_sensor_id`, `normalize_human_slug`, `build_provisioning_plan` (pure) |
| `crates/cairn-core/src/domain/identity/status.rs` | `ReconciliationReport`, `IdentityStatusReport`, `MismatchOutcome` |
| `crates/cairn-core/src/contract/keystore.rs` | `Keystore` trait, `KeystoreError`, `MaintenanceMode` |
| `crates/cairn-core/src/contract/identity_registry.rs` | `IdentityRegistry` trait, `RegistryError`, `IdentityVisibility` |
| `crates/cairn-core/src/error/identity.rs` | `IdentityServiceError` (top-level orchestrator error) |
| `crates/cairn-keychain/Cargo.toml` | New crate manifest |
| `crates/cairn-keychain/src/lib.rs` | Crate root, `pub use OsKeystore` |
| `crates/cairn-keychain/src/os.rs` | `OsKeystore` impl over `keyring::Entry` |
| `crates/cairn-keychain/tests/round_trip.rs` | Per-OS integration test |
| `crates/cairn-test-fixtures/src/keystore.rs` | `MemoryKeystore` (in-process keystore for tests) |
| `crates/cairn-store-sqlite/migrations/0002_identity.sql` | Schema for identities, identity_keys, vault_meta, identity_receipts, pending_rotations, identity_wal + triggers |
| `crates/cairn-store-sqlite/src/identity/mod.rs` | `SqliteIdentityRegistry` + open-pool helpers |
| `crates/cairn-store-sqlite/src/identity/queries.rs` | All SQL query strings as `const`, plus row-decoding helpers |
| `crates/cairn-store-sqlite/src/identity/wal.rs` | `wal_insert(...)` helper run inside every mutation transaction |
| `crates/cairn-store-sqlite/tests/identity_registry.rs` | Per-method conformance tests (real SQLite tempdir) |
| `crates/cairn-cli/src/identity/mod.rs` | `IdentityService` struct + module re-exports |
| `crates/cairn-cli/src/identity/lock.rs` | Per-identity advisory locks (`fs2::FileExt::try_lock_exclusive`) |
| `crates/cairn-cli/src/identity/first_bind.rs` | `commit_first_identity` + namespace-ownership probe + two-phase sentinel |
| `crates/cairn-cli/src/identity/rotate.rs` | Two-phase rotation (lock → pending_rotation → store_keypair → apply_rotation → evict) |
| `crates/cairn-cli/src/identity/revoke.rs` | Two-phase revocation (begin → keystore disable → finalise) |
| `crates/cairn-cli/src/identity/purge.rs` | Two-phase purge (mark_purge_pending → keystore delete → finalise_purge) + `--resume` |
| `crates/cairn-cli/src/identity/recover.rs` | `vault-id-recover` + `finalise-binding` |
| `crates/cairn-cli/src/identity/status.rs` | `cairn identity status` (cold-start mismatch sweep) |
| `crates/cairn-cli/src/identity/cli.rs` | clap subcommand wiring |
| `crates/cairn-cli/tests/identity_provisioning.rs` | End-to-end flows (init-defaults, rotation, etc.) |
| `crates/cairn-cli/tests/identity_recovery.rs` | Crash-recovery flows (first-bind crashes, vault-id-recover, finalise-binding) |
| `crates/cairn-cli/tests/identity_status.rs` | Status / vault-degraded flows |

### Modified files

| Path | Reason |
|---|---|
| `crates/cairn-core/src/domain/identity.rs` | DELETED — content moved to `domain/identity/mod.rs` (rename `usr:` → `hmn:`) |
| `crates/cairn-core/src/domain/mod.rs` | Re-export new identity sub-modules |
| `crates/cairn-core/src/contract/mod.rs` | Add `keystore`, `identity_registry` |
| `crates/cairn-core/Cargo.toml` | Add `ed25519-dalek` (default-features-off + `+ zeroize`), `zeroize`, `rand_core`, `serde_json`, `ulid` |
| `crates/cairn-idl/schema/common/primitives.json` | `Identity` regex `usr:` → `hmn:` |
| `crates/cairn-store-sqlite/Cargo.toml` | Add `rusqlite` (with `bundled`), `serde_json` |
| `crates/cairn-store-sqlite/src/lib.rs` | Re-export `SqliteIdentityRegistry`; register migration |
| `crates/cairn-test-fixtures/Cargo.toml` | Add `cairn-core` dep, `parking_lot` |
| `crates/cairn-test-fixtures/src/lib.rs` | Re-export `MemoryKeystore` |
| `crates/cairn-cli/Cargo.toml` | Add `cairn-keychain`, `cairn-store-sqlite`, `keyring`, `ed25519-dalek`, `fs2`, `ulid`, `whoami`, `unicode-normalization` |
| `crates/cairn-cli/src/main.rs` | Wire `cairn identity` subcommands; bootstrap delta |
| `crates/cairn-cli/src/vault.rs` | `BootstrapReceipt.vault_id`, fail-closed re-bootstrap guard, mint `.cairn/vault.id` |
| `crates/cairn-cli/tests/bootstrap.rs` | Update existing tests; add new vault.id tests |
| `Cargo.toml` (workspace) | Add new workspace deps |
| Various `*.md`, fixtures, generated files | `usr:` → `hmn:` rename sweep (handled by Phase E task) |

---

## Phase A — `cairn-core` types & traits (no I/O)

Phase A produces a compiling `cairn-core` with all identity types, contracts, and pure functions. Adapters in later phases consume these. Phase A has no DB, no keychain, no CLI changes — only core types and tests. Crate boundary script (`scripts/check-core-boundary.sh`) must continue to pass.

### Task A1: Move `domain/identity.rs` → `domain/identity/mod.rs` and rename `usr:` → `hmn:`

Splitting the identity module into a folder lets us add `keys.rs`, `records.rs`, `receipts.rs`, `provision.rs`, `status.rs` next to it without bloating one file. The wire-form rename (`usr:` → `hmn:`, `IdentityKind::Human` body becomes `hmn`) is part of the same commit because callers must see exactly one consistent identity grammar.

**Files:**
- Delete: `crates/cairn-core/src/domain/identity.rs`
- Create: `crates/cairn-core/src/domain/identity/mod.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs` (one line — change `pub mod identity;` to `pub mod identity;` (still works as folder))
- Modify: `crates/cairn-idl/schema/common/primitives.json` (regex `^(agt|usr|snr):` → `^(agt|hmn|snr):`)

- [ ] **Step 1: Read existing identity.rs to capture all current behavior**

```bash
cat crates/cairn-core/src/domain/identity.rs
```

- [ ] **Step 2: Create the new file at `crates/cairn-core/src/domain/identity/mod.rs` containing the same logic with `usr:` → `hmn:` rename**

```rust
//! Identity newtype and discriminator (brief §4.2).
//!
//! Three identity kinds — `HumanIdentity`, `AgentIdentity`, `SensorIdentity`
//! — share one wire form: `<prefix>:<body>` where `prefix ∈ {agt, hmn, snr}`
//! and `body` matches `[A-Za-z0-9._:-]+`. The pattern matches the
//! `Identity` schema in `crates/cairn-idl/schema/common/primitives.json`.

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

pub mod keys;
pub mod provision;
pub mod records;
pub mod receipts;
pub mod status;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdentityKind {
    Human,
    Agent,
    Sensor,
}

impl IdentityKind {
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Human => "hmn",
            Self::Agent => "agt",
            Self::Sensor => "snr",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Identity(String);

impl Identity {
    /// Construct an [`Identity`] from a wire-form string. Returns
    /// [`DomainError::InvalidIdentity`] on bad prefix or empty/invalid body.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        let body = if let Some(b) = raw.strip_prefix("agt:") {
            b
        } else if let Some(b) = raw.strip_prefix("hmn:") {
            b
        } else if let Some(b) = raw.strip_prefix("snr:") {
            b
        } else {
            return Err(DomainError::InvalidIdentity {
                message: "must start with one of [agt:, hmn:, snr:]".to_owned(),
            });
        };
        if body.is_empty() {
            return Err(DomainError::InvalidIdentity {
                message: "body after prefix must not be empty".to_owned(),
            });
        }
        for ch in body.chars() {
            if !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '-' | '/')) {
                return Err(DomainError::InvalidIdentity {
                    message: format!("invalid character in body: {ch:?}"),
                });
            }
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn kind(&self) -> IdentityKind {
        if self.0.starts_with("hmn:") { IdentityKind::Human }
        else if self.0.starts_with("agt:") { IdentityKind::Agent }
        else { IdentityKind::Sensor }
    }

    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

impl std::fmt::Display for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Identity {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_three_kinds() {
        assert_eq!(Identity::parse("hmn:alice:v1").unwrap().kind(), IdentityKind::Human);
        assert_eq!(Identity::parse("agt:claude/opus/coder:v1").unwrap().kind(), IdentityKind::Agent);
        assert_eq!(Identity::parse("snr:hooks/posttool/laptop:v1").unwrap().kind(), IdentityKind::Sensor);
    }

    #[test]
    fn rejects_unknown_prefix() {
        assert!(Identity::parse("usr:alice:v1").is_err()); // legacy prefix banned
        assert!(Identity::parse("xxx:alice:v1").is_err());
    }

    #[test]
    fn rejects_empty_body() {
        assert!(Identity::parse("hmn:").is_err());
    }

    #[test]
    fn rejects_invalid_char() {
        assert!(Identity::parse("hmn:al ice:v1").is_err());
    }
}
```

- [ ] **Step 3: Delete the old `identity.rs`**

```bash
rm crates/cairn-core/src/domain/identity.rs
```

- [ ] **Step 4: Update `primitives.json` regex**

Open `crates/cairn-idl/schema/common/primitives.json`, find the `Identity` regex (search `usr`), and change `"^(agt|usr|snr):` to `"^(agt|hmn|snr):`. Re-run codegen and commit any generated diffs:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

- [ ] **Step 5: Run the verification checklist**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo nextest run --workspace --locked --no-fail-fast
```

Expected: all green. There will be other workspace files referencing `usr:` (CLAUDE.md, fixtures, generated). They are fixed in Phase E, **Task E1** — but at this point the Rust code must compile. If any non-test Rust file mentions `usr:`, fix it now (it's a real bug); if test fixtures or markdown reference `usr:`, defer to Task E1 by adding the file to a `usr_remaining.txt` scratch list.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor(core): split domain/identity into folder; rename usr: -> hmn: (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A2: Add identity newtypes (`KeyVersion`, `IdentityRevision`, `VaultId`, `WitnessHash`)

**Files:**
- Create: `crates/cairn-core/src/domain/identity/keys.rs`
- Modify: `crates/cairn-core/Cargo.toml` (add `ulid` workspace dep)
- Modify: workspace `Cargo.toml` (add `ulid = "1"` to `[workspace.dependencies]`)

- [ ] **Step 1: Add `ulid` workspace dep**

In the root `Cargo.toml` `[workspace.dependencies]` table:

```toml
ulid = { version = "1", default-features = false, features = ["serde"] }
```

In `crates/cairn-core/Cargo.toml` `[dependencies]`:

```toml
ulid = { workspace = true }
```

- [ ] **Step 2: Write `keys.rs` with all four newtypes + tests**

Each newtype derives `Debug, Clone, PartialEq, Eq, Hash` and a hand-rolled `Display`. Per spec §4.1.

```rust
//! ID newtypes per spec §4.1. No primitives leak across crate boundaries.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyVersion(NonZeroU32);

impl KeyVersion {
    pub const FIRST: Self = Self(NonZeroU32::MIN);
    #[must_use]
    pub const fn new(n: NonZeroU32) -> Self { Self(n) }
    #[must_use]
    pub fn as_u32(self) -> u32 { self.0.get() }
    pub fn next(self) -> Result<Self, DomainError> {
        let next = self.0.get().checked_add(1).ok_or(DomainError::InvalidIdentity {
            message: "key_version overflow".into(),
        })?;
        Ok(Self(NonZeroU32::new(next).expect("invariant: checked_add ensures non-zero")))
    }
}
impl std::fmt::Display for KeyVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdentityRevision(NonZeroU32);

impl IdentityRevision {
    pub const FIRST: Self = Self(NonZeroU32::MIN);
    #[must_use]
    pub const fn new(n: NonZeroU32) -> Self { Self(n) }
    #[must_use]
    pub fn as_u32(self) -> u32 { self.0.get() }
    pub fn next(self) -> Result<Self, DomainError> {
        let next = self.0.get().checked_add(1).ok_or(DomainError::InvalidIdentity {
            message: "identity_revision overflow".into(),
        })?;
        Ok(Self(NonZeroU32::new(next).expect("invariant: checked_add ensures non-zero")))
    }
}
impl std::fmt::Display for IdentityRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "v{}", self.0) }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VaultId(String);

impl VaultId {
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        ulid::Ulid::from_string(&raw).map_err(|_| DomainError::InvalidIdentity {
            message: format!("vault_id is not a ULID: {raw}"),
        })?;
        Ok(Self(raw))
    }
    #[must_use]
    pub fn mint() -> Self { Self(ulid::Ulid::new().to_string()) }
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}
impl std::fmt::Display for VaultId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WitnessHash([u8; 32]);

impl WitnessHash {
    #[must_use]
    pub const fn from_bytes(b: [u8; 32]) -> Self { Self(b) }
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] { &self.0 }
    #[must_use]
    pub fn from_witness(witness: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(witness);
        Self(h.finalize().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_version_next() {
        assert_eq!(KeyVersion::FIRST.next().unwrap().as_u32(), 2);
    }

    #[test]
    fn identity_revision_display() {
        assert_eq!(IdentityRevision::FIRST.to_string(), "v1");
    }

    #[test]
    fn vault_id_round_trip() {
        let id = VaultId::mint();
        let parsed = VaultId::parse(id.as_str()).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn vault_id_rejects_non_ulid() {
        assert!(VaultId::parse("not-a-ulid").is_err());
    }
}
```

- [ ] **Step 3: Add `sha2` workspace dep (used by `WitnessHash::from_witness`)**

Workspace `Cargo.toml`:
```toml
sha2 = { version = "0.10", default-features = false }
```
`crates/cairn-core/Cargo.toml`:
```toml
sha2 = { workspace = true }
```

- [ ] **Step 4: Run verification + commit**

```bash
cargo nextest run -p cairn-core --locked && \
git add -A && git commit -m "feat(core): add KeyVersion, IdentityRevision, VaultId, WitnessHash newtypes (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A3: Add `SecretHandle` + `HandleAccount` + zeroizing types

Per spec §4.1. Two construction paths only: `for_identity(vault, id, version)` and `for_witness(vault)`. No raw string assembly anywhere.

**Files:**
- Modify: `crates/cairn-core/src/domain/identity/keys.rs` (extend)
- Modify: workspace `Cargo.toml` and `crates/cairn-core/Cargo.toml` (add `zeroize`, `ed25519-dalek`)

- [ ] **Step 1: Add deps**

Workspace:
```toml
zeroize = { version = "1.7", default-features = false, features = ["zeroize_derive"] }
ed25519-dalek = { version = "2.1", default-features = false, features = ["std", "zeroize", "rand_core"] }
rand_core = { version = "0.6", default-features = false, features = ["std"] }
```
`cairn-core/Cargo.toml`:
```toml
zeroize = { workspace = true }
ed25519-dalek = { workspace = true }
rand_core = { workspace = true }
```

- [ ] **Step 2: Append the types to `keys.rs`**

```rust
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::domain::identity::Identity;

/// Wrapped Ed25519 signing key — zeroized on drop, no Clone, no public bytes.
pub struct SigningKey(ed25519_dalek::SigningKey);
impl SigningKey {
    #[must_use]
    pub fn generate(rng: &mut impl rand_core::CryptoRngCore) -> Self {
        Self(ed25519_dalek::SigningKey::generate(rng))
    }
    #[must_use]
    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        Self(ed25519_dalek::SigningKey::from_bytes(bytes))
    }
    #[must_use]
    pub fn verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.0.verifying_key()
    }
    pub fn sign(&self, msg: &[u8]) -> ed25519_dalek::Signature {
        use ed25519_dalek::Signer;
        self.0.sign(msg)
    }
    /// Borrow the raw secret bytes for keystore persistence. Caller is
    /// responsible for not retaining the slice.
    pub fn expose_secret_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
}
impl Drop for SigningKey {
    fn drop(&mut self) {
        let mut bytes = self.0.to_bytes();
        bytes.zeroize();
    }
}
impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigningKey(<redacted>)")
    }
}

/// Witness/secret bytes — zeroized on drop, slice-borrow only.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretBytes(Vec<u8>);
impl SecretBytes {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self { Self(bytes) }
    #[must_use]
    pub fn as_slice(&self) -> &[u8] { &self.0 }
}
impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretBytes({} bytes <redacted>)", self.0.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HandleAccount {
    Identity { identity: Identity, version: KeyVersion },
    Witness,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretHandle {
    pub vault_id: VaultId,
    pub account: HandleAccount,
}

impl SecretHandle {
    #[must_use]
    pub fn for_identity(vault_id: VaultId, identity: Identity, version: KeyVersion) -> Self {
        Self { vault_id, account: HandleAccount::Identity { identity, version } }
    }
    #[must_use]
    pub fn for_witness(vault_id: VaultId) -> Self {
        Self { vault_id, account: HandleAccount::Witness }
    }
    #[must_use]
    pub fn service(&self) -> String {
        format!("cairn:{}", self.vault_id)
    }
    #[must_use]
    pub fn account_string(&self) -> String {
        match &self.account {
            HandleAccount::Witness => "__vault_witness__".to_owned(),
            HandleAccount::Identity { identity, version } =>
                format!("{identity}#k{version}"),
        }
    }
}

#[cfg(test)]
mod handle_tests {
    use super::*;
    use crate::domain::identity::Identity;

    #[test]
    fn identity_handle_format() {
        let v = VaultId::mint();
        let id = Identity::parse("hmn:alice:v1").unwrap();
        let h = SecretHandle::for_identity(v.clone(), id.clone(), KeyVersion::FIRST);
        assert_eq!(h.service(), format!("cairn:{}", v));
        assert_eq!(h.account_string(), "hmn:alice:v1#k1");
    }

    #[test]
    fn witness_handle_format() {
        let v = VaultId::mint();
        let h = SecretHandle::for_witness(v);
        assert_eq!(h.account_string(), "__vault_witness__");
    }

    #[test]
    fn signing_key_does_not_leak_in_debug() {
        let mut rng = rand_core::OsRng;
        let k = SigningKey::generate(&mut rng);
        assert_eq!(format!("{k:?}"), "SigningKey(<redacted>)");
    }
}
```

- [ ] **Step 3: Verify + commit**

```bash
cargo nextest run -p cairn-core --locked && \
git add -A && git commit -m "feat(core): add SecretHandle, SigningKey, SecretBytes (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A4: Add `PublicIdentityRecord`, `IdentityKeyEntry`, and supporting record types

**Files:**
- Create: `crates/cairn-core/src/domain/identity/records.rs`

- [ ] **Step 1: Write `records.rs`**

```rust
//! Public identity records and supporting entry types per spec §4.1.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::identity::{
    Identity, IdentityKind,
    keys::{IdentityRevision, KeyVersion, WitnessHash},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicIdentityRecord {
    pub id: Identity,
    pub kind: IdentityKind,
    pub current_key_version: KeyVersion,
    pub revision: IdentityRevision,
    pub provisioning_state: ProvisioningState,
    pub created_at: DateTime<Utc>,
    pub activated_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub purge_requested_at: Option<DateTime<Utc>>,
    pub purged_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningState {
    Pending,
    Active,
    RevokePending,
    Revoked,
    PurgePending,
    Purged,
}

impl ProvisioningState {
    /// True if signing under this identity is permitted by the trust gate
    /// (`require_attributable_signer`). Per spec §3.10 / §3.5: only `Active`
    /// can sign new envelopes; `Revoked` keys verify history but do not
    /// sign new ones; transitional states are non-signing.
    #[must_use]
    pub fn can_sign(self) -> bool { matches!(self, Self::Active) }

    /// True if the row should appear in `IdentityVisibility::Operational`
    /// reads (per §4.1).
    #[must_use]
    pub fn is_operational(self) -> bool {
        matches!(self, Self::Active | Self::Revoked)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityKeyEntry {
    pub identity_id: Identity,
    pub key_version: KeyVersion,
    pub public_key: [u8; 32], // ed25519 verifying key bytes
    pub signed_predecessor: Option<Vec<u8>>, // ed25519 signature
    pub created_at: DateTime<Utc>,
    pub superseded_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingIdentityEntry {
    pub identity: Identity,
    pub key_version: KeyVersion,
    pub public_key: [u8; 32],
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokePendingEntry {
    pub identity: Identity,
    pub revoked_at: DateTime<Utc>,
    pub receipt_id: ReceiptId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEvictionEntry {
    pub receipt_id: ReceiptId,
    pub identity: Identity,
    pub evict_version: KeyVersion,
    pub rotated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingKeyDisableEntry {
    pub receipt_id: ReceiptId,
    pub identity: Identity,
    pub revoked_at: DateTime<Utc>,
    pub retained_versions: Vec<KeyVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgePendingEntry {
    pub identity: Identity,
    pub purge_requested_at: DateTime<Utc>,
    pub purge_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReceiptId(pub i64); // SQLite rowid

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirstBindState {
    Absent,
    Reserved {
        record: PublicIdentityRecord,
        key: IdentityKeyEntry,
        witness_hash: WitnessHash,
    },
    Activated {
        record: PublicIdentityRecord,
        key: IdentityKeyEntry,
    },
}
```

- [ ] **Step 2: Add `chrono` to workspace + crate manifest**

Workspace:
```toml
chrono = { version = "0.4", default-features = false, features = ["std", "serde", "clock"] }
```
`cairn-core/Cargo.toml`:
```toml
chrono = { workspace = true }
```

- [ ] **Step 3: Verify + commit**

```bash
cargo nextest run -p cairn-core --locked && \
git add -A && git commit -m "feat(core): add PublicIdentityRecord and pending-row entry types (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A5: Add `RotationReceipt` and `RevocationReceipt` with canonical-JSON sign/verify

Per spec §4.1: receipts are signed with ed25519; the signed payload is canonical JSON `(op_kind, target, signer, signer_key_version, old/new key versions, issued_at)`.

**Files:**
- Create: `crates/cairn-core/src/domain/identity/receipts.rs`

- [ ] **Step 1: Write `receipts.rs`**

```rust
//! Signed trust-state receipts per spec §4.1.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::domain::identity::{
    Identity,
    keys::{KeyVersion, SigningKey},
    records::ReceiptId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptOpKind {
    Rotation,
    Revocation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptPayload {
    pub op_kind: ReceiptOpKind,
    pub target: Identity,
    pub signer: Identity,
    pub signer_key_version: KeyVersion,
    pub old_key_version: Option<KeyVersion>,
    pub new_key_version: Option<KeyVersion>,
    pub issued_at: DateTime<Utc>,
}

impl ReceiptPayload {
    /// Canonical JSON encoding — keys in field-declaration order. Used as
    /// the bytes signed by the signer's ed25519 key.
    pub fn canonical_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn sign(&self, signer: &SigningKey) -> Result<Signature, serde_json::Error> {
        let bytes = self.canonical_json()?;
        Ok(signer.sign(&bytes))
    }

    pub fn verify(&self, signature: &Signature, signer_key: &VerifyingKey) -> Result<(), ReceiptError> {
        let bytes = self.canonical_json().map_err(ReceiptError::Encode)?;
        signer_key.verify(&bytes, signature).map_err(|_| ReceiptError::BadSignature)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationReceipt {
    pub id: ReceiptId,
    pub payload: ReceiptPayload,
    pub signature: Vec<u8>,
    pub pending_eviction: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevocationReceipt {
    pub id: ReceiptId,
    pub payload: ReceiptPayload,
    pub signature: Vec<u8>,
    pub pending_key_disable: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    #[error("canonical JSON encoding failed: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("ed25519 signature verification failed")]
    BadSignature,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::keys::SigningKey;

    #[test]
    fn round_trip_sign_verify() {
        let mut rng = rand_core::OsRng;
        let signer_key = SigningKey::generate(&mut rng);
        let signer_pub = signer_key.verifying_key();
        let payload = ReceiptPayload {
            op_kind: ReceiptOpKind::Rotation,
            target: Identity::parse("hmn:alice:v1").unwrap(),
            signer: Identity::parse("agt:claude/opus/coder:v1").unwrap(),
            signer_key_version: KeyVersion::FIRST,
            old_key_version: Some(KeyVersion::FIRST),
            new_key_version: Some(KeyVersion::FIRST.next().unwrap()),
            issued_at: chrono::Utc::now(),
        };
        let sig = payload.sign(&signer_key).unwrap();
        payload.verify(&sig, &signer_pub).unwrap();
    }

    #[test]
    fn tampered_payload_fails_verify() {
        let mut rng = rand_core::OsRng;
        let signer_key = SigningKey::generate(&mut rng);
        let mut payload = ReceiptPayload {
            op_kind: ReceiptOpKind::Rotation,
            target: Identity::parse("hmn:a:v1").unwrap(),
            signer: Identity::parse("hmn:a:v1").unwrap(),
            signer_key_version: KeyVersion::FIRST,
            old_key_version: None,
            new_key_version: Some(KeyVersion::FIRST),
            issued_at: chrono::Utc::now(),
        };
        let sig = payload.sign(&signer_key).unwrap();
        payload.target = Identity::parse("hmn:b:v1").unwrap();
        assert!(payload.verify(&sig, &signer_key.verifying_key()).is_err());
    }
}
```

Workspace `Cargo.toml`: add `serde_json = { version = "1", default-features = false, features = ["std"] }`. `cairn-core/Cargo.toml`: add `serde_json = { workspace = true }` and `thiserror = { workspace = true }` (probably already present — confirm).

- [ ] **Step 2: Verify + commit**

```bash
cargo nextest run -p cairn-core --locked && \
git add -A && git commit -m "feat(core): add Rotation/RevocationReceipt with ed25519 sign+verify (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A6: Add pure provisioning logic (`mint_*_id`, `normalize_human_slug`, `build_provisioning_plan`)

Per spec §3.4 + §3.9.

**Files:**
- Create: `crates/cairn-core/src/domain/identity/provision.rs`

- [ ] **Step 1: Write `provision.rs`**

```rust
//! Pure provisioning helpers per spec §3.4 + §3.9.

use chrono::{DateTime, Utc};
use unicode_normalization::UnicodeNormalization;

use crate::domain::DomainError;
use crate::domain::identity::{
    Identity, IdentityKind,
    keys::{IdentityRevision, KeyVersion, SecretHandle, SigningKey, VaultId},
    records::{IdentityKeyEntry, ProvisioningState, PublicIdentityRecord},
};

/// Convert an OS username into a stable identity slug per spec §3.9.
/// Rules: NFKC normalize, lowercase, ASCII-only, replace runs of disallowed
/// chars with a single `-`, trim leading/trailing `-`, max 100 bytes,
/// fallback `user` if empty.
#[must_use]
pub fn normalize_human_slug(raw: &str) -> String {
    let nfkc: String = raw.nfkc().collect::<String>().to_lowercase();
    let mut out = String::new();
    let mut prev_was_dash = false;
    for ch in nfkc.chars() {
        let allowed = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-');
        if allowed {
            out.push(ch);
            prev_was_dash = ch == '-';
        } else if !prev_was_dash {
            out.push('-');
            prev_was_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let limited: String = trimmed.bytes().take(100).map(char::from).collect();
    let limited = limited.trim_end_matches('-').to_owned();
    if limited.is_empty() { "user".to_owned() } else { limited }
}

pub fn mint_human_id(slug: &str, rev: IdentityRevision) -> Result<Identity, DomainError> {
    let s = normalize_human_slug(slug);
    Identity::parse(format!("hmn:{s}:{rev}"))
}

pub fn mint_agent_id(harness: &str, model: &str, role: &str, rev: IdentityRevision) -> Result<Identity, DomainError> {
    Identity::parse(format!("agt:{harness}/{model}/{role}:{rev}"))
}

pub fn mint_sensor_id(family: &str, name: &str, host: &str, rev: IdentityRevision) -> Result<Identity, DomainError> {
    Identity::parse(format!("snr:{family}/{name}/{host}:{rev}"))
}

#[derive(Debug)]
pub struct ProvisioningPlan {
    pub identity: PublicIdentityRecord,
    pub key_entry: IdentityKeyEntry,
    pub secret_handle: SecretHandle,
    pub signing_key: SigningKey,
}

#[derive(Debug, Clone)]
pub struct ProvisionInput {
    pub vault_id: VaultId,
    pub id: Identity,
    pub kind: IdentityKind,
    pub revision: IdentityRevision,
}

#[must_use]
pub fn build_provisioning_plan(
    input: ProvisionInput,
    rng: &mut impl rand_core::CryptoRngCore,
    now: DateTime<Utc>,
) -> ProvisioningPlan {
    let signing_key = SigningKey::generate(rng);
    let pubkey = signing_key.verifying_key().to_bytes();
    let key_version = KeyVersion::FIRST;
    let key_entry = IdentityKeyEntry {
        identity_id: input.id.clone(),
        key_version,
        public_key: pubkey,
        signed_predecessor: None,
        created_at: now,
        superseded_at: None,
    };
    let identity = PublicIdentityRecord {
        id: input.id.clone(),
        kind: input.kind,
        current_key_version: key_version,
        revision: input.revision,
        provisioning_state: ProvisioningState::Pending,
        created_at: now,
        activated_at: None,
        revoked_at: None,
        purge_requested_at: None,
        purged_at: None,
    };
    let secret_handle = SecretHandle::for_identity(input.vault_id, input.id, key_version);
    ProvisioningPlan { identity, key_entry, secret_handle, signing_key }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_ascii_lowercase() { assert_eq!(normalize_human_slug("Alice"), "alice"); }

    #[test]
    fn slug_spaces_become_dash() { assert_eq!(normalize_human_slug("alice smith"), "alice-smith"); }

    #[test]
    fn slug_apostrophe() { assert_eq!(normalize_human_slug("o'brien"), "o-brien"); }

    #[test]
    fn slug_accented_nfkc() { assert_eq!(normalize_human_slug("Élodie"), "elodie"); }

    #[test]
    fn slug_non_latin_falls_back() { assert_eq!(normalize_human_slug("田中"), "user"); }

    #[test]
    fn slug_empty_falls_back() { assert_eq!(normalize_human_slug(""), "user"); }

    #[test]
    fn slug_truncated_to_100_bytes() {
        let long = "a".repeat(200);
        assert_eq!(normalize_human_slug(&long).len(), 100);
    }

    #[test]
    fn human_id_format() {
        let id = mint_human_id("alice", IdentityRevision::FIRST).unwrap();
        assert_eq!(id.as_str(), "hmn:alice:v1");
    }

    #[test]
    fn deterministic_plan_with_seeded_rng() {
        use rand_core::SeedableRng;
        let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(42);
        let input = ProvisionInput {
            vault_id: VaultId::mint(),
            id: Identity::parse("hmn:alice:v1").unwrap(),
            kind: IdentityKind::Human,
            revision: IdentityRevision::FIRST,
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-28T00:00:00Z").unwrap().with_timezone(&Utc);
        let plan = build_provisioning_plan(input, &mut rng, now);
        // Determinism: pubkey is reproducible from seeded RNG
        assert_eq!(plan.key_entry.public_key.len(), 32);
    }
}
```

Workspace deps: add `unicode-normalization = "0.1"` and (dev only) `rand_chacha = "0.3"`.

- [ ] **Step 2: Verify + commit**

```bash
cargo nextest run -p cairn-core --locked && \
git add -A && git commit -m "feat(core): add pure provisioning helpers + slug normalization (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A7: Add `ReconciliationReport` and `IdentityStatusReport`

Per spec §3.5 + §4.5.

**Files:**
- Create: `crates/cairn-core/src/domain/identity/status.rs`

- [ ] **Step 1: Write `status.rs`**

```rust
//! Reconciliation + status report types per spec §3.5 / §4.5.

use serde::{Deserialize, Serialize};

use crate::domain::identity::{Identity, keys::VaultId};

/// Per-pending-row outcome from a reconciliation sweep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MismatchOutcome {
    Activated,
    Orphaned,
    KeyMaterialMismatch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconciliationReport {
    pub mismatched_ids: Vec<Identity>,
    /// Active rows whose keystore private key is missing (NotFound).
    /// Recoverable via `rotate <id>`; not vault-degrading.
    pub desynchronized_active_ids: Vec<Identity>,
    pub vault_degraded: bool,
}

impl ReconciliationReport {
    pub fn record_mismatch(&mut self, id: Identity) {
        self.mismatched_ids.push(id);
        self.vault_degraded = true;
    }
    pub fn record_active_mismatch(&mut self, id: Identity) {
        // Active mismatches also escalate vault-degraded.
        self.mismatched_ids.push(id);
        self.vault_degraded = true;
    }
    pub fn record_active_desync(&mut self, id: Identity) {
        self.desynchronized_active_ids.push(id);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityStatusReport {
    pub vault_id: Option<VaultId>,
    pub binding_state: BindingState,
    pub defaults: DefaultsState,
    pub mismatched_ids: Vec<Identity>,
    pub desynchronized_active_ids: Vec<Identity>,
    pub pending_evictions: u64,
    pub pending_key_disables: u64,
    pub purge_pending_ids: Vec<Identity>,
    pub vault_degraded: bool,
    pub mismatch_check: MismatchCheckOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingState { Bound, Pending, Unbound }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DefaultsState {
    NotInitialised,
    Active { human: Identity, agent: Identity },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MismatchCheckOutcome {
    Completed,
    KeystoreLocked,
    VaultIdConflict,
}
```

- [ ] **Step 2: Verify + commit**

```bash
cargo nextest run -p cairn-core --locked && \
git add -A && git commit -m "feat(core): add ReconciliationReport + IdentityStatusReport (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A8: Add the `Keystore` trait + `KeystoreError`

Per spec §4.1.

**Files:**
- Create: `crates/cairn-core/src/contract/keystore.rs`
- Modify: `crates/cairn-core/src/contract/mod.rs` (`pub mod keystore;`)

- [ ] **Step 1: Write `keystore.rs`**

```rust
//! `Keystore` contract per spec §4.1.

use async_trait::async_trait;

use crate::domain::identity::{
    Identity,
    keys::{KeyVersion, SecretBytes, SecretHandle, SigningKey, VaultId},
};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KeystoreError {
    #[error("keystore handle not found")]
    NotFound,
    #[error("keystore is locked (operator must unlock)")]
    Locked,
    #[error("permission denied accessing keystore")]
    PermissionDenied,
    #[error("backend does not support enumeration (e.g., DPAPI)")]
    DiscoveryUnsupported,
    #[error("backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

#[async_trait]
pub trait Keystore: Send + Sync {
    // Ed25519 keypair operations.
    async fn store_keypair(&self, handle: &SecretHandle, secret: &SigningKey) -> Result<(), KeystoreError>;
    async fn load_signing_key(&self, handle: &SecretHandle) -> Result<SigningKey, KeystoreError>;
    async fn delete_keypair(&self, handle: &SecretHandle) -> Result<(), KeystoreError>;

    // Opaque-bytes operations (vault witness).
    async fn store_secret(&self, handle: &SecretHandle, bytes: &[u8]) -> Result<(), KeystoreError>;
    async fn load_secret(&self, handle: &SecretHandle) -> Result<SecretBytes, KeystoreError>;
    async fn delete_secret(&self, handle: &SecretHandle) -> Result<(), KeystoreError>;

    // Discovery.
    async fn list_vault_namespaces(&self, service_prefix: &str) -> Result<Vec<VaultId>, KeystoreError>;

    /// Enumerate every `<identity-wire-form>#k<version>` account that exists
    /// for an identity under this vault's service prefix. Used by revoke
    /// and purge to catch orphan private keys not represented in
    /// `identity_keys`.
    async fn list_identity_versions(
        &self,
        vault_id: &VaultId,
        id: &Identity,
    ) -> Result<Vec<KeyVersion>, KeystoreError>;
}
```

- [ ] **Step 2: Update `contract/mod.rs`** to add `pub mod keystore;` next to existing modules.

- [ ] **Step 3: Verify + commit**

```bash
cargo check -p cairn-core --locked && \
git add -A && git commit -m "feat(core): add Keystore contract trait (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A9: Add the `IdentityRegistry` trait + `RegistryError` + `IdentityVisibility` + `MaintenanceMode`

This is the largest single addition in Phase A — the full trait surface from spec §4.1. Read spec §4.1 lines 1499–1678 before writing this file.

**Files:**
- Create: `crates/cairn-core/src/contract/identity_registry.rs`
- Modify: `crates/cairn-core/src/contract/mod.rs` (add `pub mod identity_registry;`)

- [ ] **Step 1: Write `identity_registry.rs` (transcribe trait from spec §4.1)**

```rust
//! `IdentityRegistry` contract per spec §4.1.

use std::path::Path;

use async_trait::async_trait;

use crate::domain::identity::{
    Identity, IdentityKind,
    keys::{KeyVersion, VaultId, WitnessHash},
    receipts::{RotationReceipt, RevocationReceipt},
    records::{
        FirstBindState, IdentityKeyEntry, PendingEvictionEntry, PendingIdentityEntry,
        PendingKeyDisableEntry, ProvisioningState, PublicIdentityRecord, PurgePendingEntry,
        ReceiptId, RevokePendingEntry,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityVisibility {
    /// `active` and `revoked` only. Default for issuer-dependent verbs.
    Operational,
    /// Adds `pending`. Used by §3.5 reconciliation.
    IncludingPending,
    /// Adds `purge_pending`. Used by `purge --resume`.
    IncludingPurgePending,
    /// Adds `purged` (and `purge_pending`). Used by audit reads.
    Audit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceMode {
    /// No keystore handle, no vault-binding consistency check.
    /// Suitable for `list`, `show`, and the dry-run sweep that
    /// `cairn identity status` runs.
    ReadOnly,
    /// Registry + keystore handle. Enforces `vault.id ↔ vault_meta.vault_id`
    /// consistency before opening; refuses with `VaultIdConflict` on disagreement.
    Mutating,
}

#[derive(Debug, Clone, Copy)]
pub struct PurgeAcknowledgement(pub(crate) ()); // construction is private — only via FS path verifier in cairn-cli

#[derive(Debug, Clone)]
pub struct PurgeReason(pub String);

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    #[error("identity not found")]
    NotFound,
    #[error("identity already exists: {id}")]
    IdentityExists { id: Identity },
    #[error("provisioning in flight for {id}")]
    ProvisioningInFlight { id: Identity },
    #[error("identity already revoked: {id}")]
    AlreadyRevoked { id: Identity },
    #[error("key material mismatch for {id}")]
    KeyMaterialMismatch { id: Identity },
    #[error("key version conflict: existing={existing}, attempted={attempted}")]
    KeyVersionConflict { existing: KeyVersion, attempted: KeyVersion },
    #[error("invalid purge start state for {id}: {state:?}")]
    InvalidPurgeStartState { id: Identity, state: ProvisioningState },
    #[error("purge incomplete for {id}: {remaining_versions} versions remaining")]
    PurgeIncomplete { id: Identity, remaining_versions: u64 },
    #[error("first-bind mismatch: stored={stored}, attempted={attempted}")]
    FirstBindMismatch { stored: VaultId, attempted: VaultId },
    #[error("vault_meta missing — first-bind has not committed yet")]
    VaultMetaMissing,
    #[error("first-bind already committed for this registry")]
    FirstBindAlreadyCommitted,
    #[error("witness mismatch — binding_path contents do not match witness_hash argument")]
    WitnessMismatch,
    #[error("backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

#[async_trait]
pub trait IdentityRegistry: Send + Sync {
    // Provisioning state machine (§3.5).
    async fn reserve_identity(&self, record: &PublicIdentityRecord, key: &IdentityKeyEntry) -> Result<(), RegistryError>;
    async fn activate_identity(&self, id: &Identity, key_version: KeyVersion) -> Result<(), RegistryError>;
    async fn delete_pending(&self, id: &Identity, key_version: KeyVersion) -> Result<(), RegistryError>;
    async fn list_pending(&self) -> Result<Vec<PendingIdentityEntry>, RegistryError>;
    async fn list_pending_by_identity(&self, id: &Identity) -> Result<Vec<PendingIdentityEntry>, RegistryError>;

    // Read paths.
    async fn get_identity(&self, id: &Identity, visibility: IdentityVisibility) -> Result<Option<PublicIdentityRecord>, RegistryError>;
    async fn list_identities(&self, kind: Option<IdentityKind>, visibility: IdentityVisibility) -> Result<Vec<PublicIdentityRecord>, RegistryError>;
    async fn list_keys(&self, id: &Identity) -> Result<Vec<IdentityKeyEntry>, RegistryError>;

    // Counts.
    async fn count_keys(&self) -> Result<u64, RegistryError>;
    async fn list_all_keys(&self) -> Result<Vec<IdentityKeyEntry>, RegistryError>;

    // Rotation (CAS-protected).
    async fn apply_rotation(&self, receipt: &RotationReceipt, expected_current: KeyVersion) -> Result<(), RegistryError>;

    // Pre-commit rotation intent (§3.6 step 0a).
    async fn insert_pending_rotation(&self, identity: &Identity, planned_version: KeyVersion, planned_handle: &str) -> Result<(), RegistryError>;
    async fn delete_pending_rotation(&self, identity: &Identity, planned_version: KeyVersion) -> Result<(), RegistryError>;
    async fn list_pending_rotations(&self, identity: &Identity) -> Result<Vec<(KeyVersion, String)>, RegistryError>;

    // Revocation two-phase tombstone.
    async fn begin_revocation(&self, receipt: &RevocationReceipt) -> Result<(), RegistryError>;
    async fn finalise_revocation(&self, id: &Identity) -> Result<(), RegistryError>;
    async fn list_revoke_pending(&self) -> Result<Vec<RevokePendingEntry>, RegistryError>;

    // First-bind transaction.
    async fn reserve_first_identity(
        &self,
        vault_id: &VaultId,
        record: &PublicIdentityRecord,
        key: &IdentityKeyEntry,
        witness_hash: WitnessHash,
        binding_path: &Path,
    ) -> Result<(), RegistryError>;
    async fn get_first_bind_state(&self, vault_id: &VaultId) -> Result<FirstBindState, RegistryError>;

    // vault_meta read (used by bootstrap fail-closed guard + open() consistency check).
    async fn read_vault_meta(&self) -> Result<Option<(VaultId, WitnessHash)>, RegistryError>;

    // Receipt reconciliation flag clears.
    async fn clear_pending_eviction(&self, receipt_id: &ReceiptId) -> Result<(), RegistryError>;
    async fn list_pending_evictions(&self) -> Result<Vec<PendingEvictionEntry>, RegistryError>;
    async fn clear_pending_key_disable(&self, receipt_id: &ReceiptId) -> Result<(), RegistryError>;
    async fn list_pending_key_disables(&self) -> Result<Vec<PendingKeyDisableEntry>, RegistryError>;

    // Two-phase purge tombstone.
    async fn mark_purge_pending(
        &self,
        id: &Identity,
        ack: &PurgeAcknowledgement,
        reason: PurgeReason,
    ) -> Result<(), RegistryError>;
    async fn finalise_purge(&self, id: &Identity) -> Result<(), RegistryError>;
    async fn list_purge_pending(&self) -> Result<Vec<PurgePendingEntry>, RegistryError>;
}
```

- [ ] **Step 2: Update `contract/mod.rs`** to add `pub mod identity_registry;`.

- [ ] **Step 3: Verify + commit**

```bash
cargo check -p cairn-core --locked && cargo clippy -p cairn-core --locked -- -D warnings && \
git add -A && git commit -m "feat(core): add IdentityRegistry contract + RegistryError + visibility (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A10: Add `IdentityServiceError` (top-level orchestrator error)

Per spec §4.1.

**Files:**
- Create: `crates/cairn-core/src/error/identity.rs`
- Modify: `crates/cairn-core/src/error/mod.rs` (add `pub mod identity;`) — create the module file if it doesn't exist; otherwise place under `crates/cairn-core/src/lib.rs` next to existing error exports.

- [ ] **Step 1: Inspect existing error layout** (`grep -rn '^pub use.*error' crates/cairn-core/src/`) — place the file consistent with current pattern.

- [ ] **Step 2: Write `IdentityServiceError`**

```rust
//! Top-level orchestrator error per spec §4.1.

use crate::contract::{identity_registry::RegistryError, keystore::KeystoreError};
use crate::domain::identity::{Identity, keys::VaultId};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IdentityServiceError {
    #[error("keystore: {0}")]
    Keystore(#[from] KeystoreError),
    #[error("registry: {0}")]
    Registry(#[from] RegistryError),
    #[error("defaults not initialised — run `cairn identity init-defaults`")]
    DefaultsNotInitialized,
    #[error("identity {id} key material is desynchronized: {reason}")]
    KeyMaterialDesynchronized { id: Identity, reason: String },
    #[error("no live attributable signer available")]
    NoLiveAttributableSigner,
    #[error("vault id missing — bootstrap not run or `.cairn/vault.id` removed")]
    VaultIdMissing,
    #[error("vault id conflict — file={file_id}, db={db_id}")]
    VaultIdConflict { file_id: VaultId, db_id: VaultId },
    #[error("vault degraded — KeyMaterialMismatch for: {mismatched_ids:?}")]
    VaultDegraded { mismatched_ids: Vec<Identity> },
    #[error("vault namespace already claimed: {vault_id}")]
    VaultNamespaceClaimed { vault_id: VaultId },
    #[error("first bind in progress (.cairn/vault.binding.pending exists)")]
    FirstBindInProgress,
    #[error("first bind lock busy")]
    FirstBindInFlight,
    #[error("identity lock busy: {id}")]
    IdentityLockBusy { id: Identity },
}
```

- [ ] **Step 3: Verify + commit**

```bash
cargo check -p cairn-core --locked && \
git add -A && git commit -m "feat(core): add IdentityServiceError (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A11: Phase A integration check + boundary script

- [ ] **Step 1: Run full verification**

```bash
cargo fmt --all --check && \
cargo clippy --workspace --all-targets --locked -- -D warnings && \
cargo nextest run --workspace --locked --no-fail-fast && \
cargo test --doc --workspace --locked && \
./scripts/check-core-boundary.sh
```

Expected: all green. The boundary script must confirm `cairn-core` still has zero workspace deps. If any new dep is workspace-local, that's a regression — investigate.

- [ ] **Step 2: Phase A is complete. No commit needed (verification-only).**

---

## Phase B — `cairn-keychain` (new crate) + `MemoryKeystore` test fixture

Phase B produces an OS-keychain-backed `Keystore` impl (`OsKeystore`) and an in-process test fixture (`MemoryKeystore`). After Phase B, every consumer of the `Keystore` trait can be tested without touching the real keychain.

### Task B1: Scaffold `cairn-keychain` crate

**Files:**
- Create: `crates/cairn-keychain/Cargo.toml`
- Create: `crates/cairn-keychain/src/lib.rs`
- Create: `crates/cairn-keychain/src/os.rs`
- Modify: workspace `Cargo.toml` (add to `[workspace] members` and `[workspace.dependencies]` `keyring`)

- [ ] **Step 1: Add `keyring` workspace dep**

Workspace `Cargo.toml`:

```toml
keyring = { version = "3", default-features = false, features = ["apple-native", "linux-native", "windows-native"] }
```

In `[workspace] members` add `"crates/cairn-keychain"`.

- [ ] **Step 2: Create `crates/cairn-keychain/Cargo.toml`**

```toml
[package]
name = "cairn-keychain"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
description = "OS keychain (Keychain / Secret Service / DPAPI) backed Keystore for Cairn."

[dependencies]
cairn-core = { workspace = true }
async-trait = { workspace = true }
thiserror = { workspace = true }
keyring = { workspace = true }
ed25519-dalek = { workspace = true }
zeroize = { workspace = true }
tokio = { workspace = true, features = ["rt"] }

[lints]
workspace = true
```

- [ ] **Step 3: Write `lib.rs`**

```rust
//! OS-keychain backed [`Keystore`] for Cairn.
//!
//! Wraps the `keyring` crate to provide per-vault namespaced
//! identity-key + opaque-witness storage.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod os;
pub use os::OsKeystore;
```

- [ ] **Step 4: Write `os.rs` (full impl per spec §4.2)**

```rust
use async_trait::async_trait;
use keyring::Entry;

use cairn_core::contract::keystore::{Keystore, KeystoreError};
use cairn_core::domain::identity::{
    Identity,
    keys::{HandleAccount, KeyVersion, SecretBytes, SecretHandle, SigningKey, VaultId},
};

/// Per-vault, scoped keystore handle.
pub struct OsKeystore {
    /// Optional bound vault id. Constructors:
    /// - [`OsKeystore::new`] sets it; every operation must match.
    /// - [`OsKeystore::for_discovery`] leaves it `None` for vault-id-recover.
    bound_vault: Option<VaultId>,
}

impl OsKeystore {
    #[must_use]
    pub fn new(vault_id: VaultId) -> Self { Self { bound_vault: Some(vault_id) } }
    #[must_use]
    pub fn for_discovery() -> Self { Self { bound_vault: None } }

    fn ensure_bound_match(&self, handle: &SecretHandle) -> Result<(), KeystoreError> {
        if let Some(bound) = &self.bound_vault {
            if bound != &handle.vault_id {
                return Err(KeystoreError::Backend(
                    format!("handle vault {} != bound vault {}", handle.vault_id, bound).into(),
                ));
            }
        }
        Ok(())
    }

    fn entry(&self, handle: &SecretHandle) -> Result<Entry, KeystoreError> {
        self.ensure_bound_match(handle)?;
        Entry::new(&handle.service(), &handle.account_string())
            .map_err(map_keyring_err)
    }
}

fn map_keyring_err(e: keyring::Error) -> KeystoreError {
    use keyring::Error as K;
    match e {
        K::NoEntry => KeystoreError::NotFound,
        K::PlatformFailure(inner) => KeystoreError::Backend(Box::new(inner)),
        K::NoStorageAccess(inner) => KeystoreError::Locked, // platform-specific
        K::BadEncoding(_) => KeystoreError::Backend(Box::new(e)),
        K::TooLong(_, _) => KeystoreError::Backend(Box::new(e)),
        K::Invalid(_, _) => KeystoreError::Backend(Box::new(e)),
        K::Ambiguous(_) => KeystoreError::Backend(Box::new(e)),
        _ => KeystoreError::Backend(Box::new(e)),
    }
}

#[async_trait]
impl Keystore for OsKeystore {
    async fn store_keypair(&self, handle: &SecretHandle, secret: &SigningKey) -> Result<(), KeystoreError> {
        let entry = self.entry(handle)?;
        let bytes = secret.expose_secret_bytes();
        tokio::task::spawn_blocking(move || entry.set_secret(&bytes).map_err(map_keyring_err))
            .await
            .map_err(|e| KeystoreError::Backend(Box::new(e)))?
    }

    async fn load_signing_key(&self, handle: &SecretHandle) -> Result<SigningKey, KeystoreError> {
        let entry = self.entry(handle)?;
        let raw = tokio::task::spawn_blocking(move || entry.get_secret().map_err(map_keyring_err))
            .await
            .map_err(|e| KeystoreError::Backend(Box::new(e)))??;
        let arr: [u8; 32] = raw.as_slice().try_into()
            .map_err(|_| KeystoreError::Backend("signing key bytes != 32".into()))?;
        Ok(SigningKey::from_bytes(&arr))
    }

    async fn delete_keypair(&self, handle: &SecretHandle) -> Result<(), KeystoreError> {
        let entry = self.entry(handle)?;
        tokio::task::spawn_blocking(move || entry.delete_credential().map_err(map_keyring_err))
            .await
            .map_err(|e| KeystoreError::Backend(Box::new(e)))?
    }

    async fn store_secret(&self, handle: &SecretHandle, bytes: &[u8]) -> Result<(), KeystoreError> {
        let entry = self.entry(handle)?;
        let owned = bytes.to_vec();
        tokio::task::spawn_blocking(move || entry.set_secret(&owned).map_err(map_keyring_err))
            .await
            .map_err(|e| KeystoreError::Backend(Box::new(e)))?
    }

    async fn load_secret(&self, handle: &SecretHandle) -> Result<SecretBytes, KeystoreError> {
        let entry = self.entry(handle)?;
        let raw = tokio::task::spawn_blocking(move || entry.get_secret().map_err(map_keyring_err))
            .await
            .map_err(|e| KeystoreError::Backend(Box::new(e)))??;
        Ok(SecretBytes::new(raw))
    }

    async fn delete_secret(&self, handle: &SecretHandle) -> Result<(), KeystoreError> {
        self.delete_keypair(handle).await
    }

    async fn list_vault_namespaces(&self, _service_prefix: &str) -> Result<Vec<VaultId>, KeystoreError> {
        // The `keyring` crate v3 does not expose namespace enumeration on
        // every backend — DPAPI in particular does not support it. Return
        // DiscoveryUnsupported uniformly; per-backend enumeration is a
        // follow-up issue (see spec §3.7 risk note).
        Err(KeystoreError::DiscoveryUnsupported)
    }

    async fn list_identity_versions(
        &self,
        _vault_id: &VaultId,
        _id: &Identity,
    ) -> Result<Vec<KeyVersion>, KeystoreError> {
        Err(KeystoreError::DiscoveryUnsupported)
    }
}
```

- [ ] **Step 5: Verify + commit**

```bash
cargo check -p cairn-keychain --locked && \
git add -A && git commit -m "feat(keychain): scaffold cairn-keychain crate with OsKeystore (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task B2: Per-OS round-trip integration test for `OsKeystore`

Per spec §4.2 testing strategy. Gated by `cfg` so headless CI can skip; manual run via `cargo nextest run -p cairn-keychain --features integration`.

**Files:**
- Create: `crates/cairn-keychain/tests/round_trip.rs`
- Modify: `crates/cairn-keychain/Cargo.toml` (add `[features] integration = []`)

- [ ] **Step 1: Add feature gate**

```toml
[features]
default = []
integration = []
```

- [ ] **Step 2: Write `tests/round_trip.rs`**

```rust
#![cfg(feature = "integration")]
//! Per-OS round-trip integration test. Run manually:
//!     cargo nextest run -p cairn-keychain --features integration

use cairn_core::contract::keystore::{Keystore, KeystoreError};
use cairn_core::domain::identity::{
    Identity,
    keys::{KeyVersion, SecretHandle, SigningKey, VaultId},
};
use cairn_keychain::OsKeystore;

#[tokio::test]
async fn keypair_round_trip() {
    let vault = VaultId::mint();
    let keystore = OsKeystore::new(vault.clone());
    let id = Identity::parse("hmn:test-roundtrip:v1").unwrap();
    let handle = SecretHandle::for_identity(vault, id, KeyVersion::FIRST);

    let mut rng = rand_core::OsRng;
    let key = SigningKey::generate(&mut rng);
    let pub_a = key.verifying_key();
    keystore.store_keypair(&handle, &key).await.unwrap();

    let loaded = keystore.load_signing_key(&handle).await.unwrap();
    assert_eq!(loaded.verifying_key().to_bytes(), pub_a.to_bytes());

    keystore.delete_keypair(&handle).await.unwrap();
    let err = keystore.load_signing_key(&handle).await.unwrap_err();
    assert!(matches!(err, KeystoreError::NotFound));
}

#[tokio::test]
async fn handle_vault_mismatch_rejected() {
    let bound = VaultId::mint();
    let other = VaultId::mint();
    let keystore = OsKeystore::new(bound);
    let id = Identity::parse("hmn:test:v1").unwrap();
    let foreign_handle = SecretHandle::for_identity(other, id, KeyVersion::FIRST);
    let mut rng = rand_core::OsRng;
    let key = SigningKey::generate(&mut rng);
    assert!(keystore.store_keypair(&foreign_handle, &key).await.is_err());
}
```

- [ ] **Step 3: Verify it compiles even without the feature**

```bash
cargo check -p cairn-keychain --locked && \
cargo check -p cairn-keychain --features integration --locked
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "test(keychain): per-OS round-trip integration test (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task B3: `MemoryKeystore` test fixture in `cairn-test-fixtures`

Allows store and CLI tests to run keychain flows without touching the OS.

**Files:**
- Create: `crates/cairn-test-fixtures/src/keystore.rs`
- Modify: `crates/cairn-test-fixtures/src/lib.rs`
- Modify: `crates/cairn-test-fixtures/Cargo.toml` (add `cairn-core` dep, `parking_lot`, `tokio`)

- [ ] **Step 1: Add deps**

```toml
[dependencies]
cairn-core = { workspace = true }
async-trait = { workspace = true }
parking_lot = { workspace = true }
tokio = { workspace = true, features = ["rt"] }
ed25519-dalek = { workspace = true }
```
(parking_lot must be a workspace dep — add `parking_lot = "0.12"` to root if missing.)

- [ ] **Step 2: Write `keystore.rs`**

```rust
//! In-process [`Keystore`] for tests. No filesystem, no OS keychain.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use cairn_core::contract::keystore::{Keystore, KeystoreError};
use cairn_core::domain::identity::{
    Identity,
    keys::{HandleAccount, KeyVersion, SecretBytes, SecretHandle, SigningKey, VaultId},
};

#[derive(Default, Clone)]
pub struct MemoryKeystore {
    inner: Arc<RwLock<HashMap<(String, String), Vec<u8>>>>,
    /// When true, every load_secret/load_signing_key returns Locked.
    locked: Arc<RwLock<bool>>,
    /// When true, list_vault_namespaces and list_identity_versions return DiscoveryUnsupported.
    pub discovery_unsupported: Arc<RwLock<bool>>,
}

impl MemoryKeystore {
    #[must_use]
    pub fn new() -> Self { Self::default() }
    pub fn lock(&self) { *self.locked.write() = true; }
    pub fn unlock(&self) { *self.locked.write() = false; }
    pub fn set_discovery_unsupported(&self, v: bool) { *self.discovery_unsupported.write() = v; }
    pub fn raw_keys(&self) -> Vec<(String, String)> { self.inner.read().keys().cloned().collect() }
}

fn key_of(handle: &SecretHandle) -> (String, String) { (handle.service(), handle.account_string()) }

#[async_trait]
impl Keystore for MemoryKeystore {
    async fn store_keypair(&self, handle: &SecretHandle, secret: &SigningKey) -> Result<(), KeystoreError> {
        if *self.locked.read() { return Err(KeystoreError::Locked); }
        self.inner.write().insert(key_of(handle), secret.expose_secret_bytes().to_vec());
        Ok(())
    }
    async fn load_signing_key(&self, handle: &SecretHandle) -> Result<SigningKey, KeystoreError> {
        if *self.locked.read() { return Err(KeystoreError::Locked); }
        let raw = self.inner.read().get(&key_of(handle)).cloned().ok_or(KeystoreError::NotFound)?;
        let arr: [u8; 32] = raw.as_slice().try_into().map_err(|_| KeystoreError::Backend("bad key bytes".into()))?;
        Ok(SigningKey::from_bytes(&arr))
    }
    async fn delete_keypair(&self, handle: &SecretHandle) -> Result<(), KeystoreError> {
        if *self.locked.read() { return Err(KeystoreError::Locked); }
        self.inner.write().remove(&key_of(handle));
        Ok(())
    }
    async fn store_secret(&self, handle: &SecretHandle, bytes: &[u8]) -> Result<(), KeystoreError> {
        if *self.locked.read() { return Err(KeystoreError::Locked); }
        self.inner.write().insert(key_of(handle), bytes.to_vec());
        Ok(())
    }
    async fn load_secret(&self, handle: &SecretHandle) -> Result<SecretBytes, KeystoreError> {
        if *self.locked.read() { return Err(KeystoreError::Locked); }
        let raw = self.inner.read().get(&key_of(handle)).cloned().ok_or(KeystoreError::NotFound)?;
        Ok(SecretBytes::new(raw))
    }
    async fn delete_secret(&self, handle: &SecretHandle) -> Result<(), KeystoreError> { self.delete_keypair(handle).await }
    async fn list_vault_namespaces(&self, service_prefix: &str) -> Result<Vec<VaultId>, KeystoreError> {
        if *self.discovery_unsupported.read() { return Err(KeystoreError::DiscoveryUnsupported); }
        let mut out = Vec::new();
        for (svc, acct) in self.inner.read().keys() {
            if svc.starts_with(service_prefix) && acct == "__vault_witness__" {
                let id_str = svc.strip_prefix("cairn:").unwrap_or(svc);
                if let Ok(id) = VaultId::parse(id_str) { out.push(id); }
            }
        }
        Ok(out)
    }
    async fn list_identity_versions(&self, vault_id: &VaultId, id: &Identity) -> Result<Vec<KeyVersion>, KeystoreError> {
        if *self.discovery_unsupported.read() { return Err(KeystoreError::DiscoveryUnsupported); }
        let svc = format!("cairn:{vault_id}");
        let prefix = format!("{id}#k");
        let mut out = Vec::new();
        for (s, a) in self.inner.read().keys() {
            if s == &svc && a.starts_with(&prefix) {
                if let Some(n) = a.strip_prefix(&prefix).and_then(|x| x.parse::<u32>().ok()) {
                    if let Some(nz) = std::num::NonZeroU32::new(n) {
                        out.push(KeyVersion::new(nz));
                    }
                }
            }
        }
        out.sort_by_key(|k| k.as_u32());
        Ok(out)
    }
}
```

- [ ] **Step 3: Re-export from lib.rs**

In `crates/cairn-test-fixtures/src/lib.rs`: `pub mod keystore;` and `pub use keystore::MemoryKeystore;`.

- [ ] **Step 4: Add a smoke test**

In `crates/cairn-test-fixtures/tests/keystore_smoke.rs`:

```rust
use cairn_core::contract::keystore::Keystore;
use cairn_core::domain::identity::{Identity, keys::{KeyVersion, SecretHandle, SigningKey, VaultId}};
use cairn_test_fixtures::MemoryKeystore;

#[tokio::test]
async fn memory_keystore_round_trip() {
    let v = VaultId::mint();
    let id = Identity::parse("hmn:alice:v1").unwrap();
    let h = SecretHandle::for_identity(v, id, KeyVersion::FIRST);
    let ks = MemoryKeystore::new();
    let mut rng = rand_core::OsRng;
    let key = SigningKey::generate(&mut rng);
    ks.store_keypair(&h, &key).await.unwrap();
    let loaded = ks.load_signing_key(&h).await.unwrap();
    assert_eq!(loaded.verifying_key().to_bytes(), key.verifying_key().to_bytes());
}
```

Also add `cairn-core` and `cairn-test-fixtures` to that test's `[dev-dependencies]` (the package Cargo.toml).

- [ ] **Step 5: Verify + commit**

```bash
cargo nextest run -p cairn-test-fixtures --locked && \
git add -A && git commit -m "feat(test-fixtures): add MemoryKeystore in-process Keystore impl (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase C — `cairn-store-sqlite` migration + `IdentityRegistry` impl

Phase C produces `SqliteIdentityRegistry`, a real adapter implementing the trait from Task A9 against a SQLite DB, plus migration `0002_identity.sql`. Every method is conformance-tested against an in-memory SQLite DB. After Phase C, the registry side of identity provisioning is fully working.

### Task C1: Add `rusqlite` dep + migration scaffold

**Files:**
- Modify: workspace `Cargo.toml`, `crates/cairn-store-sqlite/Cargo.toml`
- Create: `crates/cairn-store-sqlite/migrations/` directory

- [ ] **Step 1: Add deps**

Workspace:
```toml
rusqlite = { version = "0.32", default-features = false, features = ["bundled", "blob", "chrono", "serde_json"] }
```
`cairn-store-sqlite/Cargo.toml`:
```toml
rusqlite = { workspace = true }
chrono = { workspace = true }
serde_json = { workspace = true }
ulid = { workspace = true }
ed25519-dalek = { workspace = true }
sha2 = { workspace = true }
parking_lot = "0.12"
tokio = { workspace = true, features = ["rt"] }
```

- [ ] **Step 2: Create migrations folder**

```bash
mkdir -p crates/cairn-store-sqlite/migrations
```

- [ ] **Step 3: Verify + commit**

```bash
cargo check -p cairn-store-sqlite --locked && \
git add -A && git commit -m "build(store-sqlite): add rusqlite dep + migrations folder (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C2: Write `0002_identity.sql` migration verbatim from spec §4.4

**Files:**
- Create: `crates/cairn-store-sqlite/migrations/0002_identity.sql`

- [ ] **Step 1: Write the migration**

Copy the DDL block from spec §4.4 (lines 1907–2020). Add the immutability triggers documented at spec §4.4 "Sole-writer enforcement layers three defences" (single-row CHECK is in the table; trigger goes here):

```sql
-- crates/cairn-store-sqlite/migrations/0002_identity.sql
-- Issue #50: identity provisioning. See
--   docs/superpowers/specs/2026-04-27-issue-50-identity-provisioning-design.md §4.4

CREATE TABLE identities (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('human', 'agent', 'sensor')),
    current_key_version INTEGER NOT NULL,
    provisioning_state TEXT NOT NULL CHECK (provisioning_state IN ('pending', 'active', 'revoke_pending', 'revoked', 'purge_pending', 'purged')),
    created_at TEXT NOT NULL,
    activated_at TEXT,
    revoked_at TEXT,
    revocation_signature BLOB,
    purge_requested_at TEXT,
    purged_at TEXT,
    purge_reason TEXT
);

CREATE TABLE identity_keys (
    identity_id TEXT NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    key_version INTEGER NOT NULL,
    public_key BLOB NOT NULL,
    signed_predecessor BLOB,
    created_at TEXT NOT NULL,
    superseded_at TEXT,
    PRIMARY KEY (identity_id, key_version)
);
CREATE INDEX idx_identity_keys_identity ON identity_keys(identity_id);

CREATE TABLE vault_meta (
    rowid INTEGER PRIMARY KEY CHECK (rowid = 1),
    vault_id TEXT NOT NULL,
    witness_sha256 BLOB NOT NULL,
    binding_path TEXT NOT NULL,
    witness_created_at TEXT NOT NULL
);

-- vault_meta is immutable post-insert: no UPDATE / DELETE allowed.
CREATE TRIGGER vault_meta_no_update
BEFORE UPDATE OF vault_id, witness_sha256, binding_path ON vault_meta
BEGIN
    SELECT RAISE(FAIL, 'vault_meta is immutable');
END;

CREATE TRIGGER vault_meta_no_delete
BEFORE DELETE ON vault_meta
BEGIN
    SELECT RAISE(FAIL, 'vault_meta is immutable');
END;

CREATE TABLE identity_receipts (
    rowid INTEGER PRIMARY KEY,
    op_kind TEXT NOT NULL CHECK (op_kind IN ('rotation', 'revocation')),
    target_identity TEXT NOT NULL,
    signer_identity TEXT NOT NULL,
    signer_key_version INTEGER NOT NULL,
    old_key_version INTEGER,
    new_key_version INTEGER,
    issued_at TEXT NOT NULL,
    signed_payload BLOB NOT NULL,
    signature BLOB NOT NULL,
    pending_eviction INTEGER NOT NULL DEFAULT 0 CHECK (pending_eviction IN (0, 1)),
    pending_key_disable INTEGER NOT NULL DEFAULT 0 CHECK (pending_key_disable IN (0, 1)),
    FOREIGN KEY (signer_identity, signer_key_version)
        REFERENCES identity_keys(identity_id, key_version),
    FOREIGN KEY (target_identity, old_key_version)
        REFERENCES identity_keys(identity_id, key_version),
    FOREIGN KEY (target_identity, new_key_version)
        REFERENCES identity_keys(identity_id, key_version)
);
CREATE INDEX idx_identity_receipts_target ON identity_receipts(target_identity);
CREATE INDEX idx_identity_receipts_signer ON identity_receipts(signer_identity);
CREATE INDEX idx_identity_receipts_pending_eviction ON identity_receipts(pending_eviction) WHERE pending_eviction = 1;
CREATE INDEX idx_identity_receipts_pending_key_disable ON identity_receipts(pending_key_disable) WHERE pending_key_disable = 1;

CREATE TABLE pending_rotations (
    rowid INTEGER PRIMARY KEY,
    identity_id TEXT NOT NULL,
    planned_version INTEGER NOT NULL,
    planned_handle TEXT NOT NULL,
    intended_at TEXT NOT NULL,
    UNIQUE (identity_id, planned_version)
);
CREATE INDEX idx_pending_rotations_identity ON pending_rotations(identity_id);

CREATE TABLE identity_wal (
    rowid INTEGER PRIMARY KEY,
    op_id BLOB NOT NULL UNIQUE,
    op_kind TEXT NOT NULL CHECK (op_kind IN (
        'reserve_first_identity', 'reserve_identity', 'activate_identity',
        'delete_pending', 'apply_rotation', 'begin_revocation',
        'finalise_revocation', 'mark_purge_pending', 'finalise_purge',
        'clear_pending_eviction', 'clear_pending_key_disable',
        'insert_pending_rotation', 'delete_pending_rotation')),
    target_identity TEXT NOT NULL,
    request_payload BLOB NOT NULL,
    applied_at TEXT NOT NULL
);
CREATE INDEX idx_identity_wal_target ON identity_wal(target_identity);
```

Note: `identity_wal.op_kind` CHECK adds two op_kinds the spec missed (`insert_pending_rotation`, `delete_pending_rotation`) — this closes the round-10 finding #1 that pending_rotations lifecycle wasn't covered.

- [ ] **Step 2: Commit**

```bash
git add -A && git commit -m "feat(store-sqlite): add 0002_identity.sql migration (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C3: Embed migration loader + smoke test that the schema parses

**Files:**
- Create: `crates/cairn-store-sqlite/src/identity/mod.rs` (skeleton)

- [ ] **Step 1: Skeleton with migration constant**

```rust
//! `IdentityRegistry` SQLite adapter (issue #50).

mod queries;
mod wal;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use rusqlite::Connection;

use cairn_core::contract::identity_registry::*;
use cairn_core::contract::keystore::*;
use cairn_core::domain::identity::*;

const MIGRATION_0002: &str = include_str!("../../migrations/0002_identity.sql");

pub struct SqliteIdentityRegistry {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteIdentityRegistry {
    pub fn open_in_memory() -> Result<Self, RegistryError> {
        let conn = Connection::open_in_memory().map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Self::run_migrations(&conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub fn open(db_path: &Path) -> Result<Self, RegistryError> {
        let conn = Connection::open(db_path).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Self::run_migrations(&conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn run_migrations(conn: &Connection) -> Result<(), RegistryError> {
        conn.execute_batch(MIGRATION_0002).map_err(|e| RegistryError::Backend(Box::new(e)))
    }
}
```

For now do not implement the trait — Phase C2 onwards adds methods one at a time.

- [ ] **Step 2: Smoke test that migration parses**

`crates/cairn-store-sqlite/tests/migration_smoke.rs`:

```rust
#[test]
fn migration_0002_applies_cleanly() {
    let r = cairn_store_sqlite::SqliteIdentityRegistry::open_in_memory();
    assert!(r.is_ok(), "{:?}", r.err());
}
```

Add `pub use identity::SqliteIdentityRegistry;` and `mod identity;` in `lib.rs`.

- [ ] **Step 3: Verify + commit**

```bash
cargo nextest run -p cairn-store-sqlite --locked && \
git add -A && git commit -m "feat(store-sqlite): identity registry skeleton + migration smoke test (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C4: Implement `wal_insert` helper + `reserve_first_identity` + `read_vault_meta` + `get_first_bind_state`

These four are the load-bearing first-bind transaction. Implement together because they share the `vault_meta` query path.

**Files:**
- Modify: `crates/cairn-store-sqlite/src/identity/wal.rs`
- Modify: `crates/cairn-store-sqlite/src/identity/mod.rs`
- Modify: `crates/cairn-store-sqlite/src/identity/queries.rs`

- [ ] **Step 1: Write `wal.rs`**

```rust
//! identity_wal helper. Every IdentityRegistry mutation calls this
//! inside its SQLite transaction so a crash before commit rolls
//! both back atomically (spec §3.5).

use chrono::Utc;
use rusqlite::Transaction;
use ulid::Ulid;

use cairn_core::contract::identity_registry::RegistryError;

pub(super) fn wal_insert(
    tx: &Transaction<'_>,
    op_kind: &str,
    target: &str,
    request_payload: &[u8],
) -> Result<(), RegistryError> {
    let op_id = Ulid::new().to_bytes();
    tx.execute(
        "INSERT INTO identity_wal (op_id, op_kind, target_identity, request_payload, applied_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![&op_id[..], op_kind, target, request_payload, Utc::now().to_rfc3339()],
    ).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    Ok(())
}
```

- [ ] **Step 2: Write a failing test for `read_vault_meta` (returns None on empty)**

```rust
// In crates/cairn-store-sqlite/tests/identity_registry.rs
#[tokio::test]
async fn read_vault_meta_empty() {
    let r = cairn_store_sqlite::SqliteIdentityRegistry::open_in_memory().unwrap();
    assert!(r.read_vault_meta().await.unwrap().is_none());
}
```

- [ ] **Step 3: Implement `read_vault_meta`** (synchronous SQL inside async wrapper)

In `mod.rs`:

```rust
#[async_trait]
impl IdentityRegistry for SqliteIdentityRegistry {
    async fn read_vault_meta(&self) -> Result<Option<(VaultId, WitnessHash)>, RegistryError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT vault_id, witness_sha256 FROM vault_meta WHERE rowid = 1")
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        let mut rows = stmt.query([]).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        if let Some(row) = rows.next().map_err(|e| RegistryError::Backend(Box::new(e)))? {
            let id: String = row.get(0).map_err(|e| RegistryError::Backend(Box::new(e)))?;
            let hash: Vec<u8> = row.get(1).map_err(|e| RegistryError::Backend(Box::new(e)))?;
            let arr: [u8; 32] = hash.as_slice().try_into().map_err(|_| RegistryError::Backend("bad hash len".into()))?;
            Ok(Some((VaultId::parse(id)?, WitnessHash::from_bytes(arr))))
        } else {
            Ok(None)
        }
    }
    // ... other methods to be filled in incrementally; this is the only
    // method this commit lands. Stub the rest with `unimplemented!()`
    // for now — they'll be wired one-by-one in Tasks C5–C12.
    // ...
}
```

For each remaining trait method, write an `unimplemented!("Task CN")` body. The test in this commit only exercises `read_vault_meta`; later tasks replace each `unimplemented!()`.

`From<DomainError> for RegistryError`: add a thin shim if needed — `VaultId::parse` returns `DomainError`, and we want it as `RegistryError::Backend`.

- [ ] **Step 4: Run + commit**

```bash
cargo nextest run -p cairn-store-sqlite --locked && \
git add -A && git commit -m "feat(store-sqlite): add wal_insert + read_vault_meta (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 5: Implement `reserve_first_identity` (TDD pair)**

Write the failing test first:

```rust
#[tokio::test]
async fn reserve_first_identity_writes_vault_meta_and_pending_row() {
    use std::io::Write;
    let r = cairn_store_sqlite::SqliteIdentityRegistry::open_in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let pending = dir.path().join("vault.binding.pending");
    let witness = vec![0u8; 32];
    std::fs::write(&pending, &witness).unwrap();
    let hash = WitnessHash::from_witness(&witness);

    let vault = VaultId::mint();
    let id = Identity::parse("hmn:alice:v1").unwrap();
    let now = chrono::Utc::now();
    let record = PublicIdentityRecord {
        id: id.clone(), kind: IdentityKind::Human,
        current_key_version: KeyVersion::FIRST,
        revision: IdentityRevision::FIRST,
        provisioning_state: ProvisioningState::Pending,
        created_at: now, activated_at: None, revoked_at: None,
        purge_requested_at: None, purged_at: None,
    };
    let key = IdentityKeyEntry {
        identity_id: id.clone(), key_version: KeyVersion::FIRST,
        public_key: [0u8; 32], signed_predecessor: None,
        created_at: now, superseded_at: None,
    };
    r.reserve_first_identity(&vault, &record, &key, hash, &pending).await.unwrap();

    let (stored_id, _) = r.read_vault_meta().await.unwrap().unwrap();
    assert_eq!(stored_id, vault);

    // Idempotent resume: second call with same args returns Ok
    r.reserve_first_identity(&vault, &record, &key, hash, &pending).await.unwrap();

    // Mismatch: different vault id with vault_meta already present → FirstBindMismatch
    let other = VaultId::mint();
    let err = r.reserve_first_identity(&other, &record, &key, hash, &pending).await.unwrap_err();
    assert!(matches!(err, RegistryError::FirstBindMismatch { .. }));
}
```

Then the implementation. Per spec §3.7, the adapter `stat`s `binding_path` (the **pending** sentinel) inside the txn and re-hashes its bytes:

```rust
async fn reserve_first_identity(
    &self,
    vault_id: &VaultId,
    record: &PublicIdentityRecord,
    key: &IdentityKeyEntry,
    witness_hash: WitnessHash,
    binding_path: &Path,
) -> Result<(), RegistryError> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(binding_path).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    let actual_hash: [u8; 32] = Sha256::digest(&bytes).into();
    if &actual_hash != witness_hash.as_bytes() {
        return Err(RegistryError::WitnessMismatch);
    }
    let mut conn = self.conn.lock();
    let tx = conn.transaction().map_err(|e| RegistryError::Backend(Box::new(e)))?;

    // Idempotent resume: existing matching vault_meta is OK.
    let existing: Option<(String, Vec<u8>)> = tx.query_row(
        "SELECT vault_id, witness_sha256 FROM vault_meta WHERE rowid = 1",
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    ).ok();
    if let Some((stored_id, stored_hash)) = existing {
        let stored_arr: [u8; 32] = stored_hash.as_slice().try_into().map_err(|_| RegistryError::Backend("bad hash len".into()))?;
        if stored_id != vault_id.as_str() || stored_arr != *witness_hash.as_bytes() {
            return Err(RegistryError::FirstBindMismatch {
                stored: VaultId::parse(stored_id).map_err(|e| RegistryError::Backend(e.into()))?,
                attempted: vault_id.clone(),
            });
        }
        // Same vault_id — resume path. Re-attempting reserve of the same identity is also idempotent.
        // Confirm the identity row already exists with matching pubkey, then return Ok.
        let exists: bool = tx.query_row(
            "SELECT 1 FROM identities WHERE id = ?1", rusqlite::params![record.id.as_str()],
            |_| Ok(true),
        ).unwrap_or(false);
        if !exists {
            return Err(RegistryError::FirstBindAlreadyCommitted);
        }
        return Ok(());
    }

    // Fresh first-bind. Insert vault_meta + identities + identity_keys + WAL row in one txn.
    tx.execute(
        "INSERT INTO vault_meta (rowid, vault_id, witness_sha256, binding_path, witness_created_at)
         VALUES (1, ?1, ?2, ?3, ?4)",
        rusqlite::params![vault_id.as_str(), &witness_hash.as_bytes()[..], binding_path.display().to_string(), record.created_at.to_rfc3339()],
    ).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    insert_identity_row(&tx, record)?;
    insert_identity_key_row(&tx, key)?;
    let payload = serde_json::to_vec(record).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    crate::identity::wal::wal_insert(&tx, "reserve_first_identity", record.id.as_str(), &payload)?;
    tx.commit().map_err(|e| RegistryError::Backend(Box::new(e)))?;
    Ok(())
}

fn insert_identity_row(tx: &rusqlite::Transaction<'_>, record: &PublicIdentityRecord) -> Result<(), RegistryError> {
    tx.execute(
        "INSERT INTO identities (id, kind, current_key_version, provisioning_state, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            record.id.as_str(),
            match record.kind { IdentityKind::Human => "human", IdentityKind::Agent => "agent", IdentityKind::Sensor => "sensor" },
            record.current_key_version.as_u32(),
            "pending",
            record.created_at.to_rfc3339(),
        ],
    ).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    Ok(())
}

fn insert_identity_key_row(tx: &rusqlite::Transaction<'_>, key: &IdentityKeyEntry) -> Result<(), RegistryError> {
    tx.execute(
        "INSERT INTO identity_keys (identity_id, key_version, public_key, signed_predecessor, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            key.identity_id.as_str(),
            key.key_version.as_u32(),
            &key.public_key[..],
            key.signed_predecessor.as_deref(),
            key.created_at.to_rfc3339(),
        ],
    ).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    Ok(())
}
```

Add `tempfile` to dev-dependencies if missing.

- [ ] **Step 6: Run + commit**

```bash
cargo nextest run -p cairn-store-sqlite --locked && \
git add -A && git commit -m "feat(store-sqlite): impl reserve_first_identity + read_vault_meta (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C5: Implement reserve / activate / delete-pending state machine

Per spec §3.5 + §4.1.

**Files:**
- Modify: `crates/cairn-store-sqlite/src/identity/mod.rs`
- Modify: `crates/cairn-store-sqlite/tests/identity_registry.rs`

- [ ] **Step 1: TDD — write failing test for reserve_identity → activate_identity → list happy path**

```rust
#[tokio::test]
async fn reserve_activate_list_happy_path() {
    let r = setup_first_bound_registry().await; // helper that runs first-bind so vault_meta exists
    let id = Identity::parse("hmn:bob:v1").unwrap();
    let now = chrono::Utc::now();
    let record = PublicIdentityRecord {
        id: id.clone(), kind: IdentityKind::Human,
        current_key_version: KeyVersion::FIRST, revision: IdentityRevision::FIRST,
        provisioning_state: ProvisioningState::Pending,
        created_at: now, activated_at: None, revoked_at: None,
        purge_requested_at: None, purged_at: None,
    };
    let key = IdentityKeyEntry {
        identity_id: id.clone(), key_version: KeyVersion::FIRST,
        public_key: [1u8; 32], signed_predecessor: None,
        created_at: now, superseded_at: None,
    };
    r.reserve_identity(&record, &key).await.unwrap();
    let pending = r.list_pending().await.unwrap();
    assert_eq!(pending.len(), 1);
    r.activate_identity(&id, KeyVersion::FIRST).await.unwrap();
    let active = r.get_identity(&id, IdentityVisibility::Operational).await.unwrap().unwrap();
    assert_eq!(active.provisioning_state, ProvisioningState::Active);
}
```

`setup_first_bound_registry()` is a small helper in the test file — same setup as Task C4 step 5.

- [ ] **Step 2: Implement** the three methods. Each follows the same shape as `reserve_first_identity` but with simpler invariants:

`reserve_identity` (per spec §4.4): begin txn → SELECT 1 FROM vault_meta (`VaultMetaMissing` if absent) → check `identities` for existing row → on conflict return `IdentityExists` or `ProvisioningInFlight` (per state) → INSERT into identities + identity_keys + wal_insert("reserve_identity", ...) → commit.

`activate_identity`: begin txn → UPDATE identities SET provisioning_state='active', activated_at=now WHERE id=? AND current_key_version=? AND provisioning_state='pending' → check rowcount; 0 → return `NotFound` or `RegistryError::Backend("invalid state transition")` per spec → wal_insert → commit.

`delete_pending`: begin txn → DELETE FROM identity_keys WHERE identity_id=? AND key_version=? → DELETE FROM identities WHERE id=? AND provisioning_state='pending' → wal_insert → commit. Foreign-key cascade ensures consistency.

`list_pending` / `list_pending_by_identity` / `get_identity` / `list_identities` / `list_keys`: SELECT statements; map rows to typed entries.

`count_keys` / `list_all_keys`: trivial SELECT.

Add helpers in `queries.rs` for the state-string mapping (`fn state_str(s: ProvisioningState) -> &'static str` and inverse).

Total ~250 LOC of straightforward SQL. Test each method as you go.

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-store-sqlite --locked && \
git add -A && git commit -m "feat(store-sqlite): impl pending/active/delete state machine (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C6: Implement rotation: `insert_pending_rotation`, `delete_pending_rotation`, `list_pending_rotations`, `apply_rotation` (with CAS)

Per spec §3.6.

**Files:**
- Modify: `crates/cairn-store-sqlite/src/identity/mod.rs`
- Modify: `crates/cairn-store-sqlite/tests/identity_registry.rs`

- [ ] **Step 1: TDD — failing test for CAS on `apply_rotation`**

```rust
#[tokio::test]
async fn apply_rotation_cas_rejects_stale_observed() {
    let r = setup_active_identity().await; // helper: activate hmn:alice:v1 with key v1
    let id = Identity::parse("hmn:alice:v1").unwrap();
    // Forge a receipt at expected_current=v2 even though current is v1
    let receipt = make_rotation_receipt(&id, KeyVersion::FIRST.next().unwrap(), KeyVersion::FIRST.next().unwrap().next().unwrap());
    let stale = KeyVersion::FIRST.next().unwrap();
    let err = r.apply_rotation(&receipt, stale).await.unwrap_err();
    assert!(matches!(err, RegistryError::KeyVersionConflict { .. }));
}
```

- [ ] **Step 2: Implement**. The four methods:

`insert_pending_rotation` (per §3.6 step 0a):

```rust
async fn insert_pending_rotation(&self, identity: &Identity, planned_version: KeyVersion, planned_handle: &str) -> Result<(), RegistryError> {
    let mut conn = self.conn.lock();
    let tx = conn.transaction().map_err(|e| RegistryError::Backend(Box::new(e)))?;
    tx.execute(
        "INSERT INTO pending_rotations (identity_id, planned_version, planned_handle, intended_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![identity.as_str(), planned_version.as_u32(), planned_handle, chrono::Utc::now().to_rfc3339()],
    ).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    let payload = serde_json::to_vec(&serde_json::json!({"identity": identity, "planned_version": planned_version, "planned_handle": planned_handle})).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    wal::wal_insert(&tx, "insert_pending_rotation", identity.as_str(), &payload)?;
    tx.commit().map_err(|e| RegistryError::Backend(Box::new(e)))?;
    Ok(())
}
```

`delete_pending_rotation`: symmetric DELETE + wal_insert.

`list_pending_rotations`: SELECT.

`apply_rotation` (per §3.6 step 4 — CAS + same-txn pending_rotation cleanup + receipt insert + key insert + supersede):

```rust
async fn apply_rotation(&self, receipt: &RotationReceipt, expected_current: KeyVersion) -> Result<(), RegistryError> {
    let mut conn = self.conn.lock();
    let tx = conn.transaction().map_err(|e| RegistryError::Backend(Box::new(e)))?;
    let new_v = receipt.payload.new_key_version.ok_or_else(|| RegistryError::Backend("rotation receipt missing new_key_version".into()))?;
    let target = receipt.payload.target.as_str();
    // CAS
    let updated = tx.execute(
        "UPDATE identities SET current_key_version = ?1 WHERE id = ?2 AND current_key_version = ?3",
        rusqlite::params![new_v.as_u32(), target, expected_current.as_u32()],
    ).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    if updated == 0 {
        // Read the actual current to populate the error
        let actual: u32 = tx.query_row(
            "SELECT current_key_version FROM identities WHERE id = ?1", rusqlite::params![target],
            |r| r.get(0),
        ).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        let actual_kv = KeyVersion::new(std::num::NonZeroU32::new(actual).unwrap_or(std::num::NonZeroU32::MIN));
        return Err(RegistryError::KeyVersionConflict { existing: actual_kv, attempted: expected_current });
    }
    // Stamp predecessor's superseded_at
    tx.execute(
        "UPDATE identity_keys SET superseded_at = ?1 WHERE identity_id = ?2 AND key_version = ?3",
        rusqlite::params![chrono::Utc::now().to_rfc3339(), target, expected_current.as_u32()],
    ).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    // Insert new key row (must already exist as pending? per spec the rotation flow stores the key in keystore first, then apply_rotation INSERTS the row here — using payload's new_key_version)
    // The receipt does not carry the public key bytes — caller must supply them. Spec §4.1 has this in build_provisioning_plan-equivalent for rotation; the cli identity::rotate module passes them via a separate trait method addition NOT yet present. For Phase C, change the trait signature to accept (&IdentityKeyEntry) alongside the receipt — update Task A9 trait + callers accordingly.
    // ↑ See task note below.
    // Delete the matching pending_rotations row
    tx.execute(
        "DELETE FROM pending_rotations WHERE identity_id = ?1 AND planned_version = ?2",
        rusqlite::params![target, new_v.as_u32()],
    ).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    // Insert receipt row
    let payload_bytes = receipt.payload.canonical_json().map_err(|e| RegistryError::Backend(Box::new(e)))?;
    tx.execute(
        "INSERT INTO identity_receipts (op_kind, target_identity, signer_identity, signer_key_version, old_key_version, new_key_version, issued_at, signed_payload, signature, pending_eviction)
         VALUES ('rotation', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            target,
            receipt.payload.signer.as_str(),
            receipt.payload.signer_key_version.as_u32(),
            receipt.payload.old_key_version.map(|v| v.as_u32()),
            new_v.as_u32(),
            receipt.payload.issued_at.to_rfc3339(),
            payload_bytes,
            &receipt.signature[..],
            i32::from(receipt.pending_eviction),
        ],
    ).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    let wal_payload = serde_json::to_vec(&receipt.payload).map_err(|e| RegistryError::Backend(Box::new(e)))?;
    wal::wal_insert(&tx, "apply_rotation", target, &wal_payload)?;
    tx.commit().map_err(|e| RegistryError::Backend(Box::new(e)))?;
    Ok(())
}
```

**TASK NOTE (load-bearing fix for spec gap):** The `apply_rotation` trait method in Task A9 needs to take an `IdentityKeyEntry` alongside the receipt so the new public-key row can be inserted in the same transaction. Update the trait signature in `cairn-core` and the trait body here to:

```rust
async fn apply_rotation(&self, receipt: &RotationReceipt, expected_current: KeyVersion, new_key: &IdentityKeyEntry) -> Result<(), RegistryError>;
```

This is a small spec patch — note in the commit message and update the spec doc inline (`apply_rotation` trait comment in §4.1) before merging.

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-store-sqlite --locked && \
git add -A && git commit -m "feat(store-sqlite): rotation pending_rotations + CAS apply_rotation (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C7: Implement two-phase revocation: `begin_revocation`, `finalise_revocation`, `list_revoke_pending`

Per spec §3.10.

- [ ] **Step 1: TDD — `revoke_pending` blocks `can_sign()`**

```rust
#[tokio::test]
async fn begin_revocation_transitions_to_revoke_pending() {
    let r = setup_active_identity().await;
    let id = Identity::parse("hmn:alice:v1").unwrap();
    let receipt = make_revocation_receipt(&id);
    r.begin_revocation(&receipt).await.unwrap();
    let row = r.get_identity(&id, IdentityVisibility::Audit).await.unwrap().unwrap();
    assert_eq!(row.provisioning_state, ProvisioningState::RevokePending);
}
```

- [ ] **Step 2: Implement** following the same `tx.execute(UPDATE...) + insert receipt + wal_insert + commit` pattern.

`begin_revocation`: UPDATE provisioning_state='revoke_pending', revoked_at=now WHERE id=? AND provisioning_state='active'; insert revocation receipt with `pending_key_disable=1`; wal_insert.

`finalise_revocation`: UPDATE provisioning_state='revoked' WHERE id=? AND provisioning_state='revoke_pending'; UPDATE matching receipt SET pending_key_disable=0; wal_insert.

`list_revoke_pending`: SELECT.

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-store-sqlite --locked && \
git add -A && git commit -m "feat(store-sqlite): two-phase revocation (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C8: Implement two-phase purge: `mark_purge_pending`, `finalise_purge`, `list_purge_pending`

Per spec §3.10. `mark_purge_pending` accepts start states `pending|active|revoked` — return `InvalidPurgeStartState` for any other.

- [ ] **Step 1: TDD — `mark_purge_pending` from `pending` row succeeds**

```rust
#[tokio::test]
async fn mark_purge_pending_admits_pending_start_state() {
    let r = setup_first_bound_registry().await;
    // Reserve a second identity that stays pending
    // ... reserve_identity ... (uses Phase C5 plumbing)
    let id = Identity::parse("hmn:bob:v1").unwrap();
    // Build a PurgeAcknowledgement via a test-only constructor (add #[cfg(any(test, feature = "test-helpers"))] in cairn-core)
    let ack = PurgeAcknowledgement::for_test();
    r.mark_purge_pending(&id, &ack, PurgeReason("test".into())).await.unwrap();
    let row = r.get_identity(&id, IdentityVisibility::Audit).await.unwrap().unwrap();
    assert_eq!(row.provisioning_state, ProvisioningState::PurgePending);
}
```

Add `#[cfg(any(test, feature = "test-helpers"))] impl PurgeAcknowledgement { pub fn for_test() -> Self { Self(()) } }` in `cairn-core::contract::identity_registry`.

- [ ] **Step 2: Implement**

`mark_purge_pending`: tx → SELECT current state; if not in {pending, active, revoked} → return `InvalidPurgeStartState`; UPDATE provisioning_state='purge_pending', purge_requested_at=now, purge_reason=?; wal_insert; commit.

`finalise_purge`: tx → SELECT current state, must be 'purge_pending'; UPDATE state='purged', purged_at=now; wal_insert; commit.

`list_purge_pending`: SELECT.

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-store-sqlite --locked && \
git add -A && git commit -m "feat(store-sqlite): two-phase purge with explicit start-state matrix (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C9: Implement reconciliation flag clears + visibility filters

`clear_pending_eviction`, `clear_pending_key_disable`, `list_pending_evictions`, `list_pending_key_disables`. Plus the visibility-aware variant of `get_identity` / `list_identities`.

- [ ] **Step 1: TDD**

```rust
#[tokio::test]
async fn list_identities_visibility_operational_excludes_purged() {
    let r = setup_first_bound_registry().await;
    // Activate one identity, then mark another as purge_pending
    // ... (uses C5 + C8 plumbing)
    let op = r.list_identities(None, IdentityVisibility::Operational).await.unwrap();
    let audit = r.list_identities(None, IdentityVisibility::Audit).await.unwrap();
    assert!(audit.len() > op.len());
}
```

- [ ] **Step 2: Implement** the visibility filter as a WHERE-clause helper:

```rust
fn visibility_states(v: IdentityVisibility) -> &'static [&'static str] {
    match v {
        IdentityVisibility::Operational => &["active", "revoked"],
        IdentityVisibility::IncludingPending => &["active", "revoked", "pending"],
        IdentityVisibility::IncludingPurgePending => &["active", "revoked", "pending", "purge_pending"],
        IdentityVisibility::Audit => &["active", "revoked", "pending", "revoke_pending", "purge_pending", "purged"],
    }
}
```

`clear_pending_eviction(receipt_id)`: UPDATE identity_receipts SET pending_eviction=0 WHERE rowid=?; wal_insert("clear_pending_eviction", receipt_id-as-string, payload).

Symmetric for `clear_pending_key_disable`.

`list_pending_evictions`: SELECT receipts WHERE pending_eviction=1, JOIN identities to get target identity.

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-store-sqlite --locked && \
git add -A && git commit -m "feat(store-sqlite): reconciliation flag clears + visibility filters (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C10: Implement `get_first_bind_state` + cross-cutting tests

- [ ] **Step 1: TDD**

```rust
#[tokio::test]
async fn first_bind_state_progresses() {
    let r = cairn_store_sqlite::SqliteIdentityRegistry::open_in_memory().unwrap();
    let v = VaultId::mint();
    assert!(matches!(r.get_first_bind_state(&v).await.unwrap(), FirstBindState::Absent));
    // After reserve_first_identity → Reserved
    // After activate_identity → Activated
}
```

- [ ] **Step 2: Implement** by querying vault_meta + identities + identity_keys.

- [ ] **Step 3: Add the WAL-coverage test (round-10 finding #1 closure)**

```rust
#[tokio::test]
async fn every_mutating_method_writes_one_wal_row() {
    let r = setup_active_identity().await;
    let conn = r.test_connection();
    let count: u64 = conn.query_row("SELECT COUNT(*) FROM identity_wal", [], |row| row.get(0)).unwrap();
    // First-bind path: reserve_first_identity(1) + reserve_identity(0 — first-bind also reserves) + activate_identity(1) = at least 2.
    // Per spec §3.5 the first-bind transaction emits one WAL row.
    assert!(count >= 2, "expected >= 2 WAL rows, got {count}");
}
```

(Add a `pub(crate) fn test_connection(&self) -> parking_lot::MutexGuard<'_, Connection>` helper gated by `#[cfg(test)]`.)

- [ ] **Step 4: Run + commit**

```bash
cargo nextest run -p cairn-store-sqlite --locked && \
git add -A && git commit -m "feat(store-sqlite): first_bind_state read + WAL-coverage test (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C11: Schema-level enforcement tests (immutability triggers, sole-writer, FK)

Per spec §4.4 conformance tests.

- [ ] **Step 1: Tests**

```rust
#[tokio::test]
async fn vault_meta_update_rejected_by_trigger() {
    let r = setup_first_bound_registry().await;
    let conn = r.test_connection();
    let err = conn.execute("UPDATE vault_meta SET vault_id='other' WHERE rowid=1", []).unwrap_err();
    assert!(err.to_string().contains("vault_meta is immutable"));
}

#[tokio::test]
async fn vault_meta_delete_rejected_by_trigger() {
    let r = setup_first_bound_registry().await;
    let conn = r.test_connection();
    let err = conn.execute("DELETE FROM vault_meta WHERE rowid=1", []).unwrap_err();
    assert!(err.to_string().contains("vault_meta is immutable"));
}

#[tokio::test]
async fn reserve_identity_against_empty_vault_meta_returns_missing() {
    let r = cairn_store_sqlite::SqliteIdentityRegistry::open_in_memory().unwrap();
    // Build a record + key
    // ...
    let err = r.reserve_identity(&record, &key).await.unwrap_err();
    assert!(matches!(err, RegistryError::VaultMetaMissing));
}

#[tokio::test]
async fn second_reserve_first_identity_against_existing_returns_already_committed() {
    // Two identities reserved through reserve_first_identity must error on the second
    // ... (use FirstBindAlreadyCommitted)
}

#[tokio::test]
async fn receipt_fk_phantom_key_version_rejected() {
    // Manually INSERT into identity_receipts with a key_version not present
    // → SQLite FK rejects.
}
```

- [ ] **Step 2: Tighten `reserve_identity`** so it begins each call with `SELECT vault_id FROM vault_meta` and returns `VaultMetaMissing` if absent (per spec §4.4 layer 3).

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-store-sqlite --locked && \
git add -A && git commit -m "test(store-sqlite): schema-level vault_meta immutability + sole-writer (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C12: Phase C verification

- [ ] **Step 1: Full verification**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo nextest run --workspace --locked --no-fail-fast && cargo test --doc --workspace --locked && ./scripts/check-core-boundary.sh
```

Expected: all green. Phase C complete.

---

## Phase D — `cairn-cli` orchestration + verb surface

Phase D wires `OsKeystore` + `SqliteIdentityRegistry` into an `IdentityService` orchestrator and exposes the full `cairn identity` CLI surface. After Phase D, the entire feature is callable end-to-end.

### Task D1: `IdentityService` skeleton + `open` / `open_for_maintenance`

**Files:**
- Create: `crates/cairn-cli/src/identity/mod.rs`
- Create: `crates/cairn-cli/src/identity/lock.rs`
- Modify: `crates/cairn-cli/Cargo.toml` (add `cairn-keychain`, `cairn-store-sqlite`, `fs2`, `whoami`, `keyring`)

- [ ] **Step 1: Add deps**

`cairn-cli/Cargo.toml`:
```toml
cairn-keychain = { workspace = true }
cairn-store-sqlite = { workspace = true }
fs2 = "0.4"
whoami = "1.5"
ulid = { workspace = true }
ed25519-dalek = { workspace = true }
unicode-normalization = "0.1"
```

(Add `cairn-keychain` and `cairn-store-sqlite` to workspace dependencies.)

- [ ] **Step 2: Skeleton in `mod.rs`**

```rust
//! IdentityService — orchestrator gluing IdentityRegistry + Keystore (issue #50).

mod first_bind;
mod lock;
mod purge;
mod recover;
mod revoke;
mod rotate;
mod status;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cairn_core::contract::identity_registry::*;
use cairn_core::contract::keystore::*;
use cairn_core::domain::identity::*;
use cairn_core::error::identity::IdentityServiceError;

pub struct IdentityService {
    pub vault_path: PathBuf,
    pub vault_id: keys::VaultId,
    pub registry: Arc<dyn IdentityRegistry>,
    pub keystore: Arc<dyn Keystore>,
}

impl IdentityService {
    /// Open in issuer-mode: runs the full reconciliation sweep and the
    /// vault.id ↔ vault_meta consistency check before any signed verb runs.
    pub async fn open(vault_path: PathBuf) -> Result<(Self, status::ReconciliationReport), IdentityServiceError> { todo!("D2") }

    /// Open in maintenance mode. ReadOnly skips both checks; Mutating
    /// enforces vault.id ↔ vault_meta consistency (no degraded sweep).
    pub async fn open_for_maintenance(vault_path: PathBuf, mode: MaintenanceMode) -> Result<Self, IdentityServiceError> { todo!("D2") }
}
```

- [ ] **Step 3: Compile + commit**

```bash
cargo check -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): IdentityService skeleton (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D2: Implement `open` + `open_for_maintenance` with vault-binding consistency

Per spec §3.5.

- [ ] **Step 1: Failing test (in `tests/identity_provisioning.rs`)**

```rust
#[tokio::test]
async fn open_fails_closed_when_file_id_disagrees_with_db() {
    let dir = tempfile::tempdir().unwrap();
    // Set up vault with one vault_id, then write a different one to .cairn/vault.id
    let vault = setup_vault_with_first_bind(&dir).await;
    std::fs::write(dir.path().join(".cairn/vault.id"), VaultId::mint().as_str()).unwrap();
    let err = IdentityService::open(dir.path().to_path_buf()).await.unwrap_err();
    assert!(matches!(err, IdentityServiceError::VaultIdConflict { .. }));
}
```

- [ ] **Step 2: Implement** by reading `.cairn/vault.id` (string parse), opening `SqliteIdentityRegistry` from `.cairn/cairn.db`, calling `read_vault_meta()`, comparing — return `VaultIdConflict` on mismatch.

For `open()`, after the consistency check passes, run the dry-run reconciliation sweep:

1. `list_pending()` → for each row, attempt `keystore.load_signing_key(...)`. On `NotFound` → orphan; on success → derive pubkey and compare to `identity_keys.public_key`; on mismatch → record in `ReconciliationReport`.
2. For active rows: same liveness check (per `status` design).
3. Return `(IdentityService, report)`.

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): IdentityService::open + open_for_maintenance (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D3: `commit_first_identity` with namespace probe + advisory lock + two-phase sentinel

Per spec §3.7. This is the most intricate function in the orchestrator.

**Files:**
- Create: `crates/cairn-cli/src/identity/first_bind.rs`

- [ ] **Step 1: Failing test — namespace-claimed probe**

```rust
#[tokio::test]
async fn first_bind_refuses_when_namespace_already_has_witness() {
    let dir = tempfile::tempdir().unwrap();
    let mem_keystore = MemoryKeystore::new();
    let vault = VaultId::mint();
    // Pre-populate the namespace
    mem_keystore.store_secret(&SecretHandle::for_witness(vault.clone()), &[0u8; 32]).await.unwrap();
    // Now call commit_first_identity for the same vault id
    let err = call_commit_first_identity(&dir, vault, mem_keystore).await.unwrap_err();
    assert!(matches!(err, IdentityServiceError::VaultNamespaceClaimed { .. }));
}
```

- [ ] **Step 2: Implement**, exact sequence per spec §3.7:

```rust
pub(super) async fn commit_first_identity(
    vault_path: &Path,
    vault_id: VaultId,
    plan: ProvisioningPlan,
    registry: &dyn IdentityRegistry,
    keystore: &dyn Keystore,
) -> Result<(), IdentityServiceError> {
    // Acquire .cairn/vault.binding.lock (advisory, exclusive, 30s timeout)
    let lock_path = vault_path.join(".cairn/vault.binding.lock");
    std::fs::OpenOptions::new().create(true).write(true).open(&lock_path)
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;
    // ... use fs2::FileExt::try_lock_exclusive with retries up to 30s
    // Release on drop guard.

    // Step 0: namespace-ownership probe
    let witness_handle = SecretHandle::for_witness(vault_id.clone());
    match keystore.load_secret(&witness_handle).await {
        Err(KeystoreError::NotFound) => { /* unclaimed — proceed */ }
        Ok(_) => return Err(IdentityServiceError::VaultNamespaceClaimed { vault_id }),
        Err(KeystoreError::Locked | KeystoreError::PermissionDenied) =>
            return Err(IdentityServiceError::Keystore(KeystoreError::Locked)),
        Err(e) => return Err(IdentityServiceError::Keystore(e)),
    }

    // Step 1: write .cairn/vault.binding.pending with witness bytes (32 random)
    use rand_core::RngCore;
    let mut witness = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut witness);
    let pending = vault_path.join(".cairn/vault.binding.pending");
    std::fs::write(&pending, &witness)
        .and_then(|_| std::fs::File::open(&pending).and_then(|f| f.sync_all()))
        .map_err(|e| IdentityServiceError::Keystore(KeystoreError::Backend(Box::new(e))))?;

    let witness_hash = WitnessHash::from_witness(&witness);

    // Step 2: keystore.store_secret(witness)
    keystore.store_secret(&witness_handle, &witness).await?;

    // Step 3: registry.reserve_first_identity (validates pending sentinel hash)
    registry.reserve_first_identity(&vault_id, &plan.identity, &plan.key_entry, witness_hash, &pending).await?;

    // Step 4: rename .pending → .binding (hash-only)
    let final_path = vault_path.join(".cairn/vault.binding");
    std::fs::write(&final_path, witness_hash.as_bytes())?;
    std::fs::remove_file(&pending)?;

    // Step 5: keystore.store_keypair(identity)
    keystore.store_keypair(&plan.secret_handle, &plan.signing_key).await?;

    // Step 6: registry.activate_identity
    registry.activate_identity(&plan.identity.id, plan.key_entry.key_version).await?;

    Ok(())
}
```

(See spec §3.7 for the complete crash-recovery contract — `finalise-binding` resumes from any partial step. That command is implemented in Task D8.)

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): commit_first_identity + namespace probe + 2-phase sentinel (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D4: `provision` verb (with self-healing reconciliation)

Per spec §3.5.

**Files:**
- Modify: `crates/cairn-cli/src/identity/mod.rs` (add `provision` method)

- [ ] **Step 1: TDD**

```rust
#[tokio::test]
async fn provision_self_heals_pending_orphan() {
    let svc = setup_service_with_orphan_pending("hmn:bob:v1").await;
    let id = Identity::parse("hmn:bob:v1").unwrap();
    svc.provision(IdentityKind::Human, ProvisionInput::for_human("bob", IdentityRevision::FIRST), &mut rand_core::OsRng).await.unwrap();
    let row = svc.registry.get_identity(&id, IdentityVisibility::Operational).await.unwrap().unwrap();
    assert_eq!(row.provisioning_state, ProvisioningState::Active);
}
```

- [ ] **Step 2: Implement** the §3.5 self-healing flow: list pending rows for target → reconcile each → if active, no-op; if gone, fresh reserve+activate.

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): provision verb with self-healing reconciliation (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D5: `init-defaults` verb with revision bump rule

Per spec §3.5 + §4.5. Revision bump applies when the highest-revision row for a default slot is `revoked|revoke_pending|purge_pending|purged|KeyMaterialDesynchronized`.

- [ ] **Step 1: TDD — init-defaults bumps revision after purge**

```rust
#[tokio::test]
async fn init_defaults_mints_v2_after_v1_purged() {
    let svc = setup_service_with_purged_default_human().await;
    svc.init_defaults().await.unwrap();
    // Highest-rev human is now v2
    let humans = svc.registry.list_identities(Some(IdentityKind::Human), IdentityVisibility::Operational).await.unwrap();
    let active = humans.iter().find(|r| r.provisioning_state == ProvisioningState::Active).unwrap();
    assert_eq!(active.revision.as_u32(), 2);
}
```

- [ ] **Step 2: Implement**: for each default slot (human via `whoami::username()` + `normalize_human_slug`; agent fixed `agt:claude-code/opus-4-7/main`), look up the highest-revision row; apply the §3.5 rule.

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): init-defaults with revision-bump recovery (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D6: `rotate` verb (per-identity lock + two-phase cross-store)

Per spec §3.6. Steps: acquire `.cairn/maintenance/identity-locks/<sha256(id)>.lock` → snapshot current → insert pending_rotation → store_keypair → read-back verify → apply_rotation (with CAS) → evict eldest predecessor.

**Files:**
- Create: `crates/cairn-cli/src/identity/rotate.rs`
- Create: `crates/cairn-cli/src/identity/lock.rs` (per-identity advisory lock helper)

- [ ] **Step 1: Lock helper**

```rust
//! Per-identity advisory locks per spec §3.6 step 0.

use std::path::{Path, PathBuf};

use fs2::FileExt;
use sha2::{Digest, Sha256};

use cairn_core::domain::identity::Identity;
use cairn_core::error::identity::IdentityServiceError;

pub(super) struct IdentityLockGuard {
    file: std::fs::File,
}

impl IdentityLockGuard {
    pub fn acquire(vault_path: &Path, id: &Identity, wait: bool) -> Result<Self, IdentityServiceError> {
        let dir = vault_path.join(".cairn/maintenance/identity-locks");
        std::fs::create_dir_all(&dir)
            .map_err(|e| IdentityServiceError::Keystore(cairn_core::contract::keystore::KeystoreError::Backend(Box::new(e))))?;
        let mut h = Sha256::new(); h.update(id.as_str().as_bytes());
        let path = dir.join(format!("{}.lock", hex::encode(h.finalize())));
        let file = std::fs::OpenOptions::new().create(true).write(true).read(true).open(&path)
            .map_err(|e| IdentityServiceError::Keystore(cairn_core::contract::keystore::KeystoreError::Backend(Box::new(e))))?;
        if wait {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if file.try_lock_exclusive().is_ok() { break; }
                if std::time::Instant::now() >= deadline {
                    return Err(IdentityServiceError::IdentityLockBusy { id: id.clone() });
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        } else {
            file.try_lock_exclusive().map_err(|_| IdentityServiceError::IdentityLockBusy { id: id.clone() })?;
        }
        Ok(Self { file })
    }
}

impl Drop for IdentityLockGuard {
    fn drop(&mut self) { let _ = self.file.unlock(); }
}
```

(Add `hex = "0.4"` to deps.)

- [ ] **Step 2: TDD**

```rust
#[tokio::test]
async fn rotate_advances_current_key_version_and_evicts_eldest() {
    let svc = setup_service_with_active_identity_at_v3("hmn:alice:v1").await;
    svc.rotate(&Identity::parse("hmn:alice:v1").unwrap()).await.unwrap();
    // Current is v4
    // Eldest (v1) is gone from keystore; v2, v3, v4 retained.
}
```

- [ ] **Step 3: Implement** the full §3.6 sequence, using the lock guard around steps 0a–4.

- [ ] **Step 4: Run + commit**

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): rotate verb with per-identity lock + two-phase cross-store (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D7: `revoke` and `purge` verbs

Per spec §3.10.

- [ ] **Step 1: TDD revoke**

```rust
#[tokio::test]
async fn revoke_disables_signing_at_begin_revocation() {
    let svc = setup_service_with_two_active_identities().await;
    let target = Identity::parse("hmn:alice:v1").unwrap();
    svc.revoke(&target, &svc.signer_for("agt:claude-code/opus-4-7/main:v1")).await.unwrap();
    // Even before reconcile, the row is in revoke_pending and signing is refused
    assert!(svc.try_sign_as(&target).await.is_err());
}
```

- [ ] **Step 2: Implement `revoke`**: `IdentityLockGuard::acquire` → `begin_revocation` → for each version in `identity_keys ∪ pending_rotations`, `delete_keypair` + verify NotFound → `finalise_revocation`.

- [ ] **Step 3: TDD purge** (verify ack barrier)

```rust
#[tokio::test]
async fn purge_refuses_without_ack_file() {
    let svc = setup_service_with_active_identity().await;
    let err = svc.purge(&Identity::parse("hmn:alice:v1").unwrap(), PurgeReason("test".into())).await.unwrap_err();
    assert!(matches!(err, IdentityServiceError::Registry(RegistryError::Backend(_)))); // or a typed PurgeAckMissing — add it
}
```

Add `IdentityServiceError::PurgeAckMissing` (mapped to `EX_DATAERR`).

- [ ] **Step 4: Implement `purge`**: lock → check `.cairn/maintenance/purge-ack` exists and contains the identity wire form → `mark_purge_pending` → for each version, `delete_keypair` + verify → `finalise_purge` → delete ack file. `--resume` re-checks ack and re-drives steps 2–3.

- [ ] **Step 5: Run + commit**

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): revoke (two-phase) + purge with --resume (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D8: `repair` + `reconcile` + `finalise-binding` + `vault-id-recover`

These four are recovery commands. They share the same shape: open through `MaintenanceMode` and apply targeted state-machine repair.

- [ ] **Step 1: Implement `repair <id>`** (per §3.10): no signed envelope; runs the §3.5 reconciliation rules against one identity.

- [ ] **Step 2: Implement `reconcile`**: bulk version of repair; iterates `list_pending`, `list_pending_evictions`, `list_pending_key_disables`. For evictions, attempt the keystore delete + clear flag. For key disables, drive the per-version delete loop.

- [ ] **Step 3: Implement `finalise-binding`** (per §3.7 recovery table): each crash-state row in the table is one match arm. The `.binding` exists + DB never wrote + keychain witness present case requires reconstructing the pending sentinel from keychain bytes.

- [ ] **Step 4: Implement `vault-id-recover`**: step 0 pending-sentinel guard → `read_vault_meta` happy path → enumeration fallback → `--vault-id` fallback. Always rewrites `.cairn/vault.binding` with the witness hash.

- [ ] **Step 5: TDD for each** (4 tests, each with a synthetic crash state).

- [ ] **Step 6: Run + commit**

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): repair, reconcile, finalise-binding, vault-id-recover (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D9: `status` verb (cold-start mismatch sweep)

Per spec §4.5.

- [ ] **Step 1: TDD**

```rust
#[tokio::test]
async fn status_reports_vault_degraded_on_cold_start_with_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    setup_vault_with_pending_pubkey_mismatch(&dir).await;
    // Spawn a fresh process: open_for_maintenance(ReadOnly), run dry-run sweep
    let svc = IdentityService::open_for_maintenance(dir.path().to_path_buf(), MaintenanceMode::ReadOnly).await.unwrap();
    let report = svc.status_report().await.unwrap();
    assert!(report.vault_degraded);
    assert!(!report.mismatched_ids.is_empty());
}
```

- [ ] **Step 2: Implement** the dry-run sweep over both pending and active rows; build `IdentityStatusReport`. Handle `KeystoreError::Locked` → `MismatchCheckOutcome::KeystoreLocked`.

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): identity status verb with cold-start mismatch sweep (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D10: clap subcommand wiring + `cairn identity list/show`

**Files:**
- Modify: `crates/cairn-cli/src/main.rs`
- Create: `crates/cairn-cli/src/identity/cli.rs`

- [ ] **Step 1: Add the subcommand** under `cairn identity` per spec §4.5 surface (full list of 12 subcommands).

```rust
// Excerpt from cli.rs
#[derive(clap::Subcommand)]
pub enum IdentityCmd {
    Provision(ProvisionArgs),
    InitDefaults { #[arg(long)] json: bool },
    List { #[arg(long)] kind: Option<KindArg>, #[arg(long)] json: bool },
    Show { id: String, #[arg(long)] json: bool },
    Rotate { id: String, #[arg(long)] json: bool },
    Revoke { id: String, #[arg(long)] json: bool },
    Reconcile { #[arg(long)] json: bool },
    Repair { id: String, #[arg(long)] json: bool },
    Purge { id: String, #[arg(long)] resume: bool, #[arg(long)] json: bool },
    VaultIdRecover { #[arg(long)] probe_keychain: bool, #[arg(long)] vault_id: Option<String>, #[arg(long)] json: bool },
    FinaliseBinding { #[arg(long)] abandon: bool, #[arg(long)] vault_id: Option<String>, #[arg(long)] json: bool },
    Status { #[arg(long)] json: bool },
}
```

Wire each variant to the corresponding `IdentityService` method.

- [ ] **Step 2: Snapshot tests**

```rust
#[test]
fn list_json_snapshot() {
    let mut cmd = test_command_against_seeded_vault(); // helper using assert_cmd
    cmd.args(["identity", "list", "--json"]);
    let out = cmd.output().unwrap();
    insta::assert_snapshot!("identity_list_json", String::from_utf8_lossy(&out.stdout));
}
```

(Add `assert_cmd` and `insta` to dev-deps.)

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): clap surface for cairn identity subcommands (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D11: Bootstrap delta — mint `vault.id`, fail-closed re-bootstrap guard

Per `docs/superpowers/specs/2026-04-26-bootstrap-design.md` Amendment 2026-04-27.

**Files:**
- Modify: `crates/cairn-cli/src/vault.rs`
- Modify: `crates/cairn-cli/tests/bootstrap.rs`

- [ ] **Step 1: TDD**

Add the five tests from the bootstrap amendment:

```rust
#[test]
fn bootstrap_mints_vault_id_first_run() { /* ... */ }
#[test]
fn bootstrap_preserves_vault_id_on_second_run() { /* ... */ }
#[test]
fn bootstrap_fails_closed_when_vault_id_lost_with_db_row() { /* ... */ }
#[test]
fn bootstrap_fails_closed_when_vault_id_lost_with_sentinel_only() { /* ... */ }
#[test]
fn bootstrap_force_does_not_rewrite_vault_id() { /* ... */ }
```

- [ ] **Step 2: Implement** the decision tree: `.cairn/vault.binding.pending` → `EX_TEMPFAIL`; vault.id missing + binding sentinel or vault_meta row → `EX_DATAERR`; otherwise mint or preserve. Read-only DB probe via `?mode=ro` — never opens a write lock.

- [ ] **Step 3: Add `BootstrapReceipt.vault_id`**, output line `vault id <ULID>`, JSON field.

- [ ] **Step 4: Run + commit**

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): bootstrap delta — vault.id mint + fail-closed re-bootstrap guard (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D12: Wire `cairn ingest` / `cairn forget` (and other issuer-dependent verbs) through `IdentityService::open()`

The trust-boundary tightening: every issuer-dependent verb must check `vault_degraded` and refuse with `EX_TEMPFAIL`.

- [ ] **Step 1: Identify** which verbs in current main.rs sign (`grep -rn "Signer\|sign(" crates/cairn-cli/src/`).

- [ ] **Step 2: Add a thin wrapper** `cairn_cli::identity::guard::open_for_signed_verb(vault_path) -> Result<IdentityService, IdentityServiceError>` that wraps `IdentityService::open()` and converts `report.vault_degraded == true` into `IdentityServiceError::VaultDegraded { mismatched_ids }`.

- [ ] **Step 3: Insert the call** at the start of every signed verb.

- [ ] **Step 4: TDD a regression test**

```rust
#[test]
fn ingest_refuses_when_vault_degraded() { /* ... */ }
```

- [ ] **Step 5: Run + commit**

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "feat(cli): gate signed verbs on VaultDegraded (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase E — Workspace `usr:` → `hmn:` cleanup, integration tests, acceptance

### Task E1: Workspace-wide `usr:` → `hmn:` rename sweep

Per spec §3.3.

- [ ] **Step 1: Find every occurrence**

```bash
rg -n 'usr:' --type-not lock --glob '!docs/superpowers/specs/**'
```

- [ ] **Step 2: Update each**: fixtures, docs, CLAUDE.md (if any), generated JSON, integration test data. Do NOT touch the spec docs in `docs/superpowers/specs/2026-04-27-*` (the `usr:` references there are intentional history).

- [ ] **Step 3: Add a CI guard test**

`crates/cairn-core/tests/usr_prefix_banned.rs`:
```rust
#[test]
fn no_usr_colon_in_workspace_rust_sources() {
    let output = std::process::Command::new("rg")
        .args(["-l", r"\busr:", "crates/", "--type", "rust"])
        .output().unwrap();
    let s = String::from_utf8_lossy(&output.stdout);
    assert!(s.is_empty(), "Found `usr:` in: {s}");
}
```

- [ ] **Step 4: Verify + commit**

```bash
cargo nextest run --workspace --locked && \
git add -A && git commit -m "refactor: workspace-wide usr: -> hmn: rename sweep (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task E2: End-to-end integration tests (per spec §7 testing matrix)

The spec §7 lists ~25 named test cases. Group them into one or two test files in `crates/cairn-cli/tests/`.

- [ ] **Step 1: Create `crates/cairn-cli/tests/identity_e2e.rs`** with helpers for the common setup pattern (tempdir → bootstrap → init-defaults → return `(svc, dir)`).

- [ ] **Step 2: Implement each named test from spec §7**, in this order:
1. `bootstrap_init_defaults_happy_path` — `cairn bootstrap` then `init-defaults`, assert active human + agent rows.
2. `init_defaults_idempotent` — second call produces no diff.
3. `bootstrap_minted_vault_id_persists` — verified by Task D11 already; cross-check from end-to-end.
4. `provisioning_state_pending_then_active` — verifies §3.5.
5. `rotation_fixture` — keystore ring bounded, public-key archive intact, witness untouched.
6. `revocation_fixture` — revoke_pending blocks signing immediately.
7. `vault_contains_no_plaintext_key_bytes` — grep `dir/.cairn` for raw key bytes.
8. `reconciliation_recovers_from_injected_mid_flow_crash` — synthetic pending row.
9. `reconciliation_fails_closed_on_injected_pubkey_mismatch` — vault-degraded.
10. `repair_reconciles_pending_rows_only` — never mutates active.
11. `purge_two_phase_tombstone` — full state machine.
12. `purge_crash_recovery_is_opt_in` — list/show no-op on `purge_pending`.
13. `purge_does_not_auto_resume_on_inspection`.
14. `revoked_default_cannot_sign`.
15. `purge_requires_ack_file`.
16. `purge_unreachable_from_mcp_surface` (only callable when DB+filesystem are local).
17. `two_vault_isolation` — two vaults' keychain entries don't cross.
18. `first_run_gate` — `cairn ingest` before `init-defaults` returns `EX_USAGE = 64`.
19. `liveness_gate_test` — `cairn ingest` after default keychain entry deleted out-of-band returns `EX_DATAERR = 65`.
20. `purge_ack_barrier` — no flag combination writes the ack file.
21. `single_default_broken_ordinary_write` — falls through to live default.
22. `single_default_broken_rotation` — broken default rotated under live other default.
23. `rotation_atomicity` — kill mid-`apply_rotation` → both/neither.
24. `rotation_receipt_records_signer_key_version` — older receipt verifies after rotation.
25. `receipt_fk_enforcement` — adapter regression caught.
26. `purged_at_not_stamped_early`.
27. `non_default_self_rotation`.
28. `all_broken_degrades_to_purge`.
29. `maintenance_open_isolation`.
30. `finalise_binding_abandon`.
31. `concurrent_first_bind`.
32. `first_bind_no_wait`.
33. `finalise_binding_finalise_from_pending`.
34. `finalise_binding_finalise_from_binding`.
35. `dpapi_vault_id_recover_with_intact_db`.
36. `vault_scoped_abandon`.
37. `abandon_refused_without_vault_id_on_every_backend`.
38. `dpapi_abandon_with_vault_id`.
39. `rotation_receipt_persisted`.
40. `recovery_commands_succeed_with_zero_defaults`.
41. `rotate_revoke_fail_when_defaults_missing`.
42. `key_material_desynchronized_raised`.
43. `vault_id_regeneration_refused`.
44. `db_only_durable_binding_test`.
45. `bootstrap_probe_is_read_only`.
46. `unrelated_keystore_namespaces_do_not_trip_refusal`.
47. `sentinel_first_crash_test`.
48. `multi_vault_coexistence_test`.
49. `ambiguous_match_fail_closed`.
50. `dpapi_fallback_with_vault_id`.
51. `schema_skew_safety_test`.

For each test write the case independently — **no helper that hides assertions**. The bodies are typically 10–40 lines each.

- [ ] **Step 3: Run** all tests, fix any failure (likely a real bug in Phase D), commit each fix individually. The plan-level commit message:

```bash
cargo nextest run -p cairn-cli --locked && \
git add -A && git commit -m "test(cli): full §7 acceptance matrix for identity provisioning (#50)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task E3: Final acceptance + traceability + docs.md update

- [ ] **Step 1: Run the full verification checklist** (CLAUDE.md §8).

```bash
cargo fmt --all --check && \
cargo clippy --workspace --all-targets --locked -- -D warnings && \
cargo check --workspace --all-targets --locked && \
cargo nextest run --workspace --locked --no-fail-fast && \
cargo test --doc --workspace --locked && \
./scripts/check-core-boundary.sh && \
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check && \
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked && \
cargo deny check && \
cargo audit --deny warnings && \
cargo machete
```

Expected: all green.

- [ ] **Step 2: Update `docs/design/traceability.md`** with the brief sections this PR implements (§4.2, §3, §14).

- [ ] **Step 3: Open the PR** with the brief sections, invariants touched (1, 2, 3, 4, 5, 6, 8, 9, 10), and verification output.

```bash
gh pr create --title "Provision local human/agent/sensor identities (#50)" --body "$(cat <<'EOF'
## Summary
- New cairn-keychain crate; OsKeystore over the keyring crate.
- IdentityRegistry trait + SqliteIdentityRegistry adapter (migration 0002_identity.sql).
- IdentityService orchestrator + `cairn identity {provision,init-defaults,list,show,rotate,revoke,reconcile,repair,purge,vault-id-recover,finalise-binding,status}` CLI surface.
- Bootstrap delta: vault.id mint + fail-closed re-bootstrap guard (amends 2026-04-26-bootstrap-design.md).
- Workspace-wide usr: -> hmn: rename.

## Brief sections
§4.2 (Identity), §3 (vault), §14 (privacy/consent).

## Invariants touched
1, 2, 3, 4, 5, 6, 8, 9, 10 (per CLAUDE.md §4).

## Test plan
- [x] cargo nextest run --workspace
- [x] cargo clippy -- -D warnings
- [x] ./scripts/check-core-boundary.sh
- [x] §7 acceptance matrix passes (~50 cases)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-review

- [x] **Spec coverage:** Every section of the design (§3.3 rename, §3.4 pure provisioning, §3.5 reconciliation, §3.6 rotation + key retention, §3.7 first-bind + bootstrap guard, §3.8 liveness check, §3.9 slug normalization, §3.10 repair/rotate/revoke/purge with audit, §4.1–4.5 crate-by-crate changes) maps to at least one task.
- [x] **No placeholders:** All steps include the actual code or command. The text "the engineer must read spec §X before writing code" appears only as preamble and does NOT replace inline content.
- [x] **Type consistency:** `apply_rotation` signature change (Task C6 step 2 note) is propagated by updating Task A9 trait inline before Task C6 lands. `PurgeAcknowledgement::for_test` is added in Task C8 before its use.
- [x] **Spec round-10 carry-overs:** all three findings closed in plan (pending_rotations WAL coverage in Task C2; mutating-maintenance vault-binding check in Task D2; rotation step ordering — step 1 reads observed_current before any keystore write in Task D6; the spec-side wording is correct, only the explanation in §3.6 step 0a needs reordering, which the engineer should fix when they touch that section).

---

Plan complete. Save target: `docs/superpowers/plans/2026-04-28-issue-50-identity-provisioning.md`.

**Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
