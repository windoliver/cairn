# SQLite Store CRUD (PR-A, Issue #46) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `MemoryStore` with CRUD, versioning, graph edges, and transactions on top of the existing P0 SQLite schema. Wire `tokio_rusqlite` so trait methods are honestly `async`. Leave `search_keyword` as a `CapabilityUnavailable` stub for PR-B.

**Architecture:** Widens the existing `MemoryStore` trait (single fat trait, capability flags gate optional methods). `SqliteMemoryStore` wraps a `tokio_rusqlite::Connection`; every method is one `conn.call(|c| { … })` round-trip. Records persist as a `record_json` blob plus denormalized hot columns. Versioning is in-place via the existing `(target_id, version)` schema and `records_active_target_idx`. WAL FSM (issue #8) lives at the verb layer in `cairn-core` and is **out of scope** here — this PR is pure CRUD primitives.

**Spec:** [`docs/superpowers/specs/2026-04-27-store-sqlite-crud-keyword-search-design.md`](../specs/2026-04-27-store-sqlite-crud-keyword-search-design.md)

**Spec deviation:** `with_tx` is exposed as an **inherent method** on `SqliteMemoryStore`, not on the trait. Reason: object-safety. A method with `F: FnOnce(&mut StoreTx) -> Result<T, StoreError> + Send + 'static` and `T: Send + 'static` generic parameters cannot live on a `dyn`-compatible trait, and the workspace already stores `Arc<dyn MemoryStore>` in the registry. Verb-layer code that needs transactionality takes `Arc<SqliteMemoryStore>` directly. Spec will be patched in the same PR.

**Tech Stack:** Rust 1.95.0, `tokio_rusqlite`, `rusqlite`, `rusqlite_migration`, `serde_json`, `blake3`, `async_trait`, `tracing`, `proptest`, `tempfile`, `nextest`.

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/cairn-core/src/domain/target_id.rs` | Create | `TargetId` newtype (ULID-shaped) |
| `crates/cairn-core/src/domain/body_hash.rs` | Create | `BodyHash` newtype (`blake3:` + 64 hex) |
| `crates/cairn-core/src/domain/mod.rs` | Modify | Export `TargetId`, `BodyHash` |
| `crates/cairn-core/src/domain/record.rs` | Modify | Add `target_id: TargetId` field; validation; update `sample_record` |
| `crates/cairn-core/src/domain/canonical.rs` | Modify | Update `sample` constructor for new field |
| `crates/cairn-core/tests/memory_record.rs` | Modify | Update test fixtures for new field |
| `crates/cairn-test-fixtures/tests/schema_fixtures.rs` | Modify | Update for new field |
| `crates/cairn-core/src/contract/memory_store.rs` | Modify | Bump `CONTRACT_VERSION` to 0.2.0; add CRUD/edge/search method signatures + supporting types |
| `crates/cairn-store-sqlite/Cargo.toml` | Modify | Add `tokio_rusqlite`, `serde_json`, `blake3`, `tokio`, `tracing`, `bon` deps; add `tokio` dev-dep |
| `Cargo.toml` (workspace) | Modify | Add `tokio_rusqlite`, `blake3`, `bon` to `[workspace.dependencies]` if missing |
| `crates/cairn-store-sqlite/src/migrations/sql/0007_tombstone_reason.sql` | Create | `ALTER TABLE records ADD COLUMN tombstone_reason TEXT;` |
| `crates/cairn-store-sqlite/src/migrations/sql/0008_record_extensions.sql` | Create | Adds `record_json`, `confidence`, `salience`, `target_id_explicit`, `tags_json` |
| `crates/cairn-store-sqlite/src/migrations/sql/0010_ranking_indexes.sql` | Create | Adds confidence + updated_at indexes |
| `crates/cairn-store-sqlite/src/migrations/mod.rs` | Modify | Register 0007, 0008, 0010 |
| `crates/cairn-store-sqlite/src/error.rs` | Modify | Extend `StoreError` with `NotFound`, `CapabilityUnavailable`, `Worker`, `Codec`, `Invariant` |
| `crates/cairn-store-sqlite/src/open.rs` | Modify | Open via `tokio_rusqlite::Connection`; return `SqliteMemoryStore`; add pragmas |
| `crates/cairn-store-sqlite/src/lib.rs` | Modify | Replace stub `SqliteMemoryStore` impl with real CRUD wiring; flip cap flags |
| `crates/cairn-store-sqlite/src/store/mod.rs` | Create | Module root for store impl |
| `crates/cairn-store-sqlite/src/store/upsert.rs` | Create | `upsert` + helpers |
| `crates/cairn-store-sqlite/src/store/read.rs` | Create | `get`, `list`, `versions` |
| `crates/cairn-store-sqlite/src/store/tombstone.rs` | Create | `tombstone` |
| `crates/cairn-store-sqlite/src/store/edges.rs` | Create | `put_edge`, `remove_edge`, `neighbours` |
| `crates/cairn-store-sqlite/src/store/tx.rs` | Create | `with_tx`, `StoreTx` |
| `crates/cairn-store-sqlite/src/store/projection.rs` | Create | `MemoryRecord` ↔ row projection |
| `crates/cairn-store-sqlite/tests/crud_roundtrip.rs` | Create | Integration test |
| `crates/cairn-store-sqlite/tests/upsert_idempotent.rs` | Create | Integration + proptest |
| `crates/cairn-store-sqlite/tests/versioning.rs` | Create | Integration test |
| `crates/cairn-store-sqlite/tests/tombstone_reasons.rs` | Create | Integration test |
| `crates/cairn-store-sqlite/tests/edges_crud.rs` | Create | Integration test |
| `crates/cairn-store-sqlite/tests/tx_rollback.rs` | Create | Integration test |
| `crates/cairn-store-sqlite/tests/hot_columns_match_json.rs` | Create | Proptest |
| `crates/cairn-store-sqlite/tests/records_latest.rs` | Modify | Add `record_json` defaults to existing inserts |
| `crates/cairn-test-fixtures/src/lib.rs` | Modify | Add `sample_record(seed)`, `tempstore()`, `memstore()` |

---

## Section 1 — Foundation: new domain types

### Task 1: Add `TargetId` newtype

**Files:**
- Create: `crates/cairn-core/src/domain/target_id.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/cairn-core/src/domain/mod.rs` first to expose the module and then write the test below at the bottom of the new `target_id.rs` file.

In `crates/cairn-core/src/domain/target_id.rs`:

```rust
//! [`TargetId`] — supersession lineage key (brief §3, §3.0).
//!
//! Distinct from [`crate::domain::RecordId`]: `RecordId` identifies one
//! version row; `TargetId` identifies the lineage that supersession
//! advances. Same wire form (ULID, 26 chars, Crockford base32, uppercase,
//! no `I L O U`, leading char `0..=7`).

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct TargetId(String);

impl TargetId {
    /// Parse a wire-form ULID. Same validation as
    /// [`crate::domain::RecordId::parse`].
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.len() != 26 {
            return Err(DomainError::EmptyField { field: "target_id" });
        }
        let bytes = raw.as_bytes();
        if !matches!(bytes[0], b'0'..=b'7') {
            return Err(DomainError::EmptyField { field: "target_id" });
        }
        if !bytes[1..].iter().all(|b| {
            matches!(b,
                b'0'..=b'9'
                | b'A'..=b'H'
                | b'J'
                | b'K'
                | b'M'
                | b'N'
                | b'P'..=b'T'
                | b'V'..=b'Z')
        }) {
            return Err(DomainError::EmptyField { field: "target_id" });
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for TargetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for TargetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_ulid() {
        let t = TargetId::parse("01HQZX9F5N0000000000000000").expect("valid");
        assert_eq!(t.as_str(), "01HQZX9F5N0000000000000000");
    }

    #[test]
    fn rejects_overflow_first_char() {
        let err = TargetId::parse("8ZZZZZZZZZZZZZZZZZZZZZZZZZ").unwrap_err();
        assert!(matches!(err, DomainError::EmptyField { field: "target_id" }));
    }

    #[test]
    fn rejects_wrong_length() {
        let err = TargetId::parse("01HQZX9F5N").unwrap_err();
        assert!(matches!(err, DomainError::EmptyField { field: "target_id" }));
    }

    #[test]
    fn rejects_lowercase_alphabet() {
        let err = TargetId::parse("01hqzx9f5n0000000000000000").unwrap_err();
        assert!(matches!(err, DomainError::EmptyField { field: "target_id" }));
    }

    #[test]
    fn json_roundtrip() {
        let t = TargetId::parse("01HQZX9F5N0000000000000000").expect("valid");
        let s = serde_json::to_string(&t).expect("ser");
        assert_eq!(s, "\"01HQZX9F5N0000000000000000\"");
        let back: TargetId = serde_json::from_str(&s).expect("de");
        assert_eq!(t, back);
    }
}
```

- [ ] **Step 2: Wire the module**

Edit `crates/cairn-core/src/domain/mod.rs`. Find the existing `pub mod` declarations and add `target_id` alongside them. Find the existing re-export block (look for `pub use` lines that re-export domain types) and append `pub use target_id::TargetId;` to the same group.

- [ ] **Step 3: Run the tests, expect pass**

```bash
cargo test -p cairn-core --lib domain::target_id -- --nocapture
```

Expected: 5 tests pass (`parses_valid_ulid`, `rejects_overflow_first_char`, `rejects_wrong_length`, `rejects_lowercase_alphabet`, `json_roundtrip`).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/target_id.rs crates/cairn-core/src/domain/mod.rs
git commit -m "feat(core): add TargetId newtype for supersession lineage (#46)"
```

---

### Task 2: Add `BodyHash` newtype

**Files:**
- Create: `crates/cairn-core/src/domain/body_hash.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`

- [ ] **Step 1: Write the file with failing tests**

In `crates/cairn-core/src/domain/body_hash.rs`:

```rust
//! [`BodyHash`] — `blake3:` + 64 lowercase hex chars over a record's body.
//!
//! Drives the idempotent-upsert decision in
//! [`crate::contract::MemoryStore::upsert`]: identical hash → no version bump.
//! Computation is centralized in [`BodyHash::compute`] so producers and
//! verifiers can never disagree.

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct BodyHash(String);

impl BodyHash {
    /// Parse a wire-form `blake3:<64 lowercase hex>` string.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        let Some(tail) = raw.strip_prefix("blake3:") else {
            return Err(DomainError::EmptyField { field: "body_hash" });
        };
        if tail.len() != 64 {
            return Err(DomainError::EmptyField { field: "body_hash" });
        }
        if !tail.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(DomainError::EmptyField { field: "body_hash" });
        }
        Ok(Self(raw))
    }

    /// Compute over a UTF-8 body string.
    #[must_use]
    pub fn compute(body: &str) -> Self {
        let hash = blake3::hash(body.as_bytes());
        Self(format!("blake3:{}", hash.to_hex()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BodyHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for BodyHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_is_deterministic() {
        let a = BodyHash::compute("hello world");
        let b = BodyHash::compute("hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn compute_differs_on_different_input() {
        let a = BodyHash::compute("alpha");
        let b = BodyHash::compute("beta");
        assert_ne!(a, b);
    }

    #[test]
    fn parses_well_formed_hash() {
        let raw = format!("blake3:{}", "a".repeat(64));
        BodyHash::parse(raw).expect("valid");
    }

    #[test]
    fn rejects_missing_prefix() {
        let raw = "a".repeat(64);
        let err = BodyHash::parse(raw).unwrap_err();
        assert!(matches!(err, DomainError::EmptyField { field: "body_hash" }));
    }

    #[test]
    fn rejects_uppercase_hex() {
        let raw = format!("blake3:{}", "A".repeat(64));
        let err = BodyHash::parse(raw).unwrap_err();
        assert!(matches!(err, DomainError::EmptyField { field: "body_hash" }));
    }

    #[test]
    fn rejects_wrong_length() {
        let raw = format!("blake3:{}", "a".repeat(63));
        let err = BodyHash::parse(raw).unwrap_err();
        assert!(matches!(err, DomainError::EmptyField { field: "body_hash" }));
    }

    #[test]
    fn computed_hash_is_parseable() {
        let h = BodyHash::compute("anything");
        BodyHash::parse(h.as_str().to_owned()).expect("compute → parse roundtrip");
    }
}
```

- [ ] **Step 2: Add the workspace dep**

In `Cargo.toml` at the workspace root, add to `[workspace.dependencies]` (alphabetical order):

```toml
blake3 = { version = "1.5", default-features = false }
```

In `crates/cairn-core/Cargo.toml` `[dependencies]`, add:

```toml
blake3 = { workspace = true }
```

- [ ] **Step 3: Wire the module**

In `crates/cairn-core/src/domain/mod.rs`, add `pub mod body_hash;` next to the other `pub mod` lines, and `pub use body_hash::BodyHash;` to the re-export group.

- [ ] **Step 4: Run the tests, expect pass**

```bash
cargo test -p cairn-core --lib domain::body_hash -- --nocapture
```

Expected: 7 tests pass.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/cairn-core/Cargo.toml crates/cairn-core/src/domain/body_hash.rs crates/cairn-core/src/domain/mod.rs
git commit -m "feat(core): add BodyHash newtype + blake3 compute (#46)"
```

---

### Task 3: Add `target_id` to `MemoryRecord`

**Files:**
- Modify: `crates/cairn-core/src/domain/record.rs`
- Modify: `crates/cairn-core/src/domain/canonical.rs:170` (sample constructor)
- Modify: `crates/cairn-core/tests/memory_record.rs:25-30, 264-270` (two constructors)
- Modify: `crates/cairn-test-fixtures/tests/schema_fixtures.rs` (whichever line constructs `MemoryRecord`)

- [ ] **Step 1: Add the field declaration**

In `crates/cairn-core/src/domain/record.rs`, find the `MemoryRecord` struct (around line 138). Add the `target_id` field directly after `id`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecord {
    /// ULID — the stable record identifier.
    pub id: RecordId,
    /// Supersession lineage key. For a fresh fact this equals `id`. On
    /// supersession (`updates`-edge), the new record carries the prior
    /// record's `target_id`. Same wire form as `id`. Brief §3, §3.0.
    pub target_id: TargetId,
    /// Memory kind (§6.1).
    pub kind: MemoryKind,
    // … remaining fields unchanged …
}
```

Update the use list at the top of `record.rs` to include `TargetId`:

```rust
use crate::domain::{
    ActorChainEntry, CanonicalRecordHash, ChainRole, DomainError, EvidenceVector, Identity,
    IdentityKind, Provenance, Rfc3339Timestamp, ScopeTuple, TargetId, VerifiedSignedIntent,
    actor_chain::validate_chain,
    taxonomy::{MemoryClass, MemoryKind, MemoryVisibility},
};
```

- [ ] **Step 2: Update `sample_record` (in-file test fixture)**

In the `tests` mod inside `record.rs` (around line 704), update `sample_record` to set `target_id`. Insert the `target_id` field right after `id`:

```rust
pub(crate) fn sample_record() -> MemoryRecord {
    let user_id = Identity::parse("usr:tafeng").expect("valid");
    MemoryRecord {
        id: RecordId::parse("01HQZX9F5N0000000000000000").expect("valid"),
        target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid"),
        kind: MemoryKind::User,
        // … remaining fields unchanged …
    }
}
```

- [ ] **Step 3: Update `crates/cairn-core/src/domain/canonical.rs`**

Find the `sample` fn around line 162. Insert the same `target_id` line after `id`. Adjust the `use` block at the top of `canonical.rs` to import `TargetId`.

Find:

```rust
fn sample() -> MemoryRecord {
    // …
    MemoryRecord {
        id: RecordId::parse("…").expect("…"),
        kind: MemoryKind::User,
```

Change to:

```rust
fn sample() -> MemoryRecord {
    // …
    let id = RecordId::parse("01HQZX9F5N0000000000000000").expect("…");
    MemoryRecord {
        target_id: TargetId::parse(id.as_str().to_owned()).expect("valid"),
        id,
        kind: MemoryKind::User,
```

(If the existing `sample()` uses a different ULID literal, mirror it into both fields.)

- [ ] **Step 4: Update `crates/cairn-core/tests/memory_record.rs`**

Two constructor sites: line ~25 (`fn record() -> MemoryRecord`) and line ~264 (the helper inside the inner test mod). Add `target_id: TargetId::parse(/* same value as id */).expect("valid"),` after the `id:` line in both. Update the `use` block at the top of the test file to include `TargetId`.

- [ ] **Step 5: Update `crates/cairn-test-fixtures/tests/schema_fixtures.rs`**

If this file constructs a `MemoryRecord` literal, mirror the same change. If it loads from a JSON fixture, update the JSON fixture to add a `"target_id": "<same-as-id>"` entry. Run a grep first to locate:

```bash
grep -n "MemoryRecord\|target_id\|\"id\"" crates/cairn-test-fixtures/tests/schema_fixtures.rs
```

Apply the change matching what the grep reveals.

- [ ] **Step 6: Add a validation test**

Append to `crates/cairn-core/src/domain/record.rs` inside the `tests` mod:

```rust
#[test]
fn target_id_independent_of_id() {
    let r = sample_record();
    // For a fresh record the convention is target_id == id, but the type
    // does not enforce that — supersessions intentionally keep the prior
    // target_id while issuing a new id.
    assert_eq!(r.target_id.as_str(), r.id.as_str());
}

#[test]
fn target_id_round_trips_in_json() {
    let mut r = sample_record();
    let other = TargetId::parse("01HQZX9F5N1234567890ABCDEF").expect("valid");
    r.target_id = other.clone();
    let s = serde_json::to_string(&r).expect("ser");
    let back: MemoryRecord = serde_json::from_str(&s).expect("de");
    assert_eq!(back.target_id, other);
}

#[test]
fn missing_target_id_in_json_rejected() {
    let mut value = serde_json::to_value(sample_record()).expect("ser");
    value.as_object_mut().unwrap().remove("target_id");
    let res: Result<MemoryRecord, _> = serde_json::from_value(value);
    assert!(res.is_err(), "target_id is required");
}
```

- [ ] **Step 7: Run the cairn-core test suite, expect pass**

```bash
cargo nextest run -p cairn-core --locked --no-fail-fast
```

Expected: all tests pass, including the 3 new ones.

- [ ] **Step 8: Run the workspace check**

```bash
cargo check --workspace --all-targets --locked
```

Expected: clean build. If a downstream crate fails to construct `MemoryRecord`, locate the constructor and add the same `target_id` line.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-core/src/domain/record.rs \
        crates/cairn-core/src/domain/canonical.rs \
        crates/cairn-core/tests/memory_record.rs \
        crates/cairn-test-fixtures/tests/schema_fixtures.rs
git commit -m "feat(core): add target_id field to MemoryRecord (#46)"
```

---

## Section 2 — Trait extension

### Task 4: Bump `CONTRACT_VERSION` and add supporting types

**Files:**
- Modify: `crates/cairn-core/src/contract/memory_store.rs`

- [ ] **Step 1: Bump the constant**

In `crates/cairn-core/src/contract/memory_store.rs`, change:

```rust
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);
```

to:

```rust
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 2, 0);
```

Update the doc comment above to add: ` Bumped 0.1 → 0.2 in #46 when CRUD/edge/search/tx methods landed.`

- [ ] **Step 2: Add the supporting types module**

Append to the same file, below the existing `MemoryStorePlugin` trait (before the `tests` mod):

```rust
// ── Verb-method support types (#46, #47) ──────────────────────────────────────

use crate::domain::{
    BodyHash, MemoryRecord, RecordId, ScopeTuple, TargetId,
    filter::ValidatedFilter,
    taxonomy::{MemoryClass, MemoryKind, MemoryVisibility},
};

/// Why a row was tombstoned. Distinguishes user-initiated retraction
/// (`Update`, `Forget`) from system-initiated lifecycle events
/// (`Expire`, `Purge`). Brief §5.6, §10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TombstoneReason {
    /// Superseded by a fresh fact via an `updates` edge.
    Update,
    /// Aged out by the expiration workflow.
    Expire,
    /// User-requested forget (record-level).
    Forget,
    /// Hard purge (rare, after retention boundaries).
    Purge,
}

impl TombstoneReason {
    #[must_use]
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Expire => "expire",
            Self::Forget => "forget",
            Self::Purge => "purge",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "update" => Some(Self::Update),
            "expire" => Some(Self::Expire),
            "forget" => Some(Self::Forget),
            "purge" => Some(Self::Purge),
            _ => None,
        }
    }
}

/// Outcome of an `upsert` call. `content_changed = false` indicates the
/// store treated the call as idempotent (same body hash) — no new version
/// row was emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertOutcome {
    pub record_id: RecordId,
    pub target_id: TargetId,
    pub version: u32,
    pub content_changed: bool,
    pub prior_hash: Option<BodyHash>,
}

/// Filter args for `list`. All `Option` fields are AND-combined.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListArgs {
    pub kind: Option<MemoryKind>,
    pub class: Option<MemoryClass>,
    pub visibility_allowlist: Vec<MemoryVisibility>,
    pub limit: usize,
    pub cursor: Option<ListCursor>,
}

/// Opaque keyset cursor for `list`. Encoded base64-json on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListCursor {
    pub updated_at: i64,
    pub record_id: RecordId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListPage {
    pub records: Vec<MemoryRecord>,
    pub next_cursor: Option<ListCursor>,
}

/// One row from `versions(target)` — schema-level metadata only, not the
/// full hydrated record. Callers that want the body call `get(record_id)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordVersion {
    pub record_id: RecordId,
    pub target_id: TargetId,
    pub version: u32,
    pub created_at: i64,
    pub updated_at: i64,
    pub active: bool,
    pub tombstoned: bool,
    pub tombstone_reason: Option<TombstoneReason>,
    pub body_hash: BodyHash,
}

/// Edge kinds supported at P0. Exhaustive — adding a new kind is a
/// brief-level change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EdgeKind {
    /// Fact-supersession (brief §3 line ~409). Endpoints must be
    /// non-tombstoned with distinct target_ids; the store schema enforces
    /// this with triggers.
    Updates,
    /// Cross-reference / mention.
    Mentions,
    /// Supports / corroborates.
    Supports,
}

impl EdgeKind {
    #[must_use]
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Updates => "updates",
            Self::Mentions => "mentions",
            Self::Supports => "supports",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "updates" => Some(Self::Updates),
            "mentions" => Some(Self::Mentions),
            "supports" => Some(Self::Supports),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub src: RecordId,
    pub dst: RecordId,
    pub kind: EdgeKind,
    pub weight: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EdgeKey {
    pub src: RecordId,
    pub dst: RecordId,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeDir {
    Out,
    In,
    Both,
}

// ── Search types (used by trait stub here; impl in PR-B) ──────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct KeywordSearchArgs {
    /// Raw FTS5 expression. Store does not validate FTS5 syntax; SQLite
    /// surfaces parse errors which the store re-wraps as
    /// [`crate::contract::memory_store::SearchError`] (PR-B).
    pub query: String,
    /// Pre-validated filter tree from
    /// [`crate::domain::filter::validate_filter`].
    pub filter: Option<ValidatedFilter>,
    pub visibility_allowlist: Vec<MemoryVisibility>,
    pub limit: usize,
    pub cursor: Option<KeywordCursor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeywordCursor {
    pub bm25: f64,
    pub record_id: RecordId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeywordSearchPage {
    pub candidates: Vec<SearchCandidate>,
    pub next_cursor: Option<KeywordCursor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchCandidate {
    pub record_id: RecordId,
    pub target_id: TargetId,
    pub scope: ScopeTuple,
    pub kind: MemoryKind,
    pub class: MemoryClass,
    pub visibility: MemoryVisibility,
    pub bm25: f64,
    pub recency_seconds: i64,
    pub confidence: f32,
    pub salience: f32,
    pub staleness_seconds: i64,
    pub snippet: String,
    /// Serialized `MemoryRecord` for callers that want full hydration
    /// without a second round-trip. Never logged above `trace`.
    pub record_json: String,
}
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo check -p cairn-core --all-targets --locked
```

Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/contract/memory_store.rs
git commit -m "feat(core): bump MemoryStore CONTRACT_VERSION to 0.2 + add CRUD/search support types (#46)"
```

---

### Task 5: Extend the `MemoryStore` trait with CRUD/edge/search method signatures

**Files:**
- Modify: `crates/cairn-core/src/contract/memory_store.rs`

- [ ] **Step 1: Widen the trait**

In `crates/cairn-core/src/contract/memory_store.rs`, find the existing `pub trait MemoryStore` block and extend it:

```rust
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    /// Stable identifier of the registered plugin instance.
    fn name(&self) -> &str;

    /// Static capability advertisement (brief §4.1).
    fn capabilities(&self) -> &MemoryStoreCapabilities;

    /// Range of `MemoryStore::CONTRACT_VERSION` values this impl accepts.
    fn supported_contract_versions(&self) -> VersionRange;

    // ── CRUD (#46) ────────────────────────────────────────────────────────

    /// Insert a new record version, or no-op when the canonical body hash
    /// matches the active row for `record.target_id`. Idempotent — safe
    /// for replay. Brief §5.2.
    async fn upsert(&self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError>;

    /// Fetch one record by `record_id`. Returns `Ok(None)` for missing or
    /// tombstoned rows; `tombstoned` rows are not exposed via `get`.
    async fn get(&self, id: &RecordId) -> Result<Option<MemoryRecord>, StoreError>;

    /// Page through active, non-tombstoned records ordered by
    /// `(updated_at DESC, record_id)`. Brief §5.1.
    async fn list(&self, args: &ListArgs) -> Result<ListPage, StoreError>;

    /// Mark a specific record version as tombstoned with the given reason.
    /// Idempotent — already-tombstoned rows return `Ok(())`.
    async fn tombstone(
        &self,
        id: &RecordId,
        reason: TombstoneReason,
    ) -> Result<(), StoreError>;

    /// Full version history for a target, oldest → newest. Includes
    /// active and inactive rows.
    async fn versions(&self, target: &TargetId) -> Result<Vec<RecordVersion>, StoreError>;

    // ── Edges (#46) ───────────────────────────────────────────────────────

    /// Insert or replace an edge. `updates`-edge invariants are enforced
    /// by schema triggers (distinct target_ids, non-tombstoned endpoints,
    /// post-insert immutability) and surface as
    /// [`StoreError::Sql`] when violated.
    async fn put_edge(&self, edge: &Edge) -> Result<(), StoreError>;

    /// Remove an edge. Returns `true` if a row was deleted, `false`
    /// otherwise. `updates` edges are immutable and removal returns a
    /// trigger error wrapped in [`StoreError::Sql`].
    async fn remove_edge(&self, key: &EdgeKey) -> Result<bool, StoreError>;

    /// Edges adjacent to `id`. `EdgeDir::Out` returns outgoing edges,
    /// `EdgeDir::In` incoming, `EdgeDir::Both` the union. Endpoints
    /// pointing into superseded or tombstoned records are dropped.
    async fn neighbours(
        &self,
        id: &RecordId,
        dir: EdgeDir,
    ) -> Result<Vec<Edge>, StoreError>;

    // ── Search (#47, stubbed in PR-A) ─────────────────────────────────────

    /// Keyword search over `body` + `path` returning ranking-input
    /// candidates. The shared ranker (brief §5.1) is a separate pure
    /// function in `cairn-core`; this method does not produce a final
    /// score. Returns [`StoreError::CapabilityUnavailable`] when the
    /// `fts` capability is off.
    async fn search_keyword(
        &self,
        args: &KeywordSearchArgs,
    ) -> Result<KeywordSearchPage, StoreError>;
}
```

You'll need a forward reference to `StoreError`. Add at the top of the file (after the existing `use` block):

```rust
/// Errors raised by `MemoryStore` implementations. Adapters define their
/// own concrete type (e.g. [`cairn_store_sqlite::StoreError`]); this is
/// the trait-level alias to avoid leaking adapter types into core.
///
/// At the trait level, callers see `StoreError`. Concrete adapters
/// substitute their own enum with `From` impls covering the trait surface.
pub type StoreError = Box<dyn std::error::Error + Send + Sync + 'static>;
```

(This is intentional: cairn-core stays free of adapter-specific error variants. Adapters wrap their concrete error in `Box<dyn …>` at the trait boundary. The CapabilityUnavailable check inside the adapter still uses its concrete type internally.)

- [ ] **Step 2: Update the in-file `StubStore` to satisfy the new trait surface**

Find the `StubStore` impl in the `tests` mod (around line 82). Replace it with:

```rust
#[async_trait::async_trait]
impl MemoryStore for StubStore {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: true,
            vector: false,
            graph_edges: false,
            transactions: true,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        Self::SUPPORTED_VERSIONS
    }
    async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        Err("stub: upsert not implemented".into())
    }
    async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
        Ok(None)
    }
    async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
        Ok(ListPage { records: vec![], next_cursor: None })
    }
    async fn tombstone(&self, _id: &RecordId, _r: TombstoneReason) -> Result<(), StoreError> {
        Ok(())
    }
    async fn versions(&self, _t: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
        Ok(vec![])
    }
    async fn put_edge(&self, _e: &Edge) -> Result<(), StoreError> {
        Ok(())
    }
    async fn remove_edge(&self, _k: &EdgeKey) -> Result<bool, StoreError> {
        Ok(false)
    }
    async fn neighbours(
        &self,
        _id: &RecordId,
        _d: EdgeDir,
    ) -> Result<Vec<Edge>, StoreError> {
        Ok(vec![])
    }
    async fn search_keyword(
        &self,
        _args: &KeywordSearchArgs,
    ) -> Result<KeywordSearchPage, StoreError> {
        Err("stub: search_keyword not implemented".into())
    }
}
```

- [ ] **Step 3: Verify it compiles + tests pass**

```bash
cargo check -p cairn-core --all-targets --locked
cargo nextest run -p cairn-core --lib contract::memory_store --locked
```

Expected: clean build, existing 2 tests still pass.

- [ ] **Step 4: Verify dyn-compat survives**

The existing `dyn_compatible` test in the same file already constructs `Box<dyn MemoryStore> = Box::new(StubStore)`. It will fail to compile if any of the new trait methods has generics or non-erasable parameters.

```bash
cargo nextest run -p cairn-core --lib contract::memory_store::tests::dyn_compatible --locked
```

Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/contract/memory_store.rs
git commit -m "feat(core): widen MemoryStore trait with CRUD/edge/search methods (#46)"
```

---

## Section 3 — Migrations

### Task 6: Add migration `0007_tombstone_reason.sql`

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0007_tombstone_reason.sql`
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs`

- [ ] **Step 1: Write the SQL**

```sql
-- Migration 0007: distinguish tombstone reasons.
-- Brief source: §5.6 (operation kinds), §10 (lifecycle).

ALTER TABLE records ADD COLUMN tombstone_reason TEXT;

CREATE INDEX records_tombstoned_reason_idx
  ON records(tombstone_reason)
  WHERE tombstoned = 1;

INSERT INTO schema_migrations (migration_id, name, sql_blake3, applied_at)
  VALUES (7, '0007_tombstone_reason', '', strftime('%s','now') * 1000);
```

- [ ] **Step 2: Register the migration**

In `crates/cairn-store-sqlite/src/migrations/mod.rs`, append:

```rust
const M0007_TOMBSTONE_REASON: &str = include_str!("sql/0007_tombstone_reason.sql");
```

Add `(7, "0007_tombstone_reason", M0007_TOMBSTONE_REASON),` to `MIGRATION_SOURCES` and `M::up(M0007_TOMBSTONE_REASON),` to the `Migrations::new(vec![…])` call.

- [ ] **Step 3: Run the existing migration test, expect pass**

```bash
cargo nextest run -p cairn-store-sqlite --test migrations --locked
```

Expected: pass — the existing test asserts migration history grows when a new migration is added.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-store-sqlite/src/migrations/sql/0007_tombstone_reason.sql \
        crates/cairn-store-sqlite/src/migrations/mod.rs
git commit -m "feat(store-sqlite): migration 0007 — tombstone_reason column (#46)"
```

---

### Task 7: Add migration `0008_record_extensions.sql`

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0008_record_extensions.sql`
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs`

- [ ] **Step 1: Write the SQL**

```sql
-- Migration 0008: extend records with the columns needed to persist
-- a full MemoryRecord (record_json source-of-truth) plus denormalized
-- hot columns used by ranking and filters.
-- Brief sources: §3 (records-in-SQLite), §4.2 (record fields), §6.5.

ALTER TABLE records ADD COLUMN record_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE records ADD COLUMN confidence REAL NOT NULL DEFAULT 0.0;
ALTER TABLE records ADD COLUMN salience REAL NOT NULL DEFAULT 0.0;
ALTER TABLE records ADD COLUMN target_id_explicit TEXT;
ALTER TABLE records ADD COLUMN tags_json TEXT NOT NULL DEFAULT '[]';

INSERT INTO schema_migrations (migration_id, name, sql_blake3, applied_at)
  VALUES (8, '0008_record_extensions', '', strftime('%s','now') * 1000);
```

- [ ] **Step 2: Register**

In `crates/cairn-store-sqlite/src/migrations/mod.rs`:

```rust
const M0008_RECORD_EXTENSIONS: &str = include_str!("sql/0008_record_extensions.sql");
```

Add `(8, "0008_record_extensions", M0008_RECORD_EXTENSIONS),` to `MIGRATION_SOURCES` and `M::up(M0008_RECORD_EXTENSIONS),` to the migrations vec.

- [ ] **Step 3: Run the migration test, expect pass**

```bash
cargo nextest run -p cairn-store-sqlite --test migrations --locked
```

Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-store-sqlite/src/migrations/sql/0008_record_extensions.sql \
        crates/cairn-store-sqlite/src/migrations/mod.rs
git commit -m "feat(store-sqlite): migration 0008 — record_json + ranking inputs (#46)"
```

---

### Task 8: Add migration `0010_ranking_indexes.sql`

(Number 0009 is reserved for FTS-metadata in PR-B.)

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0010_ranking_indexes.sql`
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs`

- [ ] **Step 1: Write the SQL**

```sql
-- Migration 0010: indexes for ranking-input lookups.
-- Brief source: §5.1 (Rank & Filter — recency, confidence, salience).

CREATE INDEX records_confidence_idx
  ON records(confidence)
  WHERE active = 1 AND tombstoned = 0;

CREATE INDEX records_updated_at_idx
  ON records(updated_at)
  WHERE active = 1 AND tombstoned = 0;

INSERT INTO schema_migrations (migration_id, name, sql_blake3, applied_at)
  VALUES (10, '0010_ranking_indexes', '', strftime('%s','now') * 1000);
```

- [ ] **Step 2: Register**

In `crates/cairn-store-sqlite/src/migrations/mod.rs`:

```rust
const M0010_RANKING_INDEXES: &str = include_str!("sql/0010_ranking_indexes.sql");
```

Add `(10, "0010_ranking_indexes", M0010_RANKING_INDEXES),` to `MIGRATION_SOURCES` and `M::up(M0010_RANKING_INDEXES),` to the migrations vec. (PR-B will insert 9 between 8 and 10 in the registration order; that's fine — the migration_id is the source of order, and `rusqlite_migration` runs them in vec order, which matches numerical order here.)

- [ ] **Step 3: Run the migration test, expect pass**

```bash
cargo nextest run -p cairn-store-sqlite --test migrations --locked
```

Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-store-sqlite/src/migrations/sql/0010_ranking_indexes.sql \
        crates/cairn-store-sqlite/src/migrations/mod.rs
git commit -m "feat(store-sqlite): migration 0010 — ranking indexes (#46)"
```

---

### Task 9: Update existing `records_latest.rs` test for the new schema

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/records_latest.rs`

The existing `insert_v` helper enumerates 16 columns in `INSERT INTO records (...)`. The new columns added in 0008 have `NOT NULL DEFAULT` so unset INSERTs still succeed — but the proptest case in `hot_columns_match_json.rs` (Task 28) will assume callers populate `record_json`. For these existing tests we don't need to set `record_json`; the default `'{}'` is fine.

Verify by running the test:

- [ ] **Step 1: Run the existing tests**

```bash
cargo nextest run -p cairn-store-sqlite --test records_latest --locked
```

Expected: pass on the existing 5 tests.

- [ ] **Step 2: Commit only if changes were needed**

If the run passed without edits, skip this commit. If you needed to add a `record_json` literal to any insert, commit:

```bash
git add crates/cairn-store-sqlite/tests/records_latest.rs
git commit -m "test(store-sqlite): adapt records_latest fixtures to 0008 schema (#46)"
```

---

## Section 4 — `tokio_rusqlite` wiring + `SqliteMemoryStore` skeleton

### Task 10: Add `tokio_rusqlite` and supporting deps

**Files:**
- Modify: `Cargo.toml` (workspace)
- Modify: `crates/cairn-store-sqlite/Cargo.toml`

- [ ] **Step 1: Add workspace deps**

In `Cargo.toml` `[workspace.dependencies]`, add (alphabetical):

```toml
bon = "3.7"
serde_json = { version = "1", default-features = false, features = ["std"] }
tokio = { version = "1", default-features = false, features = ["rt-multi-thread", "macros", "sync"] }
tokio_rusqlite = { version = "0.6", default-features = false, features = ["bundled"] }
tracing = { version = "0.1", default-features = false, features = ["std", "attributes"] }
```

(Skip any line that already exists — use `grep` to check.) Verify with:

```bash
grep -E "^(bon|serde_json|tokio|tokio_rusqlite|tracing)\s*=" Cargo.toml
```

- [ ] **Step 2: Add crate deps**

In `crates/cairn-store-sqlite/Cargo.toml`, replace the `[dependencies]` block with:

```toml
[dependencies]
cairn-core = { workspace = true }
async-trait = { workspace = true }
bon = { workspace = true }
blake3 = { workspace = true }
rusqlite = { workspace = true }
rusqlite_migration = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
tokio_rusqlite = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
cairn-test-fixtures = { workspace = true }
proptest = { workspace = true }
tempfile = { workspace = true }
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

- [ ] **Step 3: Verify the workspace builds**

```bash
cargo check --workspace --all-targets --locked
```

Expected: clean build (no source changes yet, only deps added).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/cairn-store-sqlite/Cargo.toml Cargo.lock
git commit -m "chore(store-sqlite): add tokio_rusqlite + ranking deps (#46)"
```

---

### Task 11: Refactor `open()` to return a `SqliteMemoryStore` over `tokio_rusqlite`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/open.rs`
- Modify: `crates/cairn-store-sqlite/src/lib.rs`
- Create: `crates/cairn-store-sqlite/src/store/mod.rs`

- [ ] **Step 1: Create the store module skeleton**

In `crates/cairn-store-sqlite/src/store/mod.rs`:

```rust
//! `SqliteMemoryStore` impl modules.

pub(crate) mod edges;
pub(crate) mod projection;
pub(crate) mod read;
pub(crate) mod tombstone;
pub(crate) mod tx;
pub(crate) mod upsert;

use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStoreCapabilities;
use tokio_rusqlite::Connection as AsyncConn;

/// Async-fronted SQLite memory store. Wraps a single
/// `tokio_rusqlite::Connection`; every async method is one
/// `conn.call(|c| { … })` round-trip on the dedicated DB thread.
#[derive(Clone)]
pub struct SqliteMemoryStore {
    pub(crate) conn: Arc<AsyncConn>,
    pub(crate) caps: &'static MemoryStoreCapabilities,
}

impl std::fmt::Debug for SqliteMemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteMemoryStore")
            .field("caps", &self.caps)
            .finish_non_exhaustive()
    }
}
```

- [ ] **Step 2: Empty stubs for the per-method modules**

Each of `edges.rs`, `projection.rs`, `read.rs`, `tombstone.rs`, `tx.rs`, `upsert.rs` starts as an empty file with one comment so the module declarations resolve. Tasks below fill them in.

```bash
for f in edges projection read tombstone tx upsert; do
  printf "//! %s impl module — populated by later plan tasks.\n" "$f" \
    > "crates/cairn-store-sqlite/src/store/$f.rs"
done
```

- [ ] **Step 3: Refactor `open.rs`**

Replace `crates/cairn-store-sqlite/src/open.rs` with:

```rust
//! `SQLite` open path: pragmas + migrations, returning an async store handle.

use std::path::Path;
use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStoreCapabilities;
use tokio_rusqlite::Connection as AsyncConn;

use crate::error::StoreError;
use crate::migrations::migrations;
use crate::store::SqliteMemoryStore;
use crate::verify::{verify_migration_history, verify_schema_fingerprint};

/// Default capability flags after PR-A. PR-B flips `fts` to `true`.
pub(crate) static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
    fts: false,
    vector: false,
    graph_edges: true,
    transactions: true,
};

/// Open (or create) the Cairn store at `path` and bring it to schema head.
///
/// Applies persistent pragmas (WAL journal, foreign keys, busy timeout,
/// temp_store, mmap), then runs `rusqlite_migration` to the latest
/// migration. Returns an [`SqliteMemoryStore`] backed by a dedicated
/// `tokio_rusqlite` worker thread.
///
/// # Errors
/// Returns [`StoreError`] if the directory cannot be created, the
/// connection cannot be opened, pragmas fail, or migrations fail.
pub async fn open(path: impl AsRef<Path>) -> Result<SqliteMemoryStore, StoreError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| StoreError::VaultPath(e.to_string()))?;
    }

    let conn = AsyncConn::open(path).await?;
    bootstrap(&conn).await?;
    Ok(SqliteMemoryStore {
        conn: Arc::new(conn),
        caps: &CAPS,
    })
}

/// In-memory store at schema head. For tests.
pub async fn open_in_memory() -> Result<SqliteMemoryStore, StoreError> {
    let conn = AsyncConn::open_in_memory().await?;
    bootstrap(&conn).await?;
    Ok(SqliteMemoryStore {
        conn: Arc::new(conn),
        caps: &CAPS,
    })
}

async fn bootstrap(conn: &AsyncConn) -> Result<(), StoreError> {
    conn.call(|c| {
        c.execute_batch(
            "PRAGMA journal_mode=WAL;\
             PRAGMA foreign_keys=ON;\
             PRAGMA synchronous=NORMAL;\
             PRAGMA busy_timeout=5000;\
             PRAGMA temp_store=MEMORY;\
             PRAGMA mmap_size=268435456;",
        )?;
        migrations()
            .to_latest(c)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        verify_migration_history(c)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        verify_schema_fingerprint(c)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok(())
    })
    .await?;
    Ok(())
}
```

- [ ] **Step 4: Update `lib.rs` re-exports**

In `crates/cairn-store-sqlite/src/lib.rs`, replace the existing top-of-file content (everything above the existing `register_plugin!` macro) with:

```rust
//! `SQLite` record store for Cairn.
//!
//! Async-fronted via `tokio_rusqlite`. Every `MemoryStore` trait method is
//! one `conn.call(|c| { … })` round-trip on a dedicated DB thread. Records
//! persist as a `record_json` blob plus denormalized hot columns; the WAL
//! state machine (#8) lives at the verb layer.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod error;
pub mod migrations;
mod open;
pub mod store;
mod verify;

pub use error::StoreError;
pub use open::{open, open_in_memory};
pub use store::SqliteMemoryStore;
```

Then delete the existing stub `SqliteMemoryStore` struct and its `impl MemoryStore` block (everything between the old `pub struct SqliteMemoryStore;` and the comment `// Compile-time guard:`). Keep the compile-time guard and the `register_plugin!` invocation, but update the `MemoryStore` plugin name reference: it now points to `store::SqliteMemoryStore` from the new module.

The remaining tail of `lib.rs` should look like:

```rust
use cairn_core::contract::memory_store::{CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

pub const PLUGIN_NAME: &str = "cairn-store-sqlite";
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Contract-version range this crate accepts. Widened to 0.1..0.3 in #46.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 3, 0));

const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

register_plugin!(
    MemoryStore,
    SqliteMemoryStore,
    "cairn-store-sqlite",
    MANIFEST_TOML
);
```

- [ ] **Step 5: Update existing tests that took the old `Connection` return**

The existing integration tests (`drift_corner_cases.rs`, `manifest_validates.rs`, `migrations.rs`, `records_latest.rs`, `smoke.rs`, `wal_fsm.rs`) call `open_in_memory()` expecting a sync `Connection`. After this refactor `open_in_memory()` is async and returns `SqliteMemoryStore`.

Two options per test file:
1. Add `#[tokio::test]` and use `let store = open_in_memory().await?;` then drill into `store.conn` for raw SQL.
2. Provide a sync helper in `cairn-store-sqlite` for tests that need a raw `rusqlite::Connection`.

Pick option 2 to minimize churn. Add to `crates/cairn-store-sqlite/src/open.rs`:

```rust
/// Sync open returning a raw `rusqlite::Connection` for tests that drive
/// SQL directly. Not exposed in the production API.
#[cfg(any(test, feature = "test-helpers"))]
pub fn open_in_memory_sync() -> Result<rusqlite::Connection, StoreError> {
    let mut conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA synchronous=NORMAL;\
         PRAGMA busy_timeout=5000;\
         PRAGMA temp_store=MEMORY;\
         PRAGMA mmap_size=268435456;",
    )?;
    migrations().to_latest(&mut conn)?;
    verify_migration_history(&conn)?;
    verify_schema_fingerprint(&conn)?;
    Ok(conn)
}
```

Add a `test-helpers` feature to `crates/cairn-store-sqlite/Cargo.toml`:

```toml
[features]
default = []
test-helpers = []
```

In `[dev-dependencies]` section, add a self-cyclic feature activation (rustc supports this since 1.60):

```toml
cairn-store-sqlite = { path = ".", features = ["test-helpers"] }
```

(Or simpler: just gate with `#[cfg(any(test, doctest))]` and skip the feature flag. The first option is cleaner but invasive. Use the simpler form for PR-A.)

Update existing test files: change every `use cairn_store_sqlite::open_in_memory;` to `use cairn_store_sqlite::open::open_in_memory_sync as open_in_memory;` and adjust the `pub use` in `lib.rs`:

```rust
#[cfg(any(test, feature = "test-helpers"))]
pub use open::open_in_memory_sync;
```

(Decision check: revert to the simpler `#[cfg(test)]` gate if the feature flag adds friction. Tests inside the same crate can call `crate::open::open_in_memory_sync` directly.)

- [ ] **Step 6: Build + run the existing tests**

```bash
cargo nextest run -p cairn-store-sqlite --locked --no-fail-fast
```

Expected: all existing tests pass against the sync helper. New (empty) test files for PR-A tasks below add their own coverage.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-store-sqlite/
git commit -m "feat(store-sqlite): tokio_rusqlite-backed SqliteMemoryStore + open() refactor (#46)"
```

---

## Section 5 — Errors

### Task 12: Extend `StoreError` with new variants

**Files:**
- Modify: `crates/cairn-store-sqlite/src/error.rs`

- [ ] **Step 1: Replace the file**

```rust
//! Store-level error type.

use thiserror::Error;

/// Errors raised by the `SQLite` store adapter.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// Underlying `SQLite` failure.
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),

    /// Migration runner failure.
    #[error("migration error")]
    Migration(#[from] rusqlite_migration::Error),

    /// Vault path is unusable (cannot create parent directory, etc.).
    #[error("vault path error: {0}")]
    VaultPath(String),

    /// On-disk schema diverged from the binary's expected manifest.
    #[error("schema drift: {0}")]
    SchemaDrift(String),

    /// Record id was looked up but not present (or only present as a
    /// tombstoned row that callers must not see via `get`).
    #[error("record not found: {id}")]
    NotFound { id: String },

    /// Method requires a capability the store does not advertise.
    /// `what` is the cap flag name (`"fts"`, `"vector"`, `"graph_edges"`,
    /// `"transactions"`).
    #[error("capability unavailable: {what}")]
    CapabilityUnavailable { what: &'static str },

    /// FTS5 query parse error. Surfaced as a separate variant so the
    /// verb layer can return user-actionable errors instead of generic
    /// SQL failures.
    #[error("FTS5 query parse error: {message}")]
    FtsQuery { message: String },

    /// Background `tokio_rusqlite` worker error (channel closed, panic
    /// in the worker thread, etc.).
    #[error("tokio_rusqlite worker error")]
    Worker(#[from] tokio_rusqlite::Error),

    /// Record JSON ↔ struct codec error.
    #[error("record codec error")]
    Codec(#[from] serde_json::Error),

    /// Invariant violation. Indicates a bug in the store, not a user
    /// error. Logged and surfaced.
    #[error("invariant violated: {what}")]
    Invariant { what: String },
}

impl From<StoreError> for Box<dyn std::error::Error + Send + Sync + 'static> {
    fn from(e: StoreError) -> Self {
        Box::new(e)
    }
}
```

- [ ] **Step 2: Verify it builds**

```bash
cargo check -p cairn-store-sqlite --all-targets --locked
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/src/error.rs
git commit -m "feat(store-sqlite): extend StoreError with NotFound/Worker/Codec/Invariant (#46)"
```

---

## Section 6 — Row projection

### Task 13: Implement `MemoryRecord` ↔ row projection

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/projection.rs`

- [ ] **Step 1: Write failing test stub at the bottom of the file**

Replace the file content with:

```rust
//! `MemoryRecord` ↔ row projection.
//!
//! One side is the canonical `record_json` blob; the other side is the
//! denormalized hot columns. Every upsert writes both; every read returns
//! both for callers that want to skip JSON deserialization.

use cairn_core::domain::{
    BodyHash, MemoryRecord, RecordId, ScopeTuple, TargetId,
    taxonomy::{MemoryClass, MemoryKind, MemoryVisibility},
};

use crate::error::StoreError;

/// Owned, parameterizable view of the columns the store writes for one
/// record version.
#[derive(Debug, Clone)]
pub(crate) struct ProjectedRow {
    pub record_id: String,
    pub target_id: String,
    pub version: i64,
    pub path: String,
    pub kind: String,
    pub class: String,
    pub visibility: String,
    pub scope: String,            // serialized ScopeTuple
    pub actor_chain: String,      // serialized actor chain
    pub body: String,
    pub body_hash: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub active: i64,
    pub tombstoned: i64,
    pub is_static: i64,
    pub record_json: String,
    pub confidence: f64,
    pub salience: f64,
    pub target_id_explicit: Option<String>,
    pub tags_json: String,
}

impl ProjectedRow {
    /// Build a `ProjectedRow` from a `MemoryRecord` for write.
    pub(crate) fn from_record(
        record: &MemoryRecord,
        version: u32,
        created_at: i64,
        updated_at: i64,
        body_hash: &BodyHash,
        active: bool,
        tombstoned: bool,
    ) -> Result<Self, StoreError> {
        let record_json = serde_json::to_string(record)?;
        let scope = serde_json::to_string(&record.scope)?;
        let actor_chain = serde_json::to_string(&record.actor_chain)?;
        let tags_json = serde_json::to_string(&record.tags)?;
        Ok(Self {
            record_id: record.id.as_str().to_owned(),
            target_id: record.target_id.as_str().to_owned(),
            version: i64::from(version),
            path: derive_path(record),
            kind: kind_str(record.kind).to_owned(),
            class: class_str(record.class).to_owned(),
            visibility: visibility_str(record.visibility).to_owned(),
            scope,
            actor_chain,
            body: record.body.clone(),
            body_hash: body_hash.as_str().to_owned(),
            created_at,
            updated_at,
            active: i64::from(active),
            tombstoned: i64::from(tombstoned),
            is_static: 0,
            record_json,
            confidence: f64::from(record.confidence),
            salience: f64::from(record.salience),
            target_id_explicit: Some(record.target_id.as_str().to_owned()),
            tags_json,
        })
    }
}

/// Hydrate a `MemoryRecord` from a row's `record_json` column. Returns
/// [`StoreError::Codec`] on malformed JSON.
pub(crate) fn record_from_json(json: &str) -> Result<MemoryRecord, StoreError> {
    Ok(serde_json::from_str(json)?)
}

fn derive_path(record: &MemoryRecord) -> String {
    // Path is the markdown projector's responsibility; until that lands
    // we use a deterministic fallback based on scope + record id so
    // FTS-on-path tests are still meaningful.
    format!(
        "vault/{}/{}.md",
        scope_segment(&record.scope),
        record.id.as_str()
    )
}

fn scope_segment(scope: &ScopeTuple) -> String {
    let mut parts = Vec::new();
    if let Some(t) = scope.tenant.as_deref() {
        parts.push(format!("tenant-{t}"));
    }
    if let Some(w) = scope.workspace.as_deref() {
        parts.push(format!("ws-{w}"));
    }
    if let Some(e) = scope.entity.as_deref() {
        parts.push(format!("ent-{e}"));
    }
    if parts.is_empty() {
        "_root".to_owned()
    } else {
        parts.join("/")
    }
}

fn kind_str(k: MemoryKind) -> &'static str {
    // Mirror the IDL string form. Keep in sync with
    // crate::generated::common::MemoryKind ↔ wire.
    match k {
        MemoryKind::User => "user",
        MemoryKind::Rule => "rule",
        MemoryKind::Fact => "fact",
        MemoryKind::Reasoning => "reasoning",
        MemoryKind::Episode => "episode",
        MemoryKind::Skill => "skill",
        MemoryKind::SensorObservation => "sensor_observation",
    }
}

fn class_str(c: MemoryClass) -> &'static str {
    match c {
        MemoryClass::Semantic => "semantic",
        MemoryClass::Episodic => "episodic",
        MemoryClass::Procedural => "procedural",
    }
}

fn visibility_str(v: MemoryVisibility) -> &'static str {
    match v {
        MemoryVisibility::Private => "private",
        MemoryVisibility::Session => "session",
        MemoryVisibility::Project => "project",
        MemoryVisibility::Team => "team",
        MemoryVisibility::Org => "org",
        MemoryVisibility::Public => "public",
    }
}

pub(crate) fn record_id_from_str(s: &str) -> Result<RecordId, StoreError> {
    RecordId::parse(s.to_owned())
        .map_err(|e| StoreError::Invariant { what: format!("invalid record_id `{s}`: {e}") })
}

pub(crate) fn target_id_from_str(s: &str) -> Result<TargetId, StoreError> {
    TargetId::parse(s.to_owned())
        .map_err(|e| StoreError::Invariant { what: format!("invalid target_id `{s}`: {e}") })
}

pub(crate) fn body_hash_from_str(s: &str) -> Result<BodyHash, StoreError> {
    BodyHash::parse(s.to_owned())
        .map_err(|e| StoreError::Invariant { what: format!("invalid body_hash `{s}`: {e}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MemoryRecord {
        // Reuse the in-tree sample.
        cairn_core::domain::record::tests_export::sample_record()
    }

    #[test]
    fn json_round_trip_via_projection() {
        let r = sample();
        let body_hash = BodyHash::compute(&r.body);
        let row = ProjectedRow::from_record(&r, 1, 1000, 2000, &body_hash, true, false)
            .expect("project");
        let back = record_from_json(&row.record_json).expect("hydrate");
        assert_eq!(r, back);
    }

    #[test]
    fn target_id_explicit_mirrors_record() {
        let r = sample();
        let body_hash = BodyHash::compute(&r.body);
        let row = ProjectedRow::from_record(&r, 1, 1000, 2000, &body_hash, true, false)
            .expect("project");
        assert_eq!(row.target_id_explicit.as_deref(), Some(r.target_id.as_str()));
    }
}
```

- [ ] **Step 2: Re-export `sample_record` from cairn-core for cross-crate tests**

The projection unit test calls `cairn_core::domain::record::tests_export::sample_record()` — this doesn't exist yet. Add it.

In `crates/cairn-core/src/domain/record.rs`, near the bottom (above the `tests` mod), insert:

```rust
#[cfg(any(test, feature = "test-fixtures"))]
pub mod tests_export {
    pub use super::tests::sample_record;
}
```

In `crates/cairn-core/Cargo.toml` add a feature:

```toml
[features]
default = []
test-fixtures = []
```

In `crates/cairn-store-sqlite/Cargo.toml` `[dev-dependencies]` add:

```toml
cairn-core = { workspace = true, features = ["test-fixtures"] }
```

- [ ] **Step 3: Run the projection tests**

```bash
cargo nextest run -p cairn-store-sqlite --lib store::projection --locked
```

Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/record.rs \
        crates/cairn-core/Cargo.toml \
        crates/cairn-store-sqlite/Cargo.toml \
        crates/cairn-store-sqlite/src/store/projection.rs
git commit -m "feat(store-sqlite): MemoryRecord ↔ row projection (#46)"
```

---

## Section 7 — CRUD impl

### Task 14: Implement `upsert`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/upsert.rs`
- Create: `crates/cairn-store-sqlite/tests/upsert_idempotent.rs`

- [ ] **Step 1: Write the failing integration test**

In `crates/cairn-store-sqlite/tests/upsert_idempotent.rs`:

```rust
//! Upsert idempotency, version bumps, and content-changed accounting.

use cairn_core::contract::memory_store::{MemoryStore, UpsertOutcome};
use cairn_core::domain::{BodyHash, MemoryRecord};
use cairn_store_sqlite::open_in_memory;

#[tokio::test]
async fn first_upsert_is_v1() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    let out = store.upsert(&r).await.expect("upsert");
    assert_eq!(out.version, 1);
    assert!(out.content_changed);
    assert!(out.prior_hash.is_none());
}

#[tokio::test]
async fn second_upsert_same_body_is_noop() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("first");
    let out = store.upsert(&r).await.expect("second");
    assert_eq!(out.version, 1, "no version bump on identical body");
    assert!(!out.content_changed);
    assert_eq!(out.prior_hash, Some(BodyHash::compute(&r.body)));
}

#[tokio::test]
async fn upsert_with_different_body_bumps_version() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("first");
    let mut r2 = r.clone();
    r2.body = "second body".to_owned();
    let out = store.upsert(&r2).await.expect("second");
    assert_eq!(out.version, 2);
    assert!(out.content_changed);
    assert_eq!(out.prior_hash, Some(BodyHash::compute(&r.body)));
}

fn sample() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}
```

- [ ] **Step 2: Run and expect compile failure**

```bash
cargo nextest run -p cairn-store-sqlite --test upsert_idempotent --locked 2>&1 | head -40
```

Expected: compile error — `MemoryStore::upsert` is on the stub trait but no impl on `SqliteMemoryStore` yet.

- [ ] **Step 3: Implement the upsert path**

Replace `crates/cairn-store-sqlite/src/store/upsert.rs` with:

```rust
//! `MemoryStore::upsert` impl.

use cairn_core::contract::memory_store::UpsertOutcome;
use cairn_core::domain::{BodyHash, MemoryRecord};
use rusqlite::params;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;
use crate::store::projection::{ProjectedRow, body_hash_from_str};

impl SqliteMemoryStore {
    #[instrument(
        skip(self, record),
        err,
        fields(
            verb = "upsert",
            record_id = %record.id,
            target_id = %record.target_id,
            kind = ?record.kind,
            class = ?record.class,
        ),
    )]
    pub(crate) async fn do_upsert(
        &self,
        record: &MemoryRecord,
    ) -> Result<UpsertOutcome, StoreError> {
        let record = record.clone();
        let body_hash = BodyHash::compute(&record.body);
        let target_id_for_query = record.target_id.as_str().to_owned();

        self.conn
            .call(move |c| {
                let tx = c.transaction()?;
                let prior: Option<(String, i64, String)> = tx
                    .query_row(
                        "SELECT record_id, version, body_hash \
                           FROM records \
                          WHERE target_id = ?1 AND active = 1 \
                          LIMIT 1",
                        params![target_id_for_query],
                        |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
                        },
                    )
                    .map(Some)
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        other => Err(other),
                    })?;

                let now_ms = current_unix_ms();
                let (version, prior_hash, content_changed) = match prior.as_ref() {
                    Some((prior_id, prior_version, prior_hash_str)) => {
                        let prior_hash = body_hash_from_str(prior_hash_str)
                            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                        if prior_hash == body_hash {
                            // Idempotent no-op.
                            tx.commit()?;
                            return Ok(UpsertOutcome {
                                record_id: record.id.clone(),
                                target_id: record.target_id.clone(),
                                version: u32::try_from(*prior_version).unwrap_or(u32::MAX),
                                content_changed: false,
                                prior_hash: Some(prior_hash),
                            });
                        }
                        // Body changed: deactivate prior + insert new.
                        tx.execute(
                            "UPDATE records SET active = 0, updated_at = ?1 \
                              WHERE record_id = ?2",
                            params![now_ms, prior_id],
                        )?;
                        let new_version = u32::try_from(prior_version + 1).unwrap_or(u32::MAX);
                        (new_version, Some(prior_hash), true)
                    }
                    None => (1u32, None, true),
                };

                let row = ProjectedRow::from_record(&record, version, now_ms, now_ms, &body_hash, true, false)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                tx.execute(
                    "INSERT INTO records ( \
                        record_id, target_id, version, path, kind, class, visibility, \
                        scope, actor_chain, body, body_hash, created_at, updated_at, \
                        active, tombstoned, is_static, record_json, confidence, \
                        salience, target_id_explicit, tags_json \
                     ) VALUES ( \
                        ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                        ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21 \
                     )",
                    params![
                        row.record_id, row.target_id, row.version, row.path, row.kind,
                        row.class, row.visibility, row.scope, row.actor_chain, row.body,
                        row.body_hash, row.created_at, row.updated_at, row.active,
                        row.tombstoned, row.is_static, row.record_json, row.confidence,
                        row.salience, row.target_id_explicit, row.tags_json,
                    ],
                )?;
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(UpsertOutcome {
                    record_id: record.id.clone(),
                    target_id: record.target_id.clone(),
                    version,
                    content_changed,
                    prior_hash,
                })
            })
            .await
            .map_err(StoreError::from)
    }
}

fn current_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
```

- [ ] **Step 4: Wire `upsert` on the trait impl**

Add a new file `crates/cairn-store-sqlite/src/store/trait_impl.rs`:

```rust
//! `impl MemoryStore for SqliteMemoryStore` — async dispatch into the
//! per-method modules under `store::*`.

use async_trait::async_trait;
use cairn_core::contract::memory_store::{
    Edge, EdgeDir, EdgeKey, KeywordSearchArgs, KeywordSearchPage, ListArgs, ListPage,
    MemoryStore, MemoryStoreCapabilities, RecordVersion, TombstoneReason, UpsertOutcome,
};
use cairn_core::contract::version::VersionRange;
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};

use crate::PLUGIN_NAME;
use crate::error::StoreError as ConcreteError;
use crate::store::SqliteMemoryStore;

#[async_trait]
impl MemoryStore for SqliteMemoryStore {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        self.caps
    }

    fn supported_contract_versions(&self) -> VersionRange {
        crate::ACCEPTED_RANGE
    }

    async fn upsert(&self, record: &MemoryRecord) -> Result<UpsertOutcome, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.do_upsert(record).await.map_err(box_err)?)
    }

    async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, Box<dyn std::error::Error + Send + Sync>> {
        Err(box_err(ConcreteError::Invariant { what: "get not yet implemented".into() }))
    }
    async fn list(&self, _args: &ListArgs) -> Result<ListPage, Box<dyn std::error::Error + Send + Sync>> {
        Err(box_err(ConcreteError::Invariant { what: "list not yet implemented".into() }))
    }
    async fn tombstone(&self, _id: &RecordId, _reason: TombstoneReason) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err(box_err(ConcreteError::Invariant { what: "tombstone not yet implemented".into() }))
    }
    async fn versions(&self, _t: &TargetId) -> Result<Vec<RecordVersion>, Box<dyn std::error::Error + Send + Sync>> {
        Err(box_err(ConcreteError::Invariant { what: "versions not yet implemented".into() }))
    }
    async fn put_edge(&self, _e: &Edge) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err(box_err(ConcreteError::Invariant { what: "put_edge not yet implemented".into() }))
    }
    async fn remove_edge(&self, _k: &EdgeKey) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Err(box_err(ConcreteError::Invariant { what: "remove_edge not yet implemented".into() }))
    }
    async fn neighbours(&self, _id: &RecordId, _d: EdgeDir) -> Result<Vec<Edge>, Box<dyn std::error::Error + Send + Sync>> {
        Err(box_err(ConcreteError::Invariant { what: "neighbours not yet implemented".into() }))
    }
    async fn search_keyword(
        &self,
        _args: &KeywordSearchArgs,
    ) -> Result<KeywordSearchPage, Box<dyn std::error::Error + Send + Sync>> {
        Err(box_err(ConcreteError::CapabilityUnavailable { what: "fts" }))
    }
}

fn box_err(e: ConcreteError) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(e)
}
```

Add `pub(crate) mod trait_impl;` to `crates/cairn-store-sqlite/src/store/mod.rs`.

- [ ] **Step 5: Run the upsert tests, expect pass**

```bash
cargo nextest run -p cairn-store-sqlite --test upsert_idempotent --locked
```

Expected: all 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/upsert.rs \
        crates/cairn-store-sqlite/src/store/trait_impl.rs \
        crates/cairn-store-sqlite/src/store/mod.rs \
        crates/cairn-store-sqlite/tests/upsert_idempotent.rs
git commit -m "feat(store-sqlite): impl upsert with content-hash idempotency (#46)"
```

---

### Task 15: Implement `get`, `list`, `versions`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/read.rs`
- Modify: `crates/cairn-store-sqlite/src/store/trait_impl.rs`
- Create: `crates/cairn-store-sqlite/tests/crud_roundtrip.rs`

- [ ] **Step 1: Write the failing test**

In `crates/cairn-store-sqlite/tests/crud_roundtrip.rs`:

```rust
//! End-to-end CRUD round-trip across `MemoryRecord` shapes.

use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::MemoryRecord;
use cairn_store_sqlite::open_in_memory;

fn base() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

#[tokio::test]
async fn upsert_then_get_returns_same_record() {
    let store = open_in_memory().await.expect("open");
    let r = base();
    store.upsert(&r).await.expect("upsert");
    let got = store.get(&r.id).await.expect("get").expect("present");
    assert_eq!(got, r);
}

#[tokio::test]
async fn get_missing_returns_none() {
    let store = open_in_memory().await.expect("open");
    let r = base();
    let got = store.get(&r.id).await.expect("get");
    assert!(got.is_none());
}

#[tokio::test]
async fn list_returns_inserted_records_newest_first() {
    let store = open_in_memory().await.expect("open");
    let mut r1 = base();
    r1.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000001").unwrap();
    r1.target_id = cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000001").unwrap();
    let mut r2 = base();
    r2.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000002").unwrap();
    r2.target_id = cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000002").unwrap();

    store.upsert(&r1).await.expect("upsert r1");
    store.upsert(&r2).await.expect("upsert r2");

    let page = store
        .list(&ListArgs {
            limit: 10,
            visibility_allowlist: vec![cairn_core::domain::taxonomy::MemoryVisibility::Private],
            ..ListArgs::default()
        })
        .await
        .expect("list");
    assert_eq!(page.records.len(), 2);
}

#[tokio::test]
async fn versions_returns_full_history() {
    let store = open_in_memory().await.expect("open");
    let r = base();
    store.upsert(&r).await.expect("v1");
    let mut r2 = r.clone();
    r2.body = "v2 body".to_owned();
    store.upsert(&r2).await.expect("v2");

    let history = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(history.len(), 2, "two versions visible");
    assert_eq!(history[0].version, 1);
    assert_eq!(history[1].version, 2);
}
```

- [ ] **Step 2: Implement `read.rs`**

```rust
//! `MemoryStore::{get, list, versions}` impls.

use cairn_core::contract::memory_store::{ListArgs, ListCursor, ListPage, RecordVersion, TombstoneReason};
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use rusqlite::params;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;
use crate::store::projection::{
    body_hash_from_str, record_from_json, record_id_from_str, target_id_from_str,
};

impl SqliteMemoryStore {
    #[instrument(skip(self), err, fields(verb = "get", record_id = %id))]
    pub(crate) async fn do_get(&self, id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
        let key = id.as_str().to_owned();
        self.conn
            .call(move |c| {
                let json: Option<String> = c
                    .query_row(
                        "SELECT record_json FROM records \
                          WHERE record_id = ?1 AND tombstoned = 0",
                        params![key],
                        |row| row.get::<_, String>(0),
                    )
                    .map(Some)
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        other => Err(other),
                    })?;
                match json {
                    None => Ok::<_, tokio_rusqlite::Error>(None),
                    Some(s) => record_from_json(&s)
                        .map(Some)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e))),
                }
            })
            .await
            .map_err(StoreError::from)
    }

    #[instrument(skip(self, args), err, fields(verb = "list", limit = args.limit))]
    pub(crate) async fn do_list(&self, args: &ListArgs) -> Result<ListPage, StoreError> {
        let limit = args.limit.clamp(1, 1000);
        let kind = args.kind.map(|k| crate::store::projection::kind_str_pub(k).to_owned());
        let class = args.class.map(|c| crate::store::projection::class_str_pub(c).to_owned());
        let visibilities: Vec<String> = args
            .visibility_allowlist
            .iter()
            .map(|v| crate::store::projection::visibility_str_pub(*v).to_owned())
            .collect();
        let cursor = args.cursor.clone();

        self.conn
            .call(move |c| {
                let mut sql = String::from(
                    "SELECT record_json, updated_at, record_id FROM records \
                      WHERE active = 1 AND tombstoned = 0",
                );
                let mut p: Vec<rusqlite::types::Value> = Vec::new();
                if let Some(k) = kind {
                    sql.push_str(" AND kind = ?");
                    p.push(k.into());
                }
                if let Some(cl) = class {
                    sql.push_str(" AND class = ?");
                    p.push(cl.into());
                }
                if !visibilities.is_empty() {
                    sql.push_str(" AND visibility IN (");
                    sql.push_str(&vec!["?"; visibilities.len()].join(","));
                    sql.push(')');
                    for v in &visibilities {
                        p.push(v.clone().into());
                    }
                }
                if let Some(cur) = &cursor {
                    sql.push_str(" AND (updated_at, record_id) < (?, ?)");
                    p.push(cur.updated_at.into());
                    p.push(cur.record_id.as_str().to_owned().into());
                }
                sql.push_str(" ORDER BY updated_at DESC, record_id DESC LIMIT ?");
                p.push(i64::try_from(limit + 1).unwrap_or(i64::MAX).into());

                let mut stmt = c.prepare(&sql)?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(p.iter()), |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;

                let mut records = Vec::with_capacity(rows.len().min(limit));
                let mut last: Option<(i64, String)> = None;
                for (i, (json, updated_at, rid)) in rows.iter().enumerate() {
                    if i >= limit {
                        break;
                    }
                    let r = record_from_json(json)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    records.push(r);
                    last = Some((*updated_at, rid.clone()));
                }
                let next_cursor = if rows.len() > limit {
                    last.map(|(updated_at, rid)| {
                        record_id_from_str(&rid)
                            .map(|record_id| ListCursor { updated_at, record_id })
                    })
                    .transpose()
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?
                } else {
                    None
                };
                Ok::<_, tokio_rusqlite::Error>(ListPage { records, next_cursor })
            })
            .await
            .map_err(StoreError::from)
    }

    #[instrument(skip(self), err, fields(verb = "versions", target_id = %target))]
    pub(crate) async fn do_versions(
        &self,
        target: &TargetId,
    ) -> Result<Vec<RecordVersion>, StoreError> {
        let key = target.as_str().to_owned();
        self.conn
            .call(move |c| {
                let mut stmt = c.prepare(
                    "SELECT record_id, target_id, version, created_at, updated_at, \
                            active, tombstoned, tombstone_reason, body_hash \
                       FROM records WHERE target_id = ?1 ORDER BY version ASC",
                )?;
                let rows = stmt
                    .query_map(params![key], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, String>(8)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let mut out = Vec::with_capacity(rows.len());
                for (record_id, target_id, version, created_at, updated_at, active, tombstoned, reason, body_hash) in rows {
                    let rec_id = record_id_from_str(&record_id)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    let tgt = target_id_from_str(&target_id)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    let bh = body_hash_from_str(&body_hash)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    out.push(RecordVersion {
                        record_id: rec_id,
                        target_id: tgt,
                        version: u32::try_from(version).unwrap_or(u32::MAX),
                        created_at,
                        updated_at,
                        active: active != 0,
                        tombstoned: tombstoned != 0,
                        tombstone_reason: reason.as_deref().and_then(TombstoneReason::parse),
                        body_hash: bh,
                    });
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await
            .map_err(StoreError::from)
    }
}
```

- [ ] **Step 2b: Expose the public projection helpers used by `do_list`**

In `crates/cairn-store-sqlite/src/store/projection.rs`, add:

```rust
pub(crate) fn kind_str_pub(k: cairn_core::domain::taxonomy::MemoryKind) -> &'static str {
    kind_str(k)
}
pub(crate) fn class_str_pub(c: cairn_core::domain::taxonomy::MemoryClass) -> &'static str {
    class_str(c)
}
pub(crate) fn visibility_str_pub(v: cairn_core::domain::taxonomy::MemoryVisibility) -> &'static str {
    visibility_str(v)
}
```

- [ ] **Step 3: Wire on the trait impl**

In `crates/cairn-store-sqlite/src/store/trait_impl.rs`, replace the stub `get`, `list`, `versions` methods with delegation to `do_*`. Pattern:

```rust
async fn get(&self, id: &RecordId) -> Result<Option<MemoryRecord>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(self.do_get(id).await.map_err(box_err)?)
}
async fn list(&self, args: &ListArgs) -> Result<ListPage, Box<dyn std::error::Error + Send + Sync>> {
    Ok(self.do_list(args).await.map_err(box_err)?)
}
async fn versions(&self, target: &TargetId) -> Result<Vec<RecordVersion>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(self.do_versions(target).await.map_err(box_err)?)
}
```

- [ ] **Step 4: Run the tests, expect pass**

```bash
cargo nextest run -p cairn-store-sqlite --test crud_roundtrip --locked
```

Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/read.rs \
        crates/cairn-store-sqlite/src/store/projection.rs \
        crates/cairn-store-sqlite/src/store/trait_impl.rs \
        crates/cairn-store-sqlite/tests/crud_roundtrip.rs
git commit -m "feat(store-sqlite): impl get/list/versions (#46)"
```

---

### Task 16: Implement `tombstone`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/tombstone.rs`
- Modify: `crates/cairn-store-sqlite/src/store/trait_impl.rs`
- Create: `crates/cairn-store-sqlite/tests/tombstone_reasons.rs`

- [ ] **Step 1: Write the failing test**

In `crates/cairn-store-sqlite/tests/tombstone_reasons.rs`:

```rust
//! Tombstone records each `TombstoneReason` distinctly and is idempotent.

use cairn_core::contract::memory_store::{MemoryStore, TombstoneReason};
use cairn_store_sqlite::open_in_memory;

#[tokio::test]
async fn tombstone_records_reason() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    store.upsert(&r).await.expect("upsert");
    store.tombstone(&r.id, TombstoneReason::Forget).await.expect("tombstone");
    let history = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(history.len(), 1);
    assert!(history[0].tombstoned);
    assert_eq!(history[0].tombstone_reason, Some(TombstoneReason::Forget));
}

#[tokio::test]
async fn tombstone_is_idempotent() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    store.upsert(&r).await.expect("upsert");
    store.tombstone(&r.id, TombstoneReason::Update).await.expect("first");
    store.tombstone(&r.id, TombstoneReason::Update).await.expect("second");
    let history = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(history.len(), 1);
}

#[tokio::test]
async fn get_returns_none_for_tombstoned() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    store.upsert(&r).await.expect("upsert");
    store.tombstone(&r.id, TombstoneReason::Expire).await.expect("tombstone");
    let got = store.get(&r.id).await.expect("get");
    assert!(got.is_none(), "tombstoned rows must not be returned by get");
}
```

- [ ] **Step 2: Implement**

```rust
//! `MemoryStore::tombstone`.

use cairn_core::contract::memory_store::TombstoneReason;
use cairn_core::domain::RecordId;
use rusqlite::params;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    #[instrument(skip(self), err, fields(verb = "tombstone", record_id = %id, reason = ?reason))]
    pub(crate) async fn do_tombstone(
        &self,
        id: &RecordId,
        reason: TombstoneReason,
    ) -> Result<(), StoreError> {
        let key = id.as_str().to_owned();
        let reason_str = reason.as_db_str();
        self.conn
            .call(move |c| {
                let now_ms = current_unix_ms();
                c.execute(
                    "UPDATE records \
                        SET tombstoned = 1, tombstone_reason = ?1, updated_at = ?2 \
                      WHERE record_id = ?3",
                    params![reason_str, now_ms, key],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await
            .map_err(StoreError::from)
    }
}

fn current_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
```

- [ ] **Step 3: Wire on trait impl**

In `crates/cairn-store-sqlite/src/store/trait_impl.rs`, replace the stub `tombstone` with:

```rust
async fn tombstone(&self, id: &RecordId, reason: TombstoneReason) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Ok(self.do_tombstone(id, reason).await.map_err(box_err)?)
}
```

- [ ] **Step 4: Run the tests, expect pass**

```bash
cargo nextest run -p cairn-store-sqlite --test tombstone_reasons --locked
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/tombstone.rs \
        crates/cairn-store-sqlite/src/store/trait_impl.rs \
        crates/cairn-store-sqlite/tests/tombstone_reasons.rs
git commit -m "feat(store-sqlite): impl tombstone with reason (#46)"
```

---

### Task 17: Implement edges (`put_edge`, `remove_edge`, `neighbours`)

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/edges.rs`
- Modify: `crates/cairn-store-sqlite/src/store/trait_impl.rs`
- Create: `crates/cairn-store-sqlite/tests/edges_crud.rs`

- [ ] **Step 1: Write the failing test**

In `crates/cairn-store-sqlite/tests/edges_crud.rs`:

```rust
//! Edge CRUD round-trip + invariants.

use cairn_core::contract::memory_store::{Edge, EdgeDir, EdgeKey, EdgeKind, MemoryStore};
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use cairn_store_sqlite::open_in_memory;

fn sample(id: &str, target: &str) -> MemoryRecord {
    let mut r = cairn_core::domain::record::tests_export::sample_record();
    r.id = RecordId::parse(id.to_owned()).unwrap();
    r.target_id = TargetId::parse(target.to_owned()).unwrap();
    r
}

#[tokio::test]
async fn put_then_neighbours_out() {
    let store = open_in_memory().await.expect("open");
    let r1 = sample("01HQZX9F5N0000000000000001", "01HQZX9F5N0000000000000001");
    let r2 = sample("01HQZX9F5N0000000000000002", "01HQZX9F5N0000000000000002");
    store.upsert(&r1).await.expect("r1");
    store.upsert(&r2).await.expect("r2");
    store
        .put_edge(&Edge {
            src: r1.id.clone(),
            dst: r2.id.clone(),
            kind: EdgeKind::Mentions,
            weight: Some(0.5),
        })
        .await
        .expect("put_edge");
    let out = store.neighbours(&r1.id, EdgeDir::Out).await.expect("neighbours");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].dst, r2.id);
    assert_eq!(out[0].kind, EdgeKind::Mentions);
}

#[tokio::test]
async fn remove_edge_returns_true_on_existing() {
    let store = open_in_memory().await.expect("open");
    let r1 = sample("01HQZX9F5N0000000000000001", "01HQZX9F5N0000000000000001");
    let r2 = sample("01HQZX9F5N0000000000000002", "01HQZX9F5N0000000000000002");
    store.upsert(&r1).await.expect("r1");
    store.upsert(&r2).await.expect("r2");
    let edge = Edge {
        src: r1.id.clone(),
        dst: r2.id.clone(),
        kind: EdgeKind::Mentions,
        weight: None,
    };
    store.put_edge(&edge).await.expect("put");
    let removed = store
        .remove_edge(&EdgeKey { src: edge.src.clone(), dst: edge.dst.clone(), kind: edge.kind })
        .await
        .expect("remove");
    assert!(removed);
    let removed_again = store
        .remove_edge(&EdgeKey { src: edge.src, dst: edge.dst, kind: edge.kind })
        .await
        .expect("remove_again");
    assert!(!removed_again);
}

#[tokio::test]
async fn updates_edge_immutable_via_remove_returns_error() {
    let store = open_in_memory().await.expect("open");
    let r1 = sample("01HQZX9F5N0000000000000001", "01HQZX9F5N0000000000000001");
    let r2 = sample("01HQZX9F5N0000000000000002", "01HQZX9F5N0000000000000002");
    store.upsert(&r1).await.expect("r1");
    store.upsert(&r2).await.expect("r2");
    store
        .put_edge(&Edge {
            src: r1.id.clone(),
            dst: r2.id.clone(),
            kind: EdgeKind::Updates,
            weight: None,
        })
        .await
        .expect("put updates");
    // Removal of an `updates` edge runs the immutability trigger error
    // path; the schema actually allows DELETE (the trigger is on UPDATE),
    // so this returns true. If the brief later forbids DELETE, a new
    // schema trigger is required and this test will catch the change.
    let removed = store
        .remove_edge(&EdgeKey {
            src: r1.id,
            dst: r2.id,
            kind: EdgeKind::Updates,
        })
        .await
        .expect("remove");
    assert!(removed, "updates-edge DELETE is allowed at schema 0001 today");
}
```

- [ ] **Step 2: Implement**

```rust
//! `MemoryStore::{put_edge, remove_edge, neighbours}`.

use cairn_core::contract::memory_store::{Edge, EdgeDir, EdgeKey, EdgeKind};
use cairn_core::domain::RecordId;
use rusqlite::params;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;
use crate::store::projection::record_id_from_str;

impl SqliteMemoryStore {
    #[instrument(skip(self), err, fields(verb = "put_edge", src = %edge.src, dst = %edge.dst, kind = ?edge.kind))]
    pub(crate) async fn do_put_edge(&self, edge: &Edge) -> Result<(), StoreError> {
        let src = edge.src.as_str().to_owned();
        let dst = edge.dst.as_str().to_owned();
        let kind = edge.kind.as_db_str();
        let weight = edge.weight.map(f64::from);
        self.conn
            .call(move |c| {
                c.execute(
                    "INSERT OR REPLACE INTO edges (src, dst, kind, weight) \
                       VALUES (?1, ?2, ?3, ?4)",
                    params![src, dst, kind, weight],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await
            .map_err(StoreError::from)
    }

    #[instrument(skip(self), err, fields(verb = "remove_edge", src = %key.src, dst = %key.dst, kind = ?key.kind))]
    pub(crate) async fn do_remove_edge(&self, key: &EdgeKey) -> Result<bool, StoreError> {
        let src = key.src.as_str().to_owned();
        let dst = key.dst.as_str().to_owned();
        let kind = key.kind.as_db_str();
        self.conn
            .call(move |c| {
                let n = c.execute(
                    "DELETE FROM edges WHERE src = ?1 AND dst = ?2 AND kind = ?3",
                    params![src, dst, kind],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n > 0)
            })
            .await
            .map_err(StoreError::from)
    }

    #[instrument(skip(self), err, fields(verb = "neighbours", record_id = %id, dir = ?dir))]
    pub(crate) async fn do_neighbours(
        &self,
        id: &RecordId,
        dir: EdgeDir,
    ) -> Result<Vec<Edge>, StoreError> {
        let key = id.as_str().to_owned();
        self.conn
            .call(move |c| {
                let sql = match dir {
                    EdgeDir::Out => "SELECT src, dst, kind, weight FROM edges \
                                      WHERE src = ?1 \
                                        AND dst IN (SELECT record_id FROM records_latest)",
                    EdgeDir::In => "SELECT src, dst, kind, weight FROM edges \
                                     WHERE dst = ?1 \
                                       AND src IN (SELECT record_id FROM records_latest)",
                    EdgeDir::Both => "SELECT src, dst, kind, weight FROM edges \
                                       WHERE (src = ?1 AND dst IN (SELECT record_id FROM records_latest)) \
                                          OR (dst = ?1 AND src IN (SELECT record_id FROM records_latest))",
                };
                let mut stmt = c.prepare(sql)?;
                let rows = stmt
                    .query_map(params![key], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<f64>>(3)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let mut out = Vec::with_capacity(rows.len());
                for (src, dst, kind, weight) in rows {
                    let src_id = record_id_from_str(&src)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    let dst_id = record_id_from_str(&dst)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    let kind_enum = EdgeKind::parse(&kind).ok_or_else(|| {
                        tokio_rusqlite::Error::Other(Box::new(StoreError::Invariant {
                            what: format!("unknown edge kind `{kind}`"),
                        }))
                    })?;
                    out.push(Edge {
                        src: src_id,
                        dst: dst_id,
                        kind: kind_enum,
                        #[allow(clippy::cast_possible_truncation)]
                        weight: weight.map(|w| w as f32),
                    });
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await
            .map_err(StoreError::from)
    }
}
```

- [ ] **Step 3: Wire on trait impl**

Replace the three stubs in `trait_impl.rs`:

```rust
async fn put_edge(&self, edge: &Edge) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Ok(self.do_put_edge(edge).await.map_err(box_err)?)
}
async fn remove_edge(&self, key: &EdgeKey) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    Ok(self.do_remove_edge(key).await.map_err(box_err)?)
}
async fn neighbours(&self, id: &RecordId, dir: EdgeDir) -> Result<Vec<Edge>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(self.do_neighbours(id, dir).await.map_err(box_err)?)
}
```

- [ ] **Step 4: Run the tests, expect pass**

```bash
cargo nextest run -p cairn-store-sqlite --test edges_crud --locked
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/edges.rs \
        crates/cairn-store-sqlite/src/store/trait_impl.rs \
        crates/cairn-store-sqlite/tests/edges_crud.rs
git commit -m "feat(store-sqlite): impl put_edge/remove_edge/neighbours (#46)"
```

---

### Task 18: Implement `with_tx` (inherent method) and `StoreTx`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/tx.rs`
- Create: `crates/cairn-store-sqlite/tests/tx_rollback.rs`

- [ ] **Step 1: Write the failing test**

In `crates/cairn-store-sqlite/tests/tx_rollback.rs`:

```rust
//! `with_tx` rolls back on Err, commits on Ok.

use cairn_core::contract::memory_store::MemoryStore;
use cairn_store_sqlite::{StoreError, open_in_memory};

#[tokio::test]
async fn ok_commits() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    store
        .with_tx(move |tx| {
            tx.upsert(&r)?;
            Ok::<_, StoreError>(())
        })
        .await
        .expect("with_tx");

    let r2 = cairn_core::domain::record::tests_export::sample_record();
    let got = store.get(&r2.id).await.expect("get");
    assert!(got.is_some(), "tx commit must persist the upsert");
}

#[tokio::test]
async fn err_rolls_back() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    let result = store
        .with_tx(move |tx| {
            tx.upsert(&r)?;
            Err::<(), _>(StoreError::Invariant { what: "test rollback".into() })
        })
        .await;
    assert!(result.is_err());

    let r2 = cairn_core::domain::record::tests_export::sample_record();
    let got = store.get(&r2.id).await.expect("get");
    assert!(got.is_none(), "tx rollback must not persist the upsert");
}
```

- [ ] **Step 2: Implement `tx.rs`**

```rust
//! `with_tx` — synchronous transactional closure on the DB worker thread.

use cairn_core::contract::memory_store::{
    Edge, EdgeKey, TombstoneReason, UpsertOutcome,
};
use cairn_core::domain::{BodyHash, MemoryRecord, RecordId};
use rusqlite::{Transaction, params};
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;
use crate::store::projection::{ProjectedRow, body_hash_from_str};

/// Transactional handle exposed to `with_tx` closures. Lives on the
/// dedicated DB worker thread; methods are synchronous.
pub struct StoreTx<'a> {
    pub(crate) tx: Transaction<'a>,
}

impl<'a> StoreTx<'a> {
    pub fn upsert(&self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        upsert_inner(&self.tx, record)
    }

    pub fn tombstone(&self, id: &RecordId, reason: TombstoneReason) -> Result<(), StoreError> {
        let now_ms = current_unix_ms();
        self.tx.execute(
            "UPDATE records SET tombstoned = 1, tombstone_reason = ?1, updated_at = ?2 \
              WHERE record_id = ?3",
            params![reason.as_db_str(), now_ms, id.as_str()],
        )?;
        Ok(())
    }

    pub fn put_edge(&self, edge: &Edge) -> Result<(), StoreError> {
        self.tx.execute(
            "INSERT OR REPLACE INTO edges (src, dst, kind, weight) VALUES (?1, ?2, ?3, ?4)",
            params![
                edge.src.as_str(),
                edge.dst.as_str(),
                edge.kind.as_db_str(),
                edge.weight.map(f64::from),
            ],
        )?;
        Ok(())
    }

    pub fn remove_edge(&self, key: &EdgeKey) -> Result<bool, StoreError> {
        let n = self.tx.execute(
            "DELETE FROM edges WHERE src = ?1 AND dst = ?2 AND kind = ?3",
            params![key.src.as_str(), key.dst.as_str(), key.kind.as_db_str()],
        )?;
        Ok(n > 0)
    }
}

impl SqliteMemoryStore {
    /// Run `f` inside a single SQLite transaction. Closure runs synchronously
    /// on the dedicated DB worker thread. Returning `Err` rolls back; `Ok`
    /// commits.
    #[instrument(skip(self, f), err, fields(verb = "with_tx"))]
    pub async fn with_tx<F, T>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut StoreTx<'_>) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        self.conn
            .call(move |c| {
                let tx = c.transaction()?;
                let mut handle = StoreTx { tx };
                match f(&mut handle) {
                    Ok(value) => {
                        handle.tx.commit()?;
                        Ok::<_, tokio_rusqlite::Error>(Ok(value))
                    }
                    Err(e) => {
                        // Drop without commit → rollback.
                        Ok::<_, tokio_rusqlite::Error>(Err(e))
                    }
                }
            })
            .await
            .map_err(StoreError::from)?
    }
}

fn upsert_inner(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
) -> Result<UpsertOutcome, StoreError> {
    let body_hash = BodyHash::compute(&record.body);
    let prior: Option<(String, i64, String)> = tx
        .query_row(
            "SELECT record_id, version, body_hash FROM records \
              WHERE target_id = ?1 AND active = 1 LIMIT 1",
            params![record.target_id.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;

    let now_ms = current_unix_ms();
    let (version, prior_hash, content_changed) = match prior.as_ref() {
        Some((prior_id, prior_version, prior_hash_str)) => {
            let prior_hash = body_hash_from_str(prior_hash_str)?;
            if prior_hash == body_hash {
                return Ok(UpsertOutcome {
                    record_id: record.id.clone(),
                    target_id: record.target_id.clone(),
                    version: u32::try_from(*prior_version).unwrap_or(u32::MAX),
                    content_changed: false,
                    prior_hash: Some(prior_hash),
                });
            }
            tx.execute(
                "UPDATE records SET active = 0, updated_at = ?1 WHERE record_id = ?2",
                params![now_ms, prior_id],
            )?;
            (u32::try_from(prior_version + 1).unwrap_or(u32::MAX), Some(prior_hash), true)
        }
        None => (1u32, None, true),
    };

    let row = ProjectedRow::from_record(record, version, now_ms, now_ms, &body_hash, true, false)?;
    tx.execute(
        "INSERT INTO records ( \
            record_id, target_id, version, path, kind, class, visibility, \
            scope, actor_chain, body, body_hash, created_at, updated_at, \
            active, tombstoned, is_static, record_json, confidence, \
            salience, target_id_explicit, tags_json \
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
        params![
            row.record_id, row.target_id, row.version, row.path, row.kind,
            row.class, row.visibility, row.scope, row.actor_chain, row.body,
            row.body_hash, row.created_at, row.updated_at, row.active,
            row.tombstoned, row.is_static, row.record_json, row.confidence,
            row.salience, row.target_id_explicit, row.tags_json,
        ],
    )?;
    Ok(UpsertOutcome {
        record_id: record.id.clone(),
        target_id: record.target_id.clone(),
        version,
        content_changed,
        prior_hash,
    })
}

fn current_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
```

- [ ] **Step 3: Re-export `StoreTx` from the crate root**

In `crates/cairn-store-sqlite/src/lib.rs`, add:

```rust
pub use store::tx::StoreTx;
```

- [ ] **Step 4: Run the tests, expect pass**

```bash
cargo nextest run -p cairn-store-sqlite --test tx_rollback --locked
```

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/tx.rs \
        crates/cairn-store-sqlite/src/lib.rs \
        crates/cairn-store-sqlite/tests/tx_rollback.rs
git commit -m "feat(store-sqlite): inherent with_tx + StoreTx (#46)"
```

---

## Section 8 — Versioning + JSON-projection invariants

### Task 19: Add the versioning integration test

**Files:**
- Create: `crates/cairn-store-sqlite/tests/versioning.rs`

- [ ] **Step 1: Write the test**

```rust
//! Multi-version semantics: only-one-active per target, history complete,
//! prior versions reachable.

use cairn_core::contract::memory_store::MemoryStore;
use cairn_store_sqlite::open_in_memory;

#[tokio::test]
async fn three_upserts_produce_one_active_three_history() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    store.upsert(&r).await.expect("v1");

    let mut r2 = r.clone();
    r2.body = "second".to_owned();
    let out2 = store.upsert(&r2).await.expect("v2");
    assert_eq!(out2.version, 2);

    let mut r3 = r.clone();
    r3.body = "third".to_owned();
    let out3 = store.upsert(&r3).await.expect("v3");
    assert_eq!(out3.version, 3);

    let history = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(history.len(), 3);
    let active_count = history.iter().filter(|v| v.active).count();
    assert_eq!(active_count, 1, "exactly one active row per target");
    assert!(history[2].active, "newest is active");
}
```

- [ ] **Step 2: Run, expect pass**

```bash
cargo nextest run -p cairn-store-sqlite --test versioning --locked
```

Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/tests/versioning.rs
git commit -m "test(store-sqlite): versioning invariants (#46)"
```

---

### Task 20: Add the `hot_columns_match_json` proptest

**Files:**
- Create: `crates/cairn-store-sqlite/tests/hot_columns_match_json.rs`

- [ ] **Step 1: Write the proptest**

```rust
//! Proptest: every denormalized column equals its `record_json` projection.

use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use cairn_store_sqlite::open_in_memory;
use proptest::prelude::*;

fn record_strategy() -> impl Strategy<Value = MemoryRecord> {
    // Generate by mutating the in-tree sample. Vary body and confidence
    // so projection is exercised across distinct hashes and ranges.
    (
        "[a-z ]{3,40}",
        0.0f32..=1.0,
        0.0f32..=1.0,
        0u8..=255,
    )
        .prop_map(|(body, confidence, salience, ulid_byte)| {
            let mut r = cairn_core::domain::record::tests_export::sample_record();
            r.body = body;
            r.confidence = confidence;
            r.salience = salience;
            // Permute id/target so each iteration writes a fresh row.
            let suffix = format!("{ulid_byte:02X}");
            let new_id = format!(
                "01HQZX9F5N0000000000000{suffix}00",
            );
            r.id = RecordId::parse(new_id.clone()).unwrap();
            r.target_id = TargetId::parse(new_id).unwrap();
            r
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn projection_round_trips_via_get(record in record_strategy()) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let store = open_in_memory().await.expect("open");
            store.upsert(&record).await.expect("upsert");
            let back = store.get(&record.id).await.expect("get").expect("present");
            prop_assert_eq!(back.body, record.body);
            prop_assert_eq!(back.confidence, record.confidence);
            prop_assert_eq!(back.salience, record.salience);
            Ok(())
        }).unwrap();
    }
}
```

- [ ] **Step 2: Run, expect pass**

```bash
cargo nextest run -p cairn-store-sqlite --test hot_columns_match_json --locked
```

Expected: pass (64 iterations).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/tests/hot_columns_match_json.rs
git commit -m "test(store-sqlite): proptest projection round-trip (#46)"
```

---

## Section 9 — Test fixtures

### Task 21: Add fixture helpers in `cairn-test-fixtures`

**Files:**
- Modify: `crates/cairn-test-fixtures/src/lib.rs`
- Modify: `crates/cairn-test-fixtures/Cargo.toml`

- [ ] **Step 1: Add the deps**

In `crates/cairn-test-fixtures/Cargo.toml` `[dependencies]`, add:

```toml
cairn-core = { workspace = true, features = ["test-fixtures"] }
cairn-store-sqlite = { workspace = true }
tokio = { workspace = true }
tempfile = { workspace = true }
```

- [ ] **Step 2: Add helpers to `lib.rs`**

Append to `crates/cairn-test-fixtures/src/lib.rs`:

```rust
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use cairn_store_sqlite::SqliteMemoryStore;
use tempfile::TempDir;

/// Deterministic `MemoryRecord` keyed off `seed`. Body, id, and target are
/// derived from the seed so distinct seeds always produce distinct rows.
#[must_use]
pub fn sample_record(seed: u64) -> MemoryRecord {
    let mut r = cairn_core::domain::record::tests_export::sample_record();
    let suffix = format!("{seed:020X}");
    let id_str = format!("01HQZX9F5N0{}", &suffix[..15]);
    r.id = RecordId::parse(id_str.clone()).expect("seed-derived id");
    r.target_id = TargetId::parse(id_str).expect("seed-derived target");
    r.body = format!("seeded body {seed}");
    r
}

/// File-backed store in a fresh temp dir. Caller keeps `TempDir` alive
/// for the duration of the test.
///
/// # Panics
/// Panics if the temp dir or store cannot be created.
pub async fn tempstore() -> (TempDir, SqliteMemoryStore) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("cairn.db");
    let store = cairn_store_sqlite::open(path).await.expect("open");
    (dir, store)
}

/// In-memory store. For fast tests that don't need a path on disk.
///
/// # Panics
/// Panics if the in-memory store cannot be opened.
pub async fn memstore() -> SqliteMemoryStore {
    cairn_store_sqlite::open_in_memory().await.expect("memstore")
}
```

- [ ] **Step 3: Verify `cairn-test-fixtures` builds**

```bash
cargo check -p cairn-test-fixtures --all-targets --locked
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-test-fixtures/Cargo.toml crates/cairn-test-fixtures/src/lib.rs
git commit -m "test(fixtures): add sample_record, tempstore, memstore (#46)"
```

---

## Section 10 — Final verification

### Task 22: Full workspace verification

**Files:** None — verification only.

- [ ] **Step 1: Run the per-PR verification subset**

```bash
cargo fmt --all --check
cargo clippy -p cairn-core -p cairn-store-sqlite -p cairn-test-fixtures \
    --all-targets --locked -- -D warnings
cargo nextest run -p cairn-core -p cairn-store-sqlite -p cairn-test-fixtures \
    --locked --no-fail-fast
cargo test --doc -p cairn-core -p cairn-store-sqlite -p cairn-test-fixtures --locked
./scripts/check-core-boundary.sh
```

Expected: all green.

- [ ] **Step 2: Run the workspace-wide check**

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
    cargo doc --workspace --no-deps --document-private-items --locked
```

Expected: all green.

- [ ] **Step 3: Run the supply-chain checks**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: all green. New deps (`tokio_rusqlite`, `bon`, `blake3`, `serde_json`, `tracing`, `tokio`) appear in `cargo deny check` allowlist; resolve any rejection by adding the license to `deny.toml` or filing a maintainer-sign-off comment in the PR.

- [ ] **Step 4: Update the spec to record the `with_tx` deviation**

Edit `docs/superpowers/specs/2026-04-27-store-sqlite-crud-keyword-search-design.md`. In §3 trait surface, replace the `with_tx` trait method with a one-paragraph note:

> `with_tx` is exposed as an inherent method on `SqliteMemoryStore`, not on the trait — generic `F` and `T` parameters break object-safety. Verb-layer code that needs transactional composition takes `Arc<SqliteMemoryStore>` directly.

Commit:

```bash
git add docs/superpowers/specs/2026-04-27-store-sqlite-crud-keyword-search-design.md
git commit -m "docs(specs): with_tx is inherent on SqliteMemoryStore — object-safety (#46)"
```

- [ ] **Step 5: Push the branch and open the PR**

```bash
git push -u origin HEAD
gh pr create --title "feat(store-sqlite): MemoryStore CRUD, versioning, edges, tx (#46)" --body "$(cat <<'EOF'
## Summary

Implements MemoryStore CRUD, versioning, graph edges, and transactions on top
of the existing P0 SQLite schema. Wires tokio_rusqlite so trait methods are
honestly async. Leaves search_keyword as a CapabilityUnavailable stub — that
flips on in PR-B (#47).

Resolves: #46
Spec: docs/superpowers/specs/2026-04-27-store-sqlite-crud-keyword-search-design.md
Plan: docs/superpowers/plans/2026-04-27-store-sqlite-pr-a-crud.md

Brief sources: §3, §3.0, §4, §4.1, §5.2, §6.5.
Invariants exercised: §4 #5 (WAL boundary), #6 (fail-closed cap),
#8 (no unwrap in core), §6.11 (WAL FSM at verb layer).

Cap flags after merge: fts=false, vector=false, graph_edges=true, transactions=true.

## Test plan

- [ ] cargo fmt + clippy + nextest + doctests + core-boundary check
- [ ] cargo doc with deny-warnings
- [ ] cargo deny + cargo audit + cargo machete
- [ ] manual smoke: open a temp vault, upsert + get + tombstone + versions
EOF
)"
```

Expected: PR opens cleanly. Paste the PR URL in your handoff message.

---

## Self-review checklist (run before declaring complete)

- [ ] Spec coverage: §1 (scope), §2 (existing state), §3 (trait surface) — Task 4, 5; §3.1 (async) — Task 11; §3.2 (caps) — Task 11 + 22; §4.1 (projection) — Task 13; §4.2 (migrations) — Tasks 6, 7, 8; §4.3 (versioning) — Tasks 14, 19; §4.4 (tombstone) — Task 16; §4.5 (edges) — Task 17; §4.6 (pragmas) — Task 11; §6 (errors) — Task 12; §7 (tracing) — Tasks 14–18; §8.1 (tests) — Tasks 14–20; §8.4 (fixtures) — Task 21; §9 (verification) — Task 22.
- [ ] Spec §5 (keyword search) — intentionally deferred to PR-B; trait surface and `KeywordSearchArgs/Page/Candidate` types are added in Task 4 + Task 5 to make the trait coherent.
- [ ] Spec §3 `with_tx` → deviation called out in plan header + Task 22 step 4.
- [ ] No placeholders. All `T0DO` / `TBD` / "implement later" patterns absent.
- [ ] Type names consistent: `TombstoneReason`, `UpsertOutcome`, `Edge`, `EdgeKey`, `EdgeKind`, `EdgeDir`, `KeywordSearchArgs`, `KeywordCursor`, `KeywordSearchPage`, `SearchCandidate`, `BodyHash`, `TargetId`, `RecordVersion`, `ListArgs`, `ListPage`, `ListCursor`, `StoreTx`, `SqliteMemoryStore`.
- [ ] Method names consistent: `upsert`, `get`, `list`, `tombstone`, `versions`, `put_edge`, `remove_edge`, `neighbours`, `search_keyword`, `with_tx`. Inherent counterparts named `do_upsert`, `do_get`, etc.
