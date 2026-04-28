# Issue #46 — SQLite MemoryStore CRUD, versioning, and graph edges — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the P0 `MemoryStore` adapter primitives — typed read API gated by `Principal`, copy-on-write versioned writes, graph edges, sync transaction surface gated by a WAL-only `ApplyToken`, and the full P0 SQLite schema (records / edges / FTS5 / WAL ops / replay / consent / locks / jobs).

**Architecture:** `cairn-core` extends the `MemoryStore` trait (read-only public) and adds a sealed `MemoryStoreApply` write trait gated by an `ApplyToken` whose constructor is `pub(in cairn_core::wal)`. `cairn-store-sqlite` implements both over `rusqlite (bundled)` with embedded forward-only migrations. Reads filter rows per-row via rebac (`scope`+`actor_chain`); writes go through one `tokio::task::spawn_blocking` holding one `rusqlite::Transaction` for the whole closure.

**Tech Stack:** Rust 1.95.0, `rusqlite` 0.32 (`bundled`, `serde_json`), `async_trait`, `thiserror`, `serde_json`, `blake3`, `tokio`, `rstest`, `proptest`, `insta`, `trybuild`.

**Source spec:** `docs/superpowers/specs/2026-04-27-issue-46-sqlite-store-design.md` — refer back for design rationale and section-by-section decisions.

---

## File structure

### `cairn-core` (extended)

| File | Role |
|---|---|
| `crates/cairn-core/src/contract/memory_store.rs` | Extend with `get`/`list`/`version_history`; add `Principal`-bearing read API. |
| `crates/cairn-core/src/contract/memory_store/types.rs` *(new)* | `ListQuery`, `ListResult`, `RecordVersion`, `HistoryEntry`, `PurgeMarker`, `RecordEvent`, `ChangeKind`, `Edge`, `EdgeKind`, `ConflictKind`, `ConsentJournalEntry`, `ConsentJournalRowId`, `OpId`, `PurgeOutcome`. |
| `crates/cairn-core/src/contract/memory_store/error.rs` *(new)* | Abstract `StoreError`. |
| `crates/cairn-core/src/contract/memory_store/apply.rs` *(new)* | `ApplyToken`, sealed `MemoryStoreApply` + `MemoryStoreApplyTx` traits. |
| `crates/cairn-core/src/wal/mod.rs` *(new)* | Stub module with `pub(in crate::wal) ApplyToken::new()` constructor + `#[cfg(any(test, feature = "test-util"))] pub fn test_apply_token()`. The executor itself lands in #8. |
| `crates/cairn-core/src/lib.rs` | `pub mod wal;` |

### `cairn-store-sqlite` (real implementation)

| File | Role |
|---|---|
| `crates/cairn-store-sqlite/Cargo.toml` | Add `rusqlite = { version = "0.32", features = ["bundled", "serde_json"] }`, `blake3 = "1"`, `serde_json = { workspace = true }`, `tracing = { workspace = true }`. |
| `crates/cairn-store-sqlite/build.rs` *(new)* | Compute SHA-256 checksums for each migration file at build time; emit `OUT_DIR/migration_checksums.rs`. |
| `crates/cairn-store-sqlite/migrations/0001_init_pragmas.sql` *(new)* | Empty (pragmas applied programmatically — file present so the ledger has a row for "initial open"). |
| `crates/cairn-store-sqlite/migrations/0002_records.sql` *(new)* | `records`, `record_purges` per spec §5.2. |
| `crates/cairn-store-sqlite/migrations/0003_edges.sql` *(new)* | `edges`, `edge_versions`. |
| `crates/cairn-store-sqlite/migrations/0004_fts5.sql` *(new)* | `records_fts` virtual table + sync triggers. |
| `crates/cairn-store-sqlite/migrations/0005_wal_state.sql` *(new)* | `wal_ops`, `wal_steps`. |
| `crates/cairn-store-sqlite/migrations/0006_replay_consent.sql` *(new)* | `replay_ledger`, `issuer_seq`, `challenges`, `consent_journal`. |
| `crates/cairn-store-sqlite/migrations/0007_locks_jobs.sql` *(new)* | `locks`, `reader_fence`, `jobs`. |
| `crates/cairn-store-sqlite/migrations/0008_meta.sql` *(new)* | `schema_migrations`. |
| `crates/cairn-store-sqlite/src/lib.rs` | Replace stub: re-export from new modules; keep `register_plugin!`. |
| `crates/cairn-store-sqlite/src/error.rs` *(new)* | `SqliteStoreError` + `From<SqliteStoreError> for cairn_core::StoreError`. |
| `crates/cairn-store-sqlite/src/schema/mod.rs` *(new)* | `Migration` struct + `&'static [Migration]` list (uses build.rs checksums). |
| `crates/cairn-store-sqlite/src/schema/runner.rs` *(new)* | `apply_pending(conn) -> Result<(), SqliteStoreError>`. |
| `crates/cairn-store-sqlite/src/conn.rs` *(new)* | `open(path) -> Result<SqliteMemoryStore, StoreError>`, pragma setup. |
| `crates/cairn-store-sqlite/src/store.rs` *(new)* | `SqliteMemoryStore` struct, `MemoryStore` (read) impl. |
| `crates/cairn-store-sqlite/src/apply.rs` *(new)* | `MemoryStoreApply` impl + `SqliteMemoryStoreApplyTx<'_>` + `MemoryStoreApplyTx` impl. |
| `crates/cairn-store-sqlite/src/rebac.rs` *(new, small)* | `fn principal_can_read(principal, scope_json, actor_chain_json) -> bool` — rule subset for #46 (visibility tier + scope user/agent match). |
| `crates/cairn-store-sqlite/src/rowmap.rs` *(new)* | Row → `MemoryRecord`/`RecordVersion`/`HistoryEntry` deserialization helpers. |

### Tests

| File | Role |
|---|---|
| `crates/cairn-store-sqlite/tests/crud_roundtrip.rs` | stage→activate→get round-trip. |
| `crates/cairn-store-sqlite/tests/cow_versioning.rs` | Two-version COW; one-active invariant. |
| `crates/cairn-store-sqlite/tests/tombstone_preserves_history.rs` | Lifecycle event immutability. |
| `crates/cairn-store-sqlite/tests/expire_active.rs` | Expire + FTS expiry fence. |
| `crates/cairn-store-sqlite/tests/activate_validates_existence.rs` | NotFound on bogus version. |
| `crates/cairn-store-sqlite/tests/activate_monotonicity.rs` | `expected_prior` CAS + monotonic guard. |
| `crates/cairn-store-sqlite/tests/purge_audit_marker.rs` | Purge + idempotency + audit row. |
| `crates/cairn-store-sqlite/tests/tx_rollback.rs` | Closure Err + panic rollback. |
| `crates/cairn-store-sqlite/tests/edges.rs` | add/remove + backlinks + tombstone non-cascade. |
| `crates/cairn-store-sqlite/tests/migrations.rs` | Apply, re-apply no-op, checksum mismatch abort. |
| `crates/cairn-store-sqlite/tests/capabilities.rs` | Caps flip post-#46. |
| `crates/cairn-store-sqlite/tests/apply_token_gate.rs` + `tests/ui/apply_token_gate.rs` | `trybuild` compile-fail. |
| `crates/cairn-store-sqlite/tests/consent_journal_atomicity.rs` | Same-tx commit + rollback. |
| `crates/cairn-store-sqlite/tests/rebac_visibility.rs` | Per-row drop + hidden count. |
| `crates/cairn-store-sqlite/tests/fts_visibility_predicate.rs` | FTS hides tombstoned/expired. |
| `crates/cairn-test-fixtures/src/store_conformance.rs` *(new)* | Reusable conformance suite (callable by future stores). |

---

## Task ordering (4 commits, 1 PR)

1. **Task 1** — `cairn-core` trait extension (no rusqlite).
2. **Task 2** — `cairn-store-sqlite` migrations + runner + `open()`.
3. **Task 3** — `cairn-store-sqlite` read + apply impl.
4. **Task 4** — Conformance suite + remaining integration/property tests.

Each task is a single commit. Each task is self-contained: tests pass at task end, `cargo nextest run --workspace` is green.

---

## Task 1 — Core trait surface

**Files:**
- Create: `crates/cairn-core/src/contract/memory_store/types.rs`
- Create: `crates/cairn-core/src/contract/memory_store/error.rs`
- Create: `crates/cairn-core/src/contract/memory_store/apply.rs`
- Create: `crates/cairn-core/src/wal/mod.rs`
- Modify: `crates/cairn-core/src/contract/memory_store.rs` → `crates/cairn-core/src/contract/memory_store/mod.rs` (move to module dir)
- Modify: `crates/cairn-core/src/lib.rs` (add `pub mod wal;`)
- Modify: `crates/cairn-core/Cargo.toml` (add `test-util` feature)

### Steps

- [ ] **1.1: Convert `memory_store.rs` to a module directory**

```bash
mkdir -p crates/cairn-core/src/contract/memory_store
git mv crates/cairn-core/src/contract/memory_store.rs crates/cairn-core/src/contract/memory_store/mod.rs
cargo check -p cairn-core
```

Expected: PASS (existing tests still compile because re-export path unchanged).

- [ ] **1.2: Add the test-util feature**

In `crates/cairn-core/Cargo.toml`, append under `[features]`:

```toml
[features]
default = []
test-util = []
```

Run: `cargo check -p cairn-core --features test-util`. Expected: PASS.

- [ ] **1.3: Write the `types.rs` module (no impls — types only)**

Create `crates/cairn-core/src/contract/memory_store/types.rs`:

```rust
//! Shared types for the `MemoryStore` contract surface.
//!
//! These types compose the read-API parameters and return shapes plus
//! the apply-API method arguments. Domain types (`MemoryRecord`,
//! `TargetId`, `RecordId`, `Principal`, `ActorRef`, `Timestamp`,
//! `Scope`) live in `cairn_core::domain`.

use crate::domain::{
    actor_chain::ActorRef,
    identity::Principal,
    record::MemoryRecord,
    timestamp::Timestamp,
};
use serde::{Deserialize, Serialize};

/// Stable logical record identity. Distinct from per-version `RecordId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TargetId(pub String);

impl TargetId {
    #[must_use]
    pub fn new<S: Into<String>>(s: S) -> Self { Self(s.into()) }
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

impl std::fmt::Display for TargetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

/// Per-version record id. Computed `BLAKE3(target_id || '#' || version)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordId(pub String);

impl RecordId {
    #[must_use]
    pub fn from_target_version(target: &TargetId, version: u64) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(target.as_str().as_bytes());
        hasher.update(b"#");
        hasher.update(version.to_string().as_bytes());
        Self(hasher.finalize().to_hex().to_string())
    }
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

impl std::fmt::Display for RecordId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

/// WAL operation id. Ferried through purge/journal flows for idempotency.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OpId(pub String);

impl OpId {
    #[must_use] pub fn new<S: Into<String>>(s: S) -> Self { Self(s.into()) }
    #[must_use] pub fn as_str(&self) -> &str { &self.0 }
}

/// Edge kind. Closed enum at P0; opens with non_exhaustive for forward-compat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EdgeKind {
    Refines,
    Contradicts,
    DerivedFrom,
    SeeAlso,
    Mentions,
}

/// One graph edge between two per-version `RecordId`s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub from: RecordId,
    pub to: RecordId,
    pub kind: EdgeKind,
    pub weight: f32,
    /// Opaque metadata; the store treats it as a JSON value to round-trip.
    pub metadata: serde_json::Value,
}

/// Lifecycle change kind. `Update` covers stage+activate; `Tombstone` /
/// `Expire` / `Purge` correspond to the brief's forget pipeline events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ChangeKind {
    Update,
    Tombstone,
    Expire,
    Purge,
}

/// One immutable lifecycle event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordEvent {
    pub kind: ChangeKind,
    pub at: Timestamp,
    pub actor: Option<ActorRef>,
}

/// One concrete version of a record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordVersion {
    pub record_id: RecordId,
    pub target_id: TargetId,
    pub version: u64,
    pub active: bool,
    pub events: Vec<RecordEvent>,
}

/// Audit marker for a fully-purged target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PurgeMarker {
    pub target_id: TargetId,
    pub op_id: OpId,
    pub event: RecordEvent,
    pub body_hash_salt: String,
}

/// Element of `version_history`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HistoryEntry {
    Version(RecordVersion),
    Purge(PurgeMarker),
}

/// Outcome of `purge_target`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PurgeOutcome {
    Purged,
    AlreadyPurged,
}

/// Range/list query carrying a principal + filters. Visibility/tier are
/// evaluated at the store layer; pre-resolved scope filters narrow at SQL.
#[derive(Debug, Clone)]
pub struct ListQuery {
    pub principal: Principal,
    pub target_prefix: Option<TargetId>,
    pub kind_filter: Option<String>,        // taxonomy kind
    pub max_results: Option<usize>,
    /// Surface tombstoned rows (forensic).
    pub include_tombstoned: bool,
    /// Surface expired rows (forensic).
    pub include_expired: bool,
}

impl ListQuery {
    #[must_use]
    pub fn new(principal: Principal) -> Self {
        Self {
            principal,
            target_prefix: None,
            kind_filter: None,
            max_results: None,
            include_tombstoned: false,
            include_expired: false,
        }
    }
}

/// Result envelope for `list`. `hidden` reports rows the rebac filter
/// dropped — surfaced to clients per brief line 4136.
#[derive(Debug, Clone, PartialEq)]
pub struct ListResult {
    pub rows: Vec<MemoryRecord>,
    pub hidden: usize,
}

/// Append-only consent journal entry. Opaque payload — the store
/// JSON-serializes on insert and round-trips on read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsentJournalEntry {
    pub op_id: OpId,
    pub kind: String,                    // free-form discriminator
    pub target_id: Option<TargetId>,
    pub actor: ActorRef,
    pub payload: serde_json::Value,
    pub at: Timestamp,
}

/// Returned PK of a freshly-written `consent_journal` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentJournalRowId(pub i64);
```

- [ ] **1.4: Compile-check + commit-stage step**

Add `blake3` to workspace deps and `cairn-core`:

```toml
# Cargo.toml workspace.dependencies
blake3 = "1"
```

```toml
# crates/cairn-core/Cargo.toml [dependencies]
blake3 = { workspace = true }
```

Wire the new module into the parent: edit `crates/cairn-core/src/contract/memory_store/mod.rs` and **prepend** before existing content:

```rust
pub mod types;
pub use types::*;
```

Run: `cargo check -p cairn-core --all-features`. Expected: PASS.

- [ ] **1.5: Write `error.rs`**

Create `crates/cairn-core/src/contract/memory_store/error.rs`:

```rust
//! Abstract `MemoryStore` errors. Adapter-specific backends wrap their
//! concrete error type in `StoreError::Backend`.

use super::types::TargetId;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConflictKind {
    /// Caller staged a `(target_id, version)` that already exists.
    VersionAlreadyStaged,
    /// `activate_version`'s `expected_prior` did not match, or the
    /// requested version is not strictly newer than the current active.
    ActivationRaced,
    /// Generic SQLite UNIQUE constraint violation.
    UniqueViolation,
    /// Generic SQLite foreign-key violation.
    ForeignKey,
    /// `purge_target` re-invoked with an `op_id` that already wrote a marker
    /// (returned via `PurgeOutcome::AlreadyPurged`, not as an error — but
    /// reserved here for cases where re-purge predates the marker).
    PurgeRaced,
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("record not found: target_id={0}")]
    NotFound(TargetId),
    #[error("conflict: {kind:?}")]
    Conflict { kind: ConflictKind },
    #[error("invariant violated: {0}")]
    Invariant(&'static str),
    #[error("backend error")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
}
```

Wire it: in `mod.rs` add `pub mod error;` + `pub use error::*;`. Run `cargo check -p cairn-core --all-features`. Expected: PASS.

- [ ] **1.6: Write `apply.rs` (sealed write trait + `ApplyToken`)**

Create `crates/cairn-core/src/contract/memory_store/apply.rs`:

```rust
//! Token-gated apply (write) surface. Only `cairn_core::wal` can
//! construct an `ApplyToken`; non-WAL callers cannot compile against
//! `with_apply_tx`.

use super::error::StoreError;
use super::types::{
    ConsentJournalEntry, ConsentJournalRowId, Edge, EdgeKind, OpId, PurgeOutcome,
    RecordId, TargetId,
};
use crate::domain::{
    actor_chain::ActorRef, record::MemoryRecord, timestamp::Timestamp,
};

/// Witness that the caller is the WAL state-machine executor. Constructable
/// only inside `cairn_core::wal`.
pub struct ApplyToken {
    _private: (),
}

impl ApplyToken {
    /// **Internal.** Only `cairn_core::wal` can call this. The visibility
    /// modifier blocks external construction at compile time.
    pub(in crate::wal) fn __new() -> Self { Self { _private: () } }
}

/// Sealing supertrait — keeps third-party impls out of the apply path.
mod sealed {
    pub trait Sealed {}
}

/// Sync write trait. `with_apply_tx` is the entry point; methods listed
/// here run inside that closure.
#[allow(clippy::module_name_repetitions)]
pub trait MemoryStoreApply: sealed::Sealed + Send + Sync {
    /// Run a synchronous closure inside one rusqlite transaction. Commit
    /// on `Ok`; rollback on `Err` or panic. The token-take prevents
    /// callers without a WAL-issued `ApplyToken` from compiling.
    fn with_apply_tx<F, T>(
        &self,
        token: ApplyToken,
        f: F,
    ) -> Result<T, StoreError>
    where
        F: FnOnce(&mut dyn MemoryStoreApplyTx) -> Result<T, StoreError>
            + Send
            + 'static,
        T: Send + 'static;
}

#[allow(clippy::module_name_repetitions)]
pub trait MemoryStoreApplyTx: sealed::Sealed + Send {
    fn stage_version(&mut self, record: &MemoryRecord) -> Result<RecordId, StoreError>;

    fn activate_version(
        &mut self,
        target_id: &TargetId,
        version: u64,
        expected_prior: Option<u64>,
    ) -> Result<(), StoreError>;

    fn tombstone_target(
        &mut self,
        target_id: &TargetId,
        actor: &ActorRef,
    ) -> Result<(), StoreError>;

    fn expire_active(
        &mut self,
        target_id: &TargetId,
        at: Timestamp,
    ) -> Result<(), StoreError>;

    fn purge_target(
        &mut self,
        target_id: &TargetId,
        op_id: &OpId,
        actor: &ActorRef,
    ) -> Result<PurgeOutcome, StoreError>;

    fn add_edge(&mut self, edge: &Edge) -> Result<(), StoreError>;

    fn remove_edge(
        &mut self,
        from: &RecordId,
        to: &RecordId,
        kind: EdgeKind,
    ) -> Result<(), StoreError>;

    fn append_consent_journal(
        &mut self,
        entry: &ConsentJournalEntry,
    ) -> Result<ConsentJournalRowId, StoreError>;
}

/// Public re-export so adapters in other crates can use the sealing trait
/// (they implement `Sealed` for their own types via this path).
pub use sealed::Sealed;
```

In `mod.rs`, append: `pub mod apply;` + `pub use apply::*;`.

Run: `cargo check -p cairn-core --all-features`. Expected: FAIL — `pub(in crate::wal)` references `crate::wal` which doesn't exist yet. Continue to next step.

- [ ] **1.7: Add the `wal` stub module**

Create `crates/cairn-core/src/wal/mod.rs`:

```rust
//! Stub WAL module. Hosts the `pub(in crate::wal)` constructor for
//! `ApplyToken`; the executor lands in #8.

use crate::contract::memory_store::apply::ApplyToken;

/// **Tests only.** Constructs an apply token without involving the WAL
/// executor. Gated behind the `test-util` feature so non-test builds
/// cannot mint one.
#[cfg(any(test, feature = "test-util"))]
#[must_use]
pub fn test_apply_token() -> ApplyToken { ApplyToken::__new() }
```

In `crates/cairn-core/src/lib.rs`, append: `pub mod wal;`.

Run: `cargo check -p cairn-core --all-features`. Expected: PASS.

- [ ] **1.8: Write the failing read-trait test**

In `crates/cairn-core/src/contract/memory_store/mod.rs`, replace the existing `tests` module with the extended one:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::memory_store::apply::{
        ApplyToken, MemoryStoreApply, MemoryStoreApplyTx, Sealed,
    };
    use crate::contract::memory_store::error::StoreError;
    use crate::contract::memory_store::types::{
        ConsentJournalEntry, ConsentJournalRowId, Edge, EdgeKind, HistoryEntry,
        ListQuery, ListResult, OpId, PurgeOutcome, RecordId, TargetId,
    };
    use crate::contract::version::{ContractVersion, VersionRange};
    use crate::domain::{
        actor_chain::ActorRef, identity::Principal, record::MemoryRecord,
        timestamp::Timestamp,
    };

    struct StubStore;

    #[async_trait::async_trait]
    impl MemoryStore for StubStore {
        fn name(&self) -> &str { "stub" }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: true, vector: false, graph_edges: true, transactions: true,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }
        async fn get(
            &self, _principal: &Principal, _target_id: &TargetId,
        ) -> Result<Option<MemoryRecord>, StoreError> { Ok(None) }
        async fn list(&self, _query: &ListQuery) -> Result<ListResult, StoreError> {
            Ok(ListResult { rows: vec![], hidden: 0 })
        }
        async fn version_history(
            &self, _principal: &Principal, _target_id: &TargetId,
        ) -> Result<Vec<HistoryEntry>, StoreError> { Ok(vec![]) }
    }

    impl Sealed for StubStore {}

    impl MemoryStoreApply for StubStore {
        fn with_apply_tx<F, T>(&self, _: ApplyToken, _f: F) -> Result<T, StoreError>
        where
            F: FnOnce(&mut dyn MemoryStoreApplyTx) -> Result<T, StoreError>
                + Send + 'static,
            T: Send + 'static,
        { Err(StoreError::Invariant("stub: not implemented")) }
    }

    #[test]
    fn dyn_compatible_read() {
        let s: Box<dyn MemoryStore> = Box::new(StubStore);
        assert_eq!(s.name(), "stub");
        assert!(s.capabilities().fts);
    }

    #[test]
    fn dyn_compatible_apply() {
        let s: Box<dyn MemoryStoreApply> = Box::new(StubStore);
        let token = crate::wal::test_apply_token();
        let result = s.with_apply_tx(token, |_tx| Ok::<(), StoreError>(()));
        assert!(matches!(result, Err(StoreError::Invariant(_))));
    }
}
```

Also extend the `MemoryStore` trait body in `mod.rs` to include the three new async methods (read API). The full trait now reads:

```rust
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> &MemoryStoreCapabilities;
    fn supported_contract_versions(&self) -> VersionRange;

    async fn get(
        &self,
        principal: &crate::domain::identity::Principal,
        target_id: &crate::contract::memory_store::types::TargetId,
    ) -> Result<
        Option<crate::domain::record::MemoryRecord>,
        crate::contract::memory_store::error::StoreError,
    >;

    async fn list(
        &self,
        query: &crate::contract::memory_store::types::ListQuery,
    ) -> Result<
        crate::contract::memory_store::types::ListResult,
        crate::contract::memory_store::error::StoreError,
    >;

    async fn version_history(
        &self,
        principal: &crate::domain::identity::Principal,
        target_id: &crate::contract::memory_store::types::TargetId,
    ) -> Result<
        Vec<crate::contract::memory_store::types::HistoryEntry>,
        crate::contract::memory_store::error::StoreError,
    >;
}
```

- [ ] **1.9: Run nextest, expect compilation issues from existing impls**

Run: `cargo nextest run -p cairn-core --all-features --no-fail-fast`.
Expected: existing `cairn-store-sqlite` `SqliteMemoryStore` impl now fails to compile (missing read methods). That's fine — Task 3 fixes it. For now, gate the broken impl: in `crates/cairn-store-sqlite/src/lib.rs`, comment out the `#[async_trait::async_trait] impl MemoryStore for SqliteMemoryStore` block. Replace with a `// TODO #46 Task 3` marker.

Confirm: `cargo nextest run -p cairn-core --all-features` passes.

- [ ] **1.10: Update `Principal` if needed**

Inspect `crates/cairn-core/src/domain/identity.rs`. If `Principal` does not already have a `system()` constructor, add it:

```rust
impl Principal {
    /// Privileged read principal that bypasses rebac. Used by the WAL
    /// executor and tests; never minted by user-facing code paths.
    #[must_use]
    pub fn system() -> Self {
        // Construct the existing `Principal` shape with a sentinel
        // identity. The exact field set depends on the existing struct
        // — match it. See `crates/cairn-core/src/domain/identity.rs`
        // for the canonical definition.
        Self::__system_internal()
    }
}
```

If the construction site needs adjustment per existing field set, modify the body to match. Run `cargo check -p cairn-core --all-features`. Expected: PASS.

- [ ] **1.11: Run all core tests**

Run: `cargo nextest run -p cairn-core --all-features --no-fail-fast`. Expected: PASS (including the two new dyn-compat tests).

- [ ] **1.12: Workspace check**

Run: `cargo check --workspace --all-targets --locked`. Expected: PASS (sqlite stub impl is gated out).

- [ ] **1.13: Commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(core): MemoryStore read trait + apply token + sealed write trait

Extend MemoryStore with Principal-bearing get/list/version_history per
brief lines 2557/3287/4136 (rebac at MemoryStore layer). Add a sealed
MemoryStoreApply / MemoryStoreApplyTx pair gated by ApplyToken whose
constructor is pub(in cairn_core::wal); only the WAL executor (lands in
#8) and the test-util module can mint one. Carries write methods for
stage_version, activate_version (with expected_prior CAS), tombstone_
target, expire_active, purge_target, add_edge, remove_edge, and
append_consent_journal so the executor can commit consent-journal rows
in the same SQLite tx as the state change.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 2 — `cairn-store-sqlite` migrations + runner + `open()`

**Files:**
- Modify: `crates/cairn-store-sqlite/Cargo.toml`
- Create: `crates/cairn-store-sqlite/build.rs`
- Create: `crates/cairn-store-sqlite/migrations/000{1..8}_*.sql`
- Create: `crates/cairn-store-sqlite/src/error.rs`
- Create: `crates/cairn-store-sqlite/src/schema/mod.rs`
- Create: `crates/cairn-store-sqlite/src/schema/runner.rs`
- Create: `crates/cairn-store-sqlite/src/conn.rs`

### Steps

- [ ] **2.1: Add deps**

Edit `crates/cairn-store-sqlite/Cargo.toml`:

```toml
[dependencies]
cairn-core = { workspace = true }
async-trait = { workspace = true }
thiserror = { workspace = true }
rusqlite = { version = "0.32", features = ["bundled", "serde_json"] }
serde_json = { workspace = true }
blake3 = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true, features = ["sync", "rt"] }

[build-dependencies]
sha2 = "0.10"

[dev-dependencies]
cairn-test-fixtures = { workspace = true }
tempfile = "3"
rstest = { workspace = true }
proptest = { workspace = true }
trybuild = { workspace = true }
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

Add to root `Cargo.toml` `[workspace.dependencies]` if missing: `tempfile`, `proptest`, `trybuild`. (They're likely already there from other crates — `cargo tree` to confirm.)

Drop the `package.metadata.cargo-machete.ignored = ["thiserror"]` line — it's now used.

- [ ] **2.2: Write `build.rs`**

Create `crates/cairn-store-sqlite/build.rs`:

```rust
//! Compute SHA-256 checksums for each migration at build time. Emits
//! `migration_checksums.rs` in OUT_DIR with `&[(name, checksum)]` for
//! `schema/mod.rs` to include via `include!`.

use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let migrations_dir = manifest_dir.join("migrations");
    println!("cargo:rerun-if-changed={}", migrations_dir.display());

    let mut entries: Vec<_> = fs::read_dir(&migrations_dir)
        .expect("migrations dir")
        .filter_map(Result::ok)
        .filter(|e| {
            e.path().extension().and_then(|x| x.to_str()) == Some("sql")
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut out = String::from("&[\n");
    for entry in &entries {
        println!("cargo:rerun-if-changed={}", entry.path().display());
        let bytes = fs::read(entry.path()).expect("read migration");
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let hex = format!("{:x}", hasher.finalize());
        let name = entry.file_name().to_string_lossy().into_owned();
        out.push_str(&format!("    (\"{name}\", \"{hex}\"),\n"));
    }
    out.push_str("]");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("migration_checksums.rs"), out).expect("write checksums");
}
```

- [ ] **2.3: Author all 8 migrations**

Create the migration SQL files. Treat `STRICT` tables and `WITHOUT ROWID` only where appropriate.

`crates/cairn-store-sqlite/migrations/0001_init_pragmas.sql` — empty placeholder so the ledger has a row 1:

```sql
-- Migration 0001: pragmas are applied programmatically in conn::open().
-- This file is a placeholder so the migration ledger has a row 1 entry.
SELECT 1;
```

`crates/cairn-store-sqlite/migrations/0002_records.sql`:

```sql
CREATE TABLE records (
  record_id      TEXT NOT NULL PRIMARY KEY,
  target_id      TEXT NOT NULL,
  version        INTEGER NOT NULL,
  active         INTEGER NOT NULL DEFAULT 0,
  tombstoned     INTEGER NOT NULL DEFAULT 0,
  created_at     TEXT NOT NULL,
  created_by     TEXT NOT NULL,
  tombstoned_at  TEXT,
  tombstoned_by  TEXT,
  expired_at     TEXT,
  body           TEXT NOT NULL,
  provenance     TEXT NOT NULL,
  actor_chain    TEXT NOT NULL,
  evidence       TEXT NOT NULL,
  scope          TEXT NOT NULL,
  taxonomy       TEXT NOT NULL,
  confidence     REAL NOT NULL,
  salience       REAL NOT NULL,
  UNIQUE (target_id, version)
) STRICT;

CREATE UNIQUE INDEX records_active_target_idx
  ON records(target_id) WHERE active = 1;

CREATE INDEX records_target_idx ON records(target_id);

CREATE TABLE record_purges (
  target_id        TEXT NOT NULL,
  op_id            TEXT NOT NULL,
  purged_at        TEXT NOT NULL,
  purged_by        TEXT NOT NULL,
  body_hash_salt   TEXT NOT NULL,
  PRIMARY KEY (target_id, op_id)
) STRICT;
```

`crates/cairn-store-sqlite/migrations/0003_edges.sql`:

```sql
CREATE TABLE edges (
  from_id   TEXT NOT NULL,
  to_id     TEXT NOT NULL,
  kind      TEXT NOT NULL,
  weight    REAL NOT NULL,
  metadata  TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (from_id, to_id, kind)
) STRICT;

CREATE INDEX edges_to_idx ON edges(to_id);

CREATE TABLE edge_versions (
  rowid_alias INTEGER PRIMARY KEY AUTOINCREMENT,
  from_id     TEXT NOT NULL,
  to_id       TEXT NOT NULL,
  kind        TEXT NOT NULL,
  weight      REAL,
  metadata    TEXT,
  change_kind TEXT NOT NULL,            -- 'insert' | 'update' | 'remove'
  at          TEXT NOT NULL
);

CREATE INDEX edge_versions_lookup_idx ON edge_versions(from_id, to_id, kind);
```

`crates/cairn-store-sqlite/migrations/0004_fts5.sql`:

```sql
CREATE VIRTUAL TABLE records_fts USING fts5(
  body, title, tags,
  content='',                            -- contentless: rows decoded from records
  tokenize = 'unicode61'
);

-- Row-level sync: triggers add/remove FTS rows in lockstep with records.
CREATE TRIGGER records_fts_ai AFTER INSERT ON records
WHEN NEW.tombstoned = 0
BEGIN
  INSERT INTO records_fts(rowid, body, title, tags)
  VALUES (NEW.rowid, NEW.body, COALESCE(json_extract(NEW.taxonomy, '$.title'), ''),
          COALESCE(json_extract(NEW.taxonomy, '$.tags'), ''));
END;

CREATE TRIGGER records_fts_ad AFTER DELETE ON records
BEGIN
  INSERT INTO records_fts(records_fts, rowid, body, title, tags)
  VALUES ('delete', OLD.rowid, OLD.body, '', '');
END;

CREATE TRIGGER records_fts_au AFTER UPDATE ON records
BEGIN
  INSERT INTO records_fts(records_fts, rowid, body, title, tags)
  VALUES ('delete', OLD.rowid, OLD.body, '', '');
  INSERT INTO records_fts(rowid, body, title, tags)
  VALUES (NEW.rowid, NEW.body, COALESCE(json_extract(NEW.taxonomy, '$.title'), ''),
          COALESCE(json_extract(NEW.taxonomy, '$.tags'), ''));
END;
```

`crates/cairn-store-sqlite/migrations/0005_wal_state.sql`:

```sql
CREATE TABLE wal_ops (
  op_id      TEXT NOT NULL PRIMARY KEY,
  kind       TEXT NOT NULL,
  state      TEXT NOT NULL,
  payload    TEXT NOT NULL,
  pre_image  BLOB,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
) STRICT;

CREATE INDEX wal_ops_state_idx ON wal_ops(state);

CREATE TABLE wal_steps (
  rowid_alias INTEGER PRIMARY KEY AUTOINCREMENT,
  op_id       TEXT NOT NULL,
  step_kind   TEXT NOT NULL,
  state       TEXT NOT NULL,
  payload     TEXT,
  at          TEXT NOT NULL,
  FOREIGN KEY (op_id) REFERENCES wal_ops(op_id) ON DELETE CASCADE
);

CREATE INDEX wal_steps_op_idx ON wal_steps(op_id);
```

`crates/cairn-store-sqlite/migrations/0006_replay_consent.sql`:

```sql
CREATE TABLE replay_ledger (
  nonce      TEXT NOT NULL PRIMARY KEY,
  issuer     TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  expires_at TEXT NOT NULL,
  seen_at    TEXT NOT NULL
) STRICT;

CREATE TABLE issuer_seq (
  issuer    TEXT NOT NULL PRIMARY KEY,
  last_seq  INTEGER NOT NULL,
  updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE challenges (
  challenge_id TEXT NOT NULL PRIMARY KEY,
  issued_at    TEXT NOT NULL,
  expires_at   TEXT NOT NULL,
  consumed_at  TEXT
) STRICT;

CREATE TABLE consent_journal (
  rowid_alias INTEGER PRIMARY KEY AUTOINCREMENT,
  op_id       TEXT NOT NULL,
  kind        TEXT NOT NULL,
  target_id   TEXT,
  actor       TEXT NOT NULL,
  payload     TEXT NOT NULL,
  at          TEXT NOT NULL
);

CREATE INDEX consent_journal_op_idx ON consent_journal(op_id);
CREATE INDEX consent_journal_target_idx ON consent_journal(target_id);
```

`crates/cairn-store-sqlite/migrations/0007_locks_jobs.sql`:

```sql
CREATE TABLE locks (
  scope_kind TEXT NOT NULL,
  scope_key  TEXT NOT NULL,
  op_id      TEXT NOT NULL,
  acquired_at TEXT NOT NULL,
  PRIMARY KEY (scope_kind, scope_key)
) STRICT;

CREATE TABLE reader_fence (
  scope_kind TEXT NOT NULL,
  scope_key  TEXT NOT NULL,
  op_id      TEXT NOT NULL,
  state      TEXT NOT NULL,
  opened_at  TEXT NOT NULL,
  closed_at  TEXT,
  PRIMARY KEY (scope_kind, scope_key)
) STRICT;

CREATE TABLE jobs (
  rowid_alias INTEGER PRIMARY KEY AUTOINCREMENT,
  workflow    TEXT NOT NULL,
  state       TEXT NOT NULL,
  payload     TEXT NOT NULL,
  scheduled_at TEXT NOT NULL,
  started_at   TEXT,
  finished_at  TEXT
);

CREATE INDEX jobs_state_idx ON jobs(state);
CREATE INDEX jobs_workflow_idx ON jobs(workflow);
```

`crates/cairn-store-sqlite/migrations/0008_meta.sql`:

```sql
CREATE TABLE schema_migrations (
  id          INTEGER NOT NULL PRIMARY KEY,
  name        TEXT NOT NULL,
  checksum    TEXT NOT NULL,
  applied_at  TEXT NOT NULL
) STRICT;
```

- [ ] **2.4: Write `error.rs`**

Create `crates/cairn-store-sqlite/src/error.rs`:

```rust
//! `rusqlite`-aware error wrapping. Produces typed `StoreError::Conflict`
//! variants where SQLite returns a recognizable constraint-violation
//! code; otherwise wraps via `Backend`.

use cairn_core::contract::memory_store::error::{ConflictKind, StoreError};
use rusqlite::ErrorCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SqliteStoreError {
    #[error("rusqlite: {0}")]
    Rusqlite(#[from] rusqlite::Error),
    #[error("migration {migration}: {source}")]
    Migration {
        migration: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("migration checksum mismatch for {migration}")]
    ChecksumMismatch { migration: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

impl From<SqliteStoreError> for StoreError {
    fn from(e: SqliteStoreError) -> Self {
        if let SqliteStoreError::Rusqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error { code, .. }, _msg,
        )) = &e
        {
            match code {
                ErrorCode::ConstraintViolation => {
                    return StoreError::Conflict { kind: ConflictKind::UniqueViolation };
                }
                _ => {}
            }
        }
        StoreError::Backend(Box::new(e))
    }
}
```

Run: `cargo check -p cairn-store-sqlite`. Expected: PASS.

- [ ] **2.5: Write `schema/mod.rs`**

Create `crates/cairn-store-sqlite/src/schema/mod.rs`:

```rust
//! Migration ledger. Identical for every build flavor. Checksums are
//! computed at build time by `build.rs` and embedded via `include!`.

pub mod runner;

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub id: u32,
    pub name: &'static str,
    pub sql: &'static str,
    pub checksum: &'static str,
}

const NAMED_CHECKSUMS: &[(&str, &str)] = include!(concat!(
    env!("OUT_DIR"), "/migration_checksums.rs"
));

fn checksum_for(name: &str) -> &'static str {
    NAMED_CHECKSUMS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .expect("migration checksum present (build.rs guarantees)")
}

macro_rules! mig {
    ($id:literal, $name:literal) => {
        Migration {
            id: $id,
            name: $name,
            sql: include_str!(concat!("../../migrations/", $name)),
            checksum: checksum_for($name),
        }
    };
}

pub static MIGRATIONS: &[Migration] = &[
    mig!(1, "0001_init_pragmas.sql"),
    mig!(2, "0002_records.sql"),
    mig!(3, "0003_edges.sql"),
    mig!(4, "0004_fts5.sql"),
    mig!(5, "0005_wal_state.sql"),
    mig!(6, "0006_replay_consent.sql"),
    mig!(7, "0007_locks_jobs.sql"),
    mig!(8, "0008_meta.sql"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_dense_and_ordered() {
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(m.id as usize, i + 1);
        }
    }

    #[test]
    fn checksums_are_64_hex() {
        for m in MIGRATIONS {
            assert_eq!(m.checksum.len(), 64, "{}", m.name);
            assert!(m.checksum.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
```

Note: the `checksum_for` `expect()` is allowed because the parent module is *not* `cairn-core` (the no-`expect` rule applies only to `cairn-core`). The reason describes the build invariant.

- [ ] **2.6: Write `schema/runner.rs`**

Create `crates/cairn-store-sqlite/src/schema/runner.rs`:

```rust
//! Forward-only migration runner. Each migration runs in its own tx;
//! the ledger row is inserted in the same tx.

use super::Migration;
use crate::error::SqliteStoreError;
use rusqlite::{params, Connection, OptionalExtension};

const META_BOOTSTRAP: &str = "
CREATE TABLE IF NOT EXISTS schema_migrations (
  id          INTEGER NOT NULL PRIMARY KEY,
  name        TEXT NOT NULL,
  checksum    TEXT NOT NULL,
  applied_at  TEXT NOT NULL
);
";

pub fn apply_pending(conn: &mut Connection, migrations: &[Migration])
    -> Result<(), SqliteStoreError>
{
    conn.execute_batch(META_BOOTSTRAP)?;

    for m in migrations {
        let existing: Option<String> = conn.query_row(
            "SELECT checksum FROM schema_migrations WHERE id = ?1",
            params![m.id],
            |row| row.get(0),
        ).optional()?;

        if let Some(prev) = existing {
            if prev != m.checksum {
                return Err(SqliteStoreError::ChecksumMismatch {
                    migration: m.name.to_string(),
                });
            }
            continue;
        }

        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)
            .map_err(|e| SqliteStoreError::Migration {
                migration: m.name.to_string(), source: e,
            })?;
        tx.execute(
            "INSERT INTO schema_migrations (id, name, checksum, applied_at) \
             VALUES (?1, ?2, ?3, datetime('now'))",
            params![m.id, m.name, m.checksum],
        )?;
        tx.commit()?;
    }
    Ok(())
}
```

- [ ] **2.7: Write `conn.rs`**

Create `crates/cairn-store-sqlite/src/conn.rs`:

```rust
//! Connection setup: pragmas + migration apply.

use crate::error::SqliteStoreError;
use crate::schema::{runner::apply_pending, MIGRATIONS};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

const PRAGMAS: &str = "
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
PRAGMA synchronous = NORMAL;
";

/// Owned SQLite connection wrapped in a tokio mutex. Sharing handle for
/// `SqliteMemoryStore` and `SqliteMemoryStoreApply`.
pub type SharedConn = Arc<Mutex<Connection>>;

/// Open + migrate. Returns an `Arc<Mutex<Connection>>` ready for use.
/// Sync function — callers wrap in `spawn_blocking` if invoking from
/// async code.
pub fn open_blocking(path: &Path) -> Result<SharedConn, SqliteStoreError> {
    let mut conn = Connection::open(path)?;
    conn.execute_batch(PRAGMAS)?;
    apply_pending(&mut conn, MIGRATIONS)?;
    Ok(Arc::new(Mutex::new(conn)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_creates_schema_migrations() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cairn.db");
        let conn = open_blocking(&path).unwrap();
        let conn = conn.try_lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(count as usize, MIGRATIONS.len());
    }

    #[test]
    fn open_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cairn.db");
        let _first = open_blocking(&path).unwrap();
        drop(_first);
        let _second = open_blocking(&path).unwrap();
        let conn = _second.try_lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(count as usize, MIGRATIONS.len());
    }

    #[test]
    fn pragmas_applied() {
        let dir = tempdir().unwrap();
        let conn = open_blocking(&dir.path().join("cairn.db")).unwrap();
        let conn = conn.try_lock().unwrap();
        let journal: String = conn.query_row(
            "PRAGMA journal_mode", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(journal, "wal");
        let fks: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fks, 1);
    }
}
```

- [ ] **2.8: Wire modules into `lib.rs`**

Edit `crates/cairn-store-sqlite/src/lib.rs`. Replace contents with:

```rust
//! `SQLite` record store for Cairn (P0).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod conn;
pub mod error;
pub mod schema;

use cairn_core::contract::memory_store::{CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

pub const PLUGIN_NAME: &str = "cairn-store-sqlite";
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

/// P0 SQLite store. Read impl in `store.rs` (#46 Task 3); apply impl in
/// `apply.rs` (#46 Task 3).
pub struct SqliteMemoryStore {
    pub(crate) conn: conn::SharedConn,
}

const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

// MemoryStore + MemoryStoreApply impls land in Task 3.

register_plugin!(
    MemoryStore,
    SqliteMemoryStore,
    "cairn-store-sqlite",
    MANIFEST_TOML
);
```

This won't compile yet because `register_plugin!` requires a `MemoryStore` impl. Comment out the `register_plugin!` invocation (with a `// TODO Task 3` marker) for this task, plus the `Default` derive isn't valid here since the struct has a non-`Default` field — leave construction to Task 3.

Run: `cargo check -p cairn-store-sqlite`. Expected: PASS.

- [ ] **2.9: Run unit tests**

Run: `cargo nextest run -p cairn-store-sqlite --no-fail-fast`. Expected: PASS — 3 conn tests + schema tests.

- [ ] **2.10: Write `migrations.rs` integration test**

Create `crates/cairn-store-sqlite/tests/migrations.rs`:

```rust
use cairn_store_sqlite::conn::open_blocking;
use cairn_store_sqlite::error::SqliteStoreError;
use cairn_store_sqlite::schema::MIGRATIONS;
use rusqlite::params;
use tempfile::tempdir;

#[test]
fn apply_on_empty_db() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cairn.db");
    let conn = open_blocking(&path).unwrap();
    let conn = conn.try_lock().unwrap();
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(n as usize, MIGRATIONS.len());
}

#[test]
fn re_apply_is_no_op() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cairn.db");
    {
        let _conn = open_blocking(&path).unwrap();
    }
    let conn = open_blocking(&path).unwrap();
    let conn = conn.try_lock().unwrap();
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(n as usize, MIGRATIONS.len(), "no duplicate ledger rows");
}

#[test]
fn checksum_mismatch_aborts() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cairn.db");
    {
        let conn = open_blocking(&path).unwrap();
        let conn = conn.try_lock().unwrap();
        conn.execute(
            "UPDATE schema_migrations SET checksum = ?1 WHERE id = 2",
            params!["deadbeef"],
        ).unwrap();
    }
    let err = open_blocking(&path).unwrap_err();
    assert!(matches!(err, SqliteStoreError::ChecksumMismatch { .. }));
}

#[test]
fn p0_tables_present() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cairn.db");
    let conn = open_blocking(&path).unwrap();
    let conn = conn.try_lock().unwrap();
    let expected = [
        "records", "record_purges", "edges", "edge_versions",
        "wal_ops", "wal_steps", "replay_ledger", "issuer_seq",
        "challenges", "consent_journal", "locks", "reader_fence",
        "jobs", "schema_migrations",
    ];
    for table in expected {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
            params![table], |r| r.get(0),
        ).unwrap();
        assert_eq!(n, 1, "missing table {table}");
    }
}
```

- [ ] **2.11: Run integration tests**

Run: `cargo nextest run -p cairn-store-sqlite --test migrations`. Expected: PASS (4 tests).

- [ ] **2.12: Workspace check**

Run: `cargo check --workspace --all-targets --locked && cargo clippy --workspace --all-targets --locked -- -D warnings`. Expected: PASS.

- [ ] **2.13: Commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(store): rusqlite, migrations runner, schema_migrations table

Land migrations 0001-0008 covering the P0 schema: records (versioned
COW per brief lines 337-369) + record_purges audit log; edges +
edge_versions; FTS5 records_fts with sync triggers; wal_ops/wal_steps
tables for #8; replay_ledger/issuer_seq/challenges/consent_journal for
#7/#17; locks/reader_fence/jobs for the workflow host; schema_migrations
ledger. build.rs computes per-migration SHA-256 checksums embedded into
the runner. open_blocking applies WAL/FK/busy_timeout/synchronous=NORMAL
pragmas and runs the runner in one shot. The MemoryStore/Apply impls
land in Task 3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 3 — Read + Apply implementation

**Files:**
- Create: `crates/cairn-store-sqlite/src/store.rs` (read impl)
- Create: `crates/cairn-store-sqlite/src/apply.rs` (write impl)
- Create: `crates/cairn-store-sqlite/src/rebac.rs`
- Create: `crates/cairn-store-sqlite/src/rowmap.rs`
- Modify: `crates/cairn-store-sqlite/src/lib.rs` (un-comment `register_plugin!`, add re-exports)

### Steps

- [ ] **3.1: Write `rowmap.rs`** (row → domain conversion helpers)

Create `crates/cairn-store-sqlite/src/rowmap.rs`:

```rust
//! Row → domain mapping helpers. Concentrates JSON column handling so
//! `store.rs` and `apply.rs` stay focused on SQL.

use cairn_core::contract::memory_store::types::{
    ChangeKind, HistoryEntry, OpId, PurgeMarker, RecordEvent, RecordId,
    RecordVersion, TargetId,
};
use cairn_core::contract::memory_store::error::StoreError;
use cairn_core::domain::{
    actor_chain::ActorRef, record::MemoryRecord, timestamp::Timestamp,
};
use rusqlite::Row;
use serde_json::Value;

pub fn row_to_record(row: &Row<'_>) -> rusqlite::Result<MemoryRecord> {
    // Adapt the column ordering to the SELECT statement that calls this.
    // The calling site selects in a stable order matching this fn.
    let body: String = row.get("body")?;
    let provenance: String = row.get("provenance")?;
    let actor_chain: String = row.get("actor_chain")?;
    let evidence: String = row.get("evidence")?;
    let scope: String = row.get("scope")?;
    let taxonomy: String = row.get("taxonomy")?;
    let confidence: f64 = row.get("confidence")?;
    let salience: f64 = row.get("salience")?;
    // Build a serde_json::Value blob then decode into MemoryRecord.
    // This pattern keeps schema evolution localized to MemoryRecord.
    let json = serde_json::json!({
        "body": body,
        "provenance": serde_json::from_str::<Value>(&provenance).unwrap_or(Value::Null),
        "actor_chain": serde_json::from_str::<Value>(&actor_chain).unwrap_or(Value::Null),
        "evidence": serde_json::from_str::<Value>(&evidence).unwrap_or(Value::Null),
        "scope": serde_json::from_str::<Value>(&scope).unwrap_or(Value::Null),
        "taxonomy": serde_json::from_str::<Value>(&taxonomy).unwrap_or(Value::Null),
        "confidence": confidence,
        "salience": salience,
    });
    serde_json::from_value(json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub fn row_to_record_version(row: &Row<'_>) -> rusqlite::Result<RecordVersion> {
    let record_id: String = row.get("record_id")?;
    let target_id: String = row.get("target_id")?;
    let version: i64 = row.get("version")?;
    let active: i64 = row.get("active")?;
    let tombstoned: i64 = row.get("tombstoned")?;
    let created_at: String = row.get("created_at")?;
    let created_by: String = row.get("created_by")?;
    let tombstoned_at: Option<String> = row.get("tombstoned_at")?;
    let tombstoned_by: Option<String> = row.get("tombstoned_by")?;
    let expired_at: Option<String> = row.get("expired_at")?;

    let mut events = vec![RecordEvent {
        kind: ChangeKind::Update,
        at: Timestamp::parse_iso8601(&created_at).unwrap_or_default(),
        actor: Some(ActorRef::from_string(&created_by)),
    }];
    if let (Some(at), Some(by)) = (tombstoned_at, tombstoned_by) {
        events.push(RecordEvent {
            kind: ChangeKind::Tombstone,
            at: Timestamp::parse_iso8601(&at).unwrap_or_default(),
            actor: Some(ActorRef::from_string(&by)),
        });
    }
    if let Some(at) = expired_at {
        if active == 1 {
            events.push(RecordEvent {
                kind: ChangeKind::Expire,
                at: Timestamp::parse_iso8601(&at).unwrap_or_default(),
                actor: None,
            });
        }
    }
    let _ = tombstoned;  // bool not surfaced directly; events are the truth.

    Ok(RecordVersion {
        record_id: RecordId(record_id),
        target_id: TargetId(target_id),
        version: version as u64,
        active: active == 1,
        events,
    })
}

pub fn row_to_purge_marker(row: &Row<'_>) -> rusqlite::Result<PurgeMarker> {
    let target_id: String = row.get("target_id")?;
    let op_id: String = row.get("op_id")?;
    let purged_at: String = row.get("purged_at")?;
    let purged_by: String = row.get("purged_by")?;
    let body_hash_salt: String = row.get("body_hash_salt")?;
    Ok(PurgeMarker {
        target_id: TargetId(target_id),
        op_id: OpId(op_id),
        event: RecordEvent {
            kind: ChangeKind::Purge,
            at: Timestamp::parse_iso8601(&purged_at).unwrap_or_default(),
            actor: Some(ActorRef::from_string(&purged_by)),
        },
        body_hash_salt,
    })
}

pub fn into_history(versions: Vec<RecordVersion>, purges: Vec<PurgeMarker>) -> Vec<HistoryEntry> {
    let mut out: Vec<HistoryEntry> = versions.into_iter().map(HistoryEntry::Version).collect();
    out.extend(purges.into_iter().map(HistoryEntry::Purge));
    out
}

pub fn store_err(e: rusqlite::Error) -> StoreError {
    StoreError::Backend(Box::new(crate::error::SqliteStoreError::Rusqlite(e)))
}
```

Note: `Timestamp::parse_iso8601` and `ActorRef::from_string` may need to be added to `cairn-core::domain` if they don't exist. If they don't, add minimal helpers there:

```rust
// In cairn_core::domain::timestamp
impl Timestamp {
    pub fn parse_iso8601(s: &str) -> Option<Self> {
        // delegate to existing parser, e.g. via chrono or jiff already in domain
        Self::from_str(s).ok()
    }
}

// In cairn_core::domain::actor_chain
impl ActorRef {
    pub fn from_string(s: &str) -> Self {
        Self::new(s.to_string())
    }
    pub fn as_string(&self) -> &str {
        self.as_str()
    }
}
```

Adapt to the existing `Timestamp`/`ActorRef` API; do not invent new field names.

- [ ] **3.2: Write `rebac.rs`**

Create `crates/cairn-store-sqlite/src/rebac.rs`:

```rust
//! Subset of the rebac decision used by `MemoryStore` reads in #46.
//! The full rule set lives in `cairn-core::rebac` (separate issue).

use cairn_core::domain::identity::Principal;
use serde_json::Value;

/// Returns `true` if the principal is allowed to read a row whose
/// `scope` JSON and `actor_chain` JSON are supplied. System principals
/// always pass.
#[must_use]
pub fn principal_can_read(
    principal: &Principal,
    scope_json: &str,
    actor_chain_json: &str,
) -> bool {
    if principal.is_system() {
        return true;
    }
    let scope: Value = serde_json::from_str(scope_json).unwrap_or(Value::Null);
    let _chain: Value = serde_json::from_str(actor_chain_json).unwrap_or(Value::Null);

    // Visibility tier check.
    let visibility = scope.get("visibility").and_then(Value::as_str).unwrap_or("private");
    match visibility {
        "private" => {
            // Only the row's owner may read.
            let owner = scope.get("user").and_then(Value::as_str).unwrap_or("");
            principal.user_id() == owner
        }
        "session" => {
            // Same session id.
            let row_session = scope.get("session").and_then(Value::as_str).unwrap_or("");
            principal.session_id() == Some(row_session)
        }
        "team" | "org" | "public" => {
            // P0 single-author vault: anyone in the vault sees these.
            true
        }
        _ => false,
    }
}
```

Adapt method names (`Principal::is_system`, `user_id`, `session_id`) to the actual `Principal` API in `cairn-core::domain::identity`. Add the `is_system` accessor there if missing:

```rust
// In Principal
pub fn is_system(&self) -> bool {
    matches!(self.kind, PrincipalKind::System)
}
```

- [ ] **3.3: Write `store.rs` (read impl)**

Create `crates/cairn-store-sqlite/src/store.rs`:

```rust
//! `MemoryStore` (read) implementation.

use crate::conn::SharedConn;
use crate::rebac::principal_can_read;
use crate::rowmap::{into_history, row_to_purge_marker, row_to_record, row_to_record_version, store_err};
use async_trait::async_trait;
use cairn_core::contract::memory_store::{
    error::StoreError,
    types::{HistoryEntry, ListQuery, ListResult, RecordVersion, TargetId, PurgeMarker},
    MemoryStore, MemoryStoreCapabilities,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::{identity::Principal, record::MemoryRecord};
use rusqlite::params;

pub(crate) const SELECT_RECORD_COLS: &str = "
record_id, target_id, version, active, tombstoned,
created_at, created_by, tombstoned_at, tombstoned_by, expired_at,
body, provenance, actor_chain, evidence, scope, taxonomy,
confidence, salience
";

pub struct SqliteMemoryStoreInner;

#[async_trait]
impl MemoryStore for crate::SqliteMemoryStore {
    fn name(&self) -> &str { crate::PLUGIN_NAME }
    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: true, vector: false, graph_edges: true, transactions: true,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    async fn get(
        &self, principal: &Principal, target_id: &TargetId,
    ) -> Result<Option<MemoryRecord>, StoreError> {
        let conn = self.conn.clone();
        let principal = principal.clone();
        let target_id = target_id.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {SELECT_RECORD_COLS} FROM records \
                 WHERE target_id = ?1 AND active = 1 \
                   AND tombstoned = 0 \
                   AND (expired_at IS NULL OR expired_at > datetime('now'))"
            )).map_err(store_err)?;
            let mut rows = stmt.query(params![target_id.as_str()])
                .map_err(store_err)?;
            if let Some(row) = rows.next().map_err(store_err)? {
                let scope: String = row.get("scope").map_err(store_err)?;
                let chain: String = row.get("actor_chain").map_err(store_err)?;
                if !principal_can_read(&principal, &scope, &chain) {
                    return Ok(None);
                }
                let rec = row_to_record(row).map_err(store_err)?;
                Ok(Some(rec))
            } else {
                Ok(None)
            }
        }).await.map_err(|e| StoreError::Backend(Box::new(e)))?
    }

    async fn list(&self, query: &ListQuery) -> Result<ListResult, StoreError> {
        let conn = self.conn.clone();
        let q = query.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut sql = format!(
                "SELECT {SELECT_RECORD_COLS} FROM records WHERE 1 = 1"
            );
            if !q.include_tombstoned { sql.push_str(" AND tombstoned = 0"); }
            if !q.include_expired {
                sql.push_str(" AND (expired_at IS NULL OR expired_at > datetime('now'))");
            }
            sql.push_str(" AND active = 1");
            // (target_prefix / kind_filter wired here via additional params if Some)
            sql.push_str(" ORDER BY target_id, version");
            if let Some(limit) = q.max_results {
                sql.push_str(&format!(" LIMIT {limit}"));
            }

            let mut stmt = conn.prepare(&sql).map_err(store_err)?;
            let mut rows = stmt.query([]).map_err(store_err)?;

            let mut out = Vec::new();
            let mut hidden = 0usize;
            while let Some(row) = rows.next().map_err(store_err)? {
                let scope: String = row.get("scope").map_err(store_err)?;
                let chain: String = row.get("actor_chain").map_err(store_err)?;
                if !principal_can_read(&q.principal, &scope, &chain) {
                    hidden += 1;
                    continue;
                }
                out.push(row_to_record(row).map_err(store_err)?);
            }
            Ok(ListResult { rows: out, hidden })
        }).await.map_err(|e| StoreError::Backend(Box::new(e)))?
    }

    async fn version_history(
        &self, principal: &Principal, target_id: &TargetId,
    ) -> Result<Vec<HistoryEntry>, StoreError> {
        let conn = self.conn.clone();
        let principal = principal.clone();
        let target_id = target_id.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn.prepare(&format!(
                "SELECT {SELECT_RECORD_COLS} FROM records \
                 WHERE target_id = ?1 ORDER BY version ASC"
            )).map_err(store_err)?;
            let versions: Vec<RecordVersion> = stmt
                .query_map(params![target_id.as_str()], |row| row_to_record_version(row))
                .map_err(store_err)?
                .collect::<Result<_, _>>().map_err(store_err)?;

            // Visibility filter: drop versions the principal cannot read.
            let visible: Vec<RecordVersion> = versions
                .into_iter()
                .filter(|v| {
                    // Re-fetch scope/chain for the visibility check.
                    let scope: String = conn.query_row(
                        "SELECT scope FROM records WHERE record_id = ?1",
                        params![v.record_id.as_str()], |r| r.get(0),
                    ).unwrap_or_default();
                    let chain: String = conn.query_row(
                        "SELECT actor_chain FROM records WHERE record_id = ?1",
                        params![v.record_id.as_str()], |r| r.get(0),
                    ).unwrap_or_default();
                    principal_can_read(&principal, &scope, &chain)
                })
                .collect();

            // Purge markers — surface to system principal only.
            let purges: Vec<PurgeMarker> = if principal.is_system() {
                let mut p = conn.prepare(
                    "SELECT target_id, op_id, purged_at, purged_by, body_hash_salt \
                     FROM record_purges WHERE target_id = ?1 ORDER BY purged_at",
                ).map_err(store_err)?;
                p.query_map(params![target_id.as_str()], |row| row_to_purge_marker(row))
                    .map_err(store_err)?
                    .collect::<Result<_, _>>().map_err(store_err)?
            } else {
                vec![]
            };

            Ok(into_history(visible, purges))
        }).await.map_err(|e| StoreError::Backend(Box::new(e)))?
    }
}
```

- [ ] **3.4: Write `apply.rs` (write impl)**

Create `crates/cairn-store-sqlite/src/apply.rs`:

```rust
//! `MemoryStoreApply` + `MemoryStoreApplyTx` implementation.

use crate::conn::SharedConn;
use crate::rowmap::store_err;
use cairn_core::contract::memory_store::{
    apply::{ApplyToken, MemoryStoreApply, MemoryStoreApplyTx, Sealed},
    error::{ConflictKind, StoreError},
    types::{
        ConsentJournalEntry, ConsentJournalRowId, Edge, EdgeKind, OpId,
        PurgeOutcome, RecordId, TargetId,
    },
};
use cairn_core::domain::{
    actor_chain::ActorRef, record::MemoryRecord, timestamp::Timestamp,
};
use rusqlite::{params, Transaction};

impl Sealed for crate::SqliteMemoryStore {}

impl MemoryStoreApply for crate::SqliteMemoryStore {
    fn with_apply_tx<F, T>(
        &self, _token: ApplyToken, f: F,
    ) -> Result<T, StoreError>
    where
        F: FnOnce(&mut dyn MemoryStoreApplyTx) -> Result<T, StoreError>
            + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        // Single spawn_blocking holding the connection for the entire
        // closure. Tokio runtime hop is at the boundaries only.
        let handle = tokio::task::spawn_blocking(move || {
            let mut conn = conn.blocking_lock();
            let tx = conn.transaction().map_err(store_err)?;
            let mut wrap = SqliteMemoryStoreApplyTx { tx: &tx };
            let outcome = f(&mut wrap as &mut dyn MemoryStoreApplyTx);
            match outcome {
                Ok(v) => { tx.commit().map_err(store_err)?; Ok(v) }
                Err(e) => { let _ = tx.rollback(); Err(e) }
            }
        });
        // Sync wrapper: the impl is sync per trait signature. Block with
        // the runtime handle. CALLERS MUST INVOKE FROM ASYNC CONTEXT —
        // the WAL executor in #8 awaits this; #46 tests use
        // `tokio::runtime::Runtime::block_on` from sync test fns, OR
        // are themselves `#[tokio::test]` and call this through the
        // executor's wrapper. We do not call `block_on` inside async
        // here.
        futures::executor::block_on(handle)
            .map_err(|e| StoreError::Backend(Box::new(e)))?
    }
}

struct SqliteMemoryStoreApplyTx<'a> {
    tx: &'a Transaction<'a>,
}

impl<'a> Sealed for SqliteMemoryStoreApplyTx<'a> {}

impl<'a> MemoryStoreApplyTx for SqliteMemoryStoreApplyTx<'a> {
    fn stage_version(&mut self, record: &MemoryRecord) -> Result<RecordId, StoreError> {
        let target_id = record.target_id().clone();
        // Compute next version: max(existing) + 1, or 1.
        let next: i64 = self.tx.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM records WHERE target_id = ?1",
            params![target_id.as_str()], |r| r.get(0),
        ).map_err(store_err)?;
        let version = next as u64;
        let record_id = RecordId::from_target_version(&target_id, version);
        let body = record.body().to_string();
        let provenance = serde_json::to_string(record.provenance())?;
        let actor_chain = serde_json::to_string(record.actor_chain())?;
        let evidence = serde_json::to_string(record.evidence())?;
        let scope = serde_json::to_string(record.scope())?;
        let taxonomy = serde_json::to_string(record.taxonomy())?;

        self.tx.execute(
            "INSERT INTO records ( \
                record_id, target_id, version, active, tombstoned, \
                created_at, created_by, body, provenance, actor_chain, \
                evidence, scope, taxonomy, confidence, salience \
             ) VALUES (?1, ?2, ?3, 0, 0, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                record_id.as_str(),
                target_id.as_str(),
                version as i64,
                Timestamp::now().to_iso8601(),
                record.created_by().as_str(),
                body,
                provenance,
                actor_chain,
                evidence,
                scope,
                taxonomy,
                record.confidence(),
                record.salience(),
            ],
        ).map_err(|e| {
            // UNIQUE on (target_id, version) → VersionAlreadyStaged.
            if let rusqlite::Error::SqliteFailure(rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation, ..
            }, _) = &e {
                StoreError::Conflict { kind: ConflictKind::VersionAlreadyStaged }
            } else {
                store_err(e)
            }
        })?;
        Ok(record_id)
    }

    fn activate_version(
        &mut self,
        target_id: &TargetId,
        version: u64,
        expected_prior: Option<u64>,
    ) -> Result<(), StoreError> {
        // Step 1: confirm row exists.
        let exists: i64 = self.tx.query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND version = ?2",
            params![target_id.as_str(), version as i64], |r| r.get(0),
        ).map_err(store_err)?;
        if exists == 0 {
            return Err(StoreError::NotFound(target_id.clone()));
        }
        // Step 2: monotonicity guard.
        let current: Option<i64> = self.tx.query_row(
            "SELECT version FROM records WHERE target_id = ?1 AND active = 1",
            params![target_id.as_str()], |r| r.get(0),
        ).ok();
        if let Some(cur) = current {
            if let Some(expected) = expected_prior {
                if cur as u64 != expected {
                    return Err(StoreError::Conflict { kind: ConflictKind::ActivationRaced });
                }
            }
            if (cur as u64) >= version {
                return Err(StoreError::Conflict { kind: ConflictKind::ActivationRaced });
            }
        } else if expected_prior.is_some() {
            return Err(StoreError::Conflict { kind: ConflictKind::ActivationRaced });
        }
        // Step 3: flip flags.
        self.tx.execute(
            "UPDATE records SET active = (version = ?2) WHERE target_id = ?1",
            params![target_id.as_str(), version as i64],
        ).map_err(store_err)?;
        // Step 4: post-condition.
        let active: i64 = self.tx.query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND active = 1",
            params![target_id.as_str()], |r| r.get(0),
        ).map_err(store_err)?;
        if active != 1 {
            return Err(StoreError::Invariant(
                "activate_version: post-update active count != 1",
            ));
        }
        Ok(())
    }

    fn tombstone_target(
        &mut self,
        target_id: &TargetId,
        actor: &ActorRef,
    ) -> Result<(), StoreError> {
        let now = Timestamp::now().to_iso8601();
        self.tx.execute(
            "UPDATE records SET tombstoned = 1, \
                tombstoned_at = COALESCE(tombstoned_at, ?2), \
                tombstoned_by = COALESCE(tombstoned_by, ?3) \
             WHERE target_id = ?1",
            params![target_id.as_str(), now, actor.as_str()],
        ).map_err(store_err)?;
        Ok(())
    }

    fn expire_active(
        &mut self, target_id: &TargetId, at: Timestamp,
    ) -> Result<(), StoreError> {
        self.tx.execute(
            "UPDATE records SET expired_at = COALESCE(expired_at, ?2) \
             WHERE target_id = ?1 AND active = 1",
            params![target_id.as_str(), at.to_iso8601()],
        ).map_err(store_err)?;
        Ok(())
    }

    fn purge_target(
        &mut self, target_id: &TargetId, op_id: &OpId, actor: &ActorRef,
    ) -> Result<PurgeOutcome, StoreError> {
        // Idempotency check.
        let existing: i64 = self.tx.query_row(
            "SELECT COUNT(*) FROM record_purges WHERE target_id = ?1 AND op_id = ?2",
            params![target_id.as_str(), op_id.as_str()], |r| r.get(0),
        ).map_err(store_err)?;
        if existing > 0 {
            return Ok(PurgeOutcome::AlreadyPurged);
        }
        // Capture record_id keyset before any DELETE.
        let mut stmt = self.tx.prepare(
            "SELECT record_id FROM records WHERE target_id = ?1",
        ).map_err(store_err)?;
        let record_ids: Vec<String> = stmt.query_map(
            params![target_id.as_str()], |r| r.get(0),
        ).map_err(store_err)?.collect::<Result<_, _>>().map_err(store_err)?;
        drop(stmt);

        // Write audit marker.
        let now = Timestamp::now().to_iso8601();
        let salt = format!("{:x}", rand::random::<u128>());
        self.tx.execute(
            "INSERT INTO record_purges (target_id, op_id, purged_at, purged_by, body_hash_salt) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![target_id.as_str(), op_id.as_str(), now, actor.as_str(), salt],
        ).map_err(store_err)?;

        // Drain edges + edge_versions referencing any per-version record_id.
        for rid in &record_ids {
            self.tx.execute(
                "DELETE FROM edges WHERE from_id = ?1 OR to_id = ?1",
                params![rid],
            ).map_err(store_err)?;
            self.tx.execute(
                "DELETE FROM edge_versions WHERE from_id = ?1 OR to_id = ?1",
                params![rid],
            ).map_err(store_err)?;
        }

        // FTS rows are removed automatically by the AFTER DELETE trigger
        // when we delete records below.

        // Delete records last.
        self.tx.execute(
            "DELETE FROM records WHERE target_id = ?1",
            params![target_id.as_str()],
        ).map_err(store_err)?;

        Ok(PurgeOutcome::Purged)
    }

    fn add_edge(&mut self, edge: &Edge) -> Result<(), StoreError> {
        let metadata = serde_json::to_string(&edge.metadata)?;
        let now = Timestamp::now().to_iso8601();
        // Capture prior for edge_versions on update.
        let prior: Option<(f64, String)> = self.tx.query_row(
            "SELECT weight, metadata FROM edges \
             WHERE from_id = ?1 AND to_id = ?2 AND kind = ?3",
            params![edge.from.as_str(), edge.to.as_str(), edge_kind_str(edge.kind)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).ok();
        if let Some((w, m)) = prior {
            self.tx.execute(
                "INSERT INTO edge_versions (from_id, to_id, kind, weight, metadata, change_kind, at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'update', ?6)",
                params![edge.from.as_str(), edge.to.as_str(), edge_kind_str(edge.kind), w, m, now],
            ).map_err(store_err)?;
        } else {
            self.tx.execute(
                "INSERT INTO edge_versions (from_id, to_id, kind, change_kind, at) \
                 VALUES (?1, ?2, ?3, 'insert', ?4)",
                params![edge.from.as_str(), edge.to.as_str(), edge_kind_str(edge.kind), now],
            ).map_err(store_err)?;
        }
        self.tx.execute(
            "INSERT INTO edges (from_id, to_id, kind, weight, metadata, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(from_id, to_id, kind) DO UPDATE SET \
                weight = excluded.weight, metadata = excluded.metadata",
            params![
                edge.from.as_str(), edge.to.as_str(), edge_kind_str(edge.kind),
                edge.weight as f64, metadata, now,
            ],
        ).map_err(store_err)?;
        Ok(())
    }

    fn remove_edge(
        &mut self, from: &RecordId, to: &RecordId, kind: EdgeKind,
    ) -> Result<(), StoreError> {
        let now = Timestamp::now().to_iso8601();
        let prior: Option<(f64, String)> = self.tx.query_row(
            "SELECT weight, metadata FROM edges \
             WHERE from_id = ?1 AND to_id = ?2 AND kind = ?3",
            params![from.as_str(), to.as_str(), edge_kind_str(kind)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).ok();
        if let Some((w, m)) = prior {
            self.tx.execute(
                "INSERT INTO edge_versions (from_id, to_id, kind, weight, metadata, change_kind, at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'remove', ?6)",
                params![from.as_str(), to.as_str(), edge_kind_str(kind), w, m, now],
            ).map_err(store_err)?;
        }
        self.tx.execute(
            "DELETE FROM edges WHERE from_id = ?1 AND to_id = ?2 AND kind = ?3",
            params![from.as_str(), to.as_str(), edge_kind_str(kind)],
        ).map_err(store_err)?;
        Ok(())
    }

    fn append_consent_journal(
        &mut self, entry: &ConsentJournalEntry,
    ) -> Result<ConsentJournalRowId, StoreError> {
        let payload = serde_json::to_string(&entry.payload)?;
        self.tx.execute(
            "INSERT INTO consent_journal (op_id, kind, target_id, actor, payload, at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.op_id.as_str(),
                entry.kind,
                entry.target_id.as_ref().map(|t| t.as_str()),
                entry.actor.as_str(),
                payload,
                entry.at.to_iso8601(),
            ],
        ).map_err(store_err)?;
        let id = self.tx.last_insert_rowid();
        Ok(ConsentJournalRowId(id))
    }
}

fn edge_kind_str(k: EdgeKind) -> &'static str {
    match k {
        EdgeKind::Refines => "refines",
        EdgeKind::Contradicts => "contradicts",
        EdgeKind::DerivedFrom => "derived_from",
        EdgeKind::SeeAlso => "see_also",
        EdgeKind::Mentions => "mentions",
    }
}
```

Note about `futures::executor::block_on`: that's gated by CLAUDE.md §6.3 ("No `block_on` inside async. No mixing `futures::executor` with tokio."). The trait signature requires sync return, so the alternative is to make `with_apply_tx` itself `async`. **Adapt: make `with_apply_tx` an `async fn` in the trait** (match the read API) and `await` the join. Update `apply.rs` in `cairn-core`:

```rust
pub trait MemoryStoreApply: sealed::Sealed + Send + Sync {
    fn with_apply_tx<'a, F, T>(
        &'a self,
        token: ApplyToken,
        f: F,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, StoreError>> + Send + 'a>>
    where
        F: FnOnce(&mut dyn MemoryStoreApplyTx) -> Result<T, StoreError>
            + Send + 'static,
        T: Send + 'static;
}
```

Then in the SQLite impl, return `Box::pin(async move { handle.await... })`. Update Task 1 step 1.6 + Task 3 step 3.4 accordingly. (Mention this in the commit body — the trait shape is the same, just async-returning.)

If the user prefers cleaner ergonomics, an alternative is `#[async_trait::async_trait]` on `MemoryStoreApply` (consistent with `MemoryStore`). That's the simpler path — adopt it: make `with_apply_tx` an `async fn` under `#[async_trait::async_trait]`. Update both `apply.rs` files.

Adopt `async_trait` on `MemoryStoreApply` for consistency:

```rust
#[async_trait::async_trait]
pub trait MemoryStoreApply: sealed::Sealed + Send + Sync {
    async fn with_apply_tx<F, T>(
        &self,
        token: ApplyToken,
        f: F,
    ) -> Result<T, StoreError>
    where
        F: FnOnce(&mut dyn MemoryStoreApplyTx) -> Result<T, StoreError>
            + Send + 'static,
        T: Send + 'static;
}
```

And in the impl, replace the `futures::executor::block_on` call with `handle.await`.

- [ ] **3.5: Wire impls into `lib.rs`**

Edit `crates/cairn-store-sqlite/src/lib.rs`:

```rust
//! `SQLite` record store for Cairn (P0).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod apply;
pub mod conn;
pub mod error;
pub mod rebac;
pub mod rowmap;
pub mod schema;
pub mod store;

use cairn_core::contract::memory_store::{CONTRACT_VERSION, MemoryStoreCapabilities};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

pub const PLUGIN_NAME: &str = "cairn-store-sqlite";
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

pub struct SqliteMemoryStore {
    pub(crate) conn: conn::SharedConn,
}

impl SqliteMemoryStore {
    /// Open or create `.cairn/cairn.db` and apply migrations.
    pub async fn open(path: &std::path::Path) -> Result<Self, error::SqliteStoreError> {
        let path = path.to_owned();
        let shared = tokio::task::spawn_blocking(move || conn::open_blocking(&path))
            .await
            .map_err(|e| error::SqliteStoreError::Io(std::io::Error::other(e)))??;
        Ok(Self { conn: shared })
    }
}

const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

register_plugin!(
    cairn_core::contract::memory_store::MemoryStore,
    SqliteMemoryStore,
    "cairn-store-sqlite",
    MANIFEST_TOML
);
```

Note: `register_plugin!` requires a `Default::default()` for the registered type. `SqliteMemoryStore` has a non-`Default` field. Inspect the macro and adjust either:

- (a) Provide a feature-gated `Default` that opens an in-memory `sqlite::memory:` DB for tests/registry probes;
- (b) Pass a constructor in the macro invocation per the macro's contract.

Inspect the existing `register_plugin!` API in `crates/cairn-core/src/contract/macros.rs` and adapt. If the macro takes a constructor closure, use `|| SqliteMemoryStore::default_for_registry()`. Otherwise, add a `Default` impl that opens an in-memory connection:

```rust
impl Default for SqliteMemoryStore {
    fn default() -> Self {
        let conn = conn::open_blocking(std::path::Path::new(":memory:"))
            .expect("default in-memory open never fails for capability probes");
        Self { conn }
    }
}
```

The `expect` is allowed in `cairn-store-sqlite` (the `unwrap_used` deny is non-test). Justify the reason inline.

- [ ] **3.6: First end-to-end smoke test**

Create `crates/cairn-store-sqlite/tests/crud_roundtrip.rs`:

```rust
use cairn_core::contract::memory_store::{
    apply::MemoryStoreApply,
    types::{ListQuery, TargetId},
    MemoryStore,
};
use cairn_core::domain::identity::Principal;
use cairn_core::wal::test_apply_token;
use cairn_store_sqlite::SqliteMemoryStore;
use cairn_test_fixtures::record::sample_private_record;
use tempfile::tempdir;

#[tokio::test]
async fn stage_activate_get_roundtrip() {
    let dir = tempdir().unwrap();
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db")).await.unwrap();
    let record = sample_private_record(); // helper landing in Task 4
    let target = record.target_id().clone();

    store.with_apply_tx(test_apply_token(), |tx| {
        let _rid = tx.stage_version(&record)?;
        tx.activate_version(&target, 1, None)?;
        Ok(())
    }).await.unwrap();

    let principal = Principal::system();
    let got = store.get(&principal, &target).await.unwrap().unwrap();
    assert_eq!(got.body(), record.body());
    let list = store.list(&ListQuery::new(principal)).await.unwrap();
    assert_eq!(list.rows.len(), 1);
    assert_eq!(list.hidden, 0);
}
```

`sample_private_record` lives in `cairn-test-fixtures` and is added in Task 4 step 4.1 — for now, comment out the test body or use a local builder against the existing `MemoryRecord` constructors.

- [ ] **3.7: Run unit + smoke tests**

Run: `cargo nextest run -p cairn-store-sqlite --no-fail-fast`. Expected: PASS (smoke + migration + conn tests).

- [ ] **3.8: Workspace check**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`. Expected: PASS.

- [ ] **3.9: Commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(store-sqlite): impl MemoryStore (read) + MemoryStoreApply (write)

Land the SQLite implementations of both halves of the storage contract.
SqliteMemoryStore::open async-opens .cairn/cairn.db and runs migrations
on a tokio::task::spawn_blocking. The read impl filters per-row via
rebac::principal_can_read and reports a hidden count in ListResult.
The apply impl is async via async_trait::async_trait and wraps one
rusqlite::Transaction inside one spawn_blocking holding the connection
mutex for the entire closure, satisfying the brief's atomicity invariant
for state change + consent_journal commits. Capability flags flip to
{fts, graph_edges, transactions}=true, vector=false (vec lands in #48).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 4 — Conformance suite + integration tests + property tests

**Files:**
- Create: `crates/cairn-test-fixtures/src/store_conformance.rs`
- Create: 14+ files in `crates/cairn-store-sqlite/tests/`
- Create: `crates/cairn-store-sqlite/tests/ui/*.rs` (trybuild compile-fail)

### Steps

- [ ] **4.1: Add `store_conformance.rs` to fixtures**

Create `crates/cairn-test-fixtures/src/store_conformance.rs`:

```rust
//! Generic `MemoryStore` conformance suite. Future stores (e.g.
//! `cairn-store-nexus`) call `run_conformance(make_store)` to verify
//! they uphold the contract.

use cairn_core::contract::memory_store::{
    apply::MemoryStoreApply, MemoryStore,
};
use std::future::Future;
use std::pin::Pin;

pub type StoreFactory<S> = Box<dyn FnOnce()
    -> Pin<Box<dyn Future<Output = S> + Send>> + Send>;

pub async fn run_conformance<S>(make_store: StoreFactory<S>)
where
    S: MemoryStore + MemoryStoreApply + 'static,
{
    let store = make_store().await;
    crud_roundtrip(&store).await;
    cow_versioning(&store).await;
    // Each helper below stages records via with_apply_tx + asserts via
    // get/list/version_history. See spec §8 for the full matrix.
}

async fn crud_roundtrip<S: MemoryStore + MemoryStoreApply>(_store: &S) {
    // Stub. Concrete implementation tracks the dedicated integration
    // test in cairn-store-sqlite/tests/crud_roundtrip.rs — copy the
    // body here once it stabilizes so future stores re-run it.
}

async fn cow_versioning<S: MemoryStore + MemoryStoreApply>(_store: &S) {
    // Stub. Same comment as crud_roundtrip.
}

pub fn sample_private_record() -> cairn_core::domain::record::MemoryRecord {
    // Build a minimal MemoryRecord with target_id "smoke-target".
    // Field-by-field constructor depending on the existing MemoryRecord
    // builder in cairn-core::domain::record. Adapt to the actual API.
    todo!("adapt to MemoryRecord builder in cairn-core")
}
```

Replace the `todo!` and stub bodies with concrete implementations as the integration tests below take shape. The fixture exists so future store implementations can re-run the suite. Add `pub mod store_conformance;` to `crates/cairn-test-fixtures/src/lib.rs`.

- [ ] **4.2: `cow_versioning.rs`**

Create `crates/cairn-store-sqlite/tests/cow_versioning.rs`:

```rust
use cairn_core::contract::memory_store::{
    apply::MemoryStoreApply, types::TargetId, MemoryStore,
};
use cairn_core::domain::identity::Principal;
use cairn_core::wal::test_apply_token;
use cairn_store_sqlite::SqliteMemoryStore;
use cairn_test_fixtures::store_conformance::sample_private_record;
use tempfile::tempdir;

#[tokio::test]
async fn two_versions_cow_with_one_active() {
    let dir = tempdir().unwrap();
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db")).await.unwrap();
    let record_v1 = sample_private_record();
    let target = record_v1.target_id().clone();

    store.with_apply_tx(test_apply_token(), |tx| {
        let _ = tx.stage_version(&record_v1)?;
        tx.activate_version(&target, 1, None)?;
        Ok(())
    }).await.unwrap();

    let mut record_v2 = record_v1.clone();
    record_v2.set_body("v2 body".into());

    store.with_apply_tx(test_apply_token(), |tx| {
        let _ = tx.stage_version(&record_v2)?;
        tx.activate_version(&target, 2, Some(1))?;
        Ok(())
    }).await.unwrap();

    let principal = Principal::system();
    let history = store.version_history(&principal, &target).await.unwrap();
    assert_eq!(history.len(), 2);
    let got = store.get(&principal, &target).await.unwrap().unwrap();
    assert_eq!(got.body(), "v2 body");
}
```

Run: `cargo nextest run -p cairn-store-sqlite --test cow_versioning`. Expected: PASS.

Commit at the end of the task block (one task = one commit).

- [ ] **4.3: `tombstone_preserves_history.rs`**

```rust
// Test as described in spec §8.2: stage v1 → activate → stage v2 →
// activate → tombstone target. Assert version_history(principal=system,
// target) returns 2 HistoryEntry::Version entries; v1.events[0] is
// Update at v1.created_at and events[1] is Tombstone at the tombstone
// timestamp; v2 mirrors. get returns None.
//
// Concrete code follows the cow_versioning pattern; substitute the
// final tombstone_target call inside one with_apply_tx and assert
// the events vector.
```

Write the test concretely (copy the harness from 4.2). Run nextest. Expected: PASS.

- [ ] **4.4: `expire_active.rs`**

Stage record + activate + expire_active (`Timestamp::now()`) → assert `get` returns `None`, `list(include_expired=true)` shows it.

- [ ] **4.5: `activate_validates_existence.rs`**

Stage v1 + activate v1 + attempt `activate_version(target, 999, Some(1))` → `StoreError::NotFound`. Re-issue with `activate_version(target, 1, None)` already done; subsequent `activate_version(target, 1, None)` → `Conflict { ActivationRaced }` because current_active >= requested.

- [ ] **4.6: `activate_monotonicity.rs`**

Stage v1+v2+v3, activate v3, then `activate_version(target, 2, Some(3))` → `Conflict::ActivationRaced`. v3 stays active.

- [ ] **4.7: `purge_audit_marker.rs`**

Stage 3 versions + edges + (FTS auto-syncs). Call `purge_target(target, op_id, actor)`. Assert: zero rows in `records`/`edges`/`edge_versions`/`records_fts` for any of the captured `record_id`s; one row in `record_purges`. `version_history(system_principal, target)` returns one `HistoryEntry::Purge` whose `event` matches. Re-invoke same `(target, op_id)` → `PurgeOutcome::AlreadyPurged`, no extra rows.

- [ ] **4.8: `tx_rollback.rs`**

Two cases:
1. Closure returns `Err(StoreError::Invariant("simulated"))` → assert no `records` row, no `consent_journal` row.
2. Closure panics → tokio surfaces a JoinError; reuse the store afterwards (next `with_apply_tx` succeeds).

- [ ] **4.9: `edges.rs`**

Stage two records, add edge between them, assert `edges` row + `edge_versions` insert marker. Re-add same edge → updates row, writes `update` marker. Remove edge → deletes row, writes `remove` marker. Tombstone source record → edge survives (verify via raw SELECT).

- [ ] **4.10: `capabilities.rs`**

```rust
let store = SqliteMemoryStore::open(...).await.unwrap();
let caps = store.capabilities();
assert!(caps.fts);
assert!(!caps.vector);
assert!(caps.graph_edges);
assert!(caps.transactions);
```

- [ ] **4.11: `apply_token_gate.rs` (trybuild compile-fail)**

Create `crates/cairn-store-sqlite/tests/apply_token_gate.rs`:

```rust
#[test]
fn apply_token_cannot_be_minted_outside_wal() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/apply_token_*.rs");
}
```

Create `crates/cairn-store-sqlite/tests/ui/apply_token_user_code.rs`:

```rust
fn main() {
    // Should fail: ApplyToken::__new is pub(in crate::wal) only.
    let _t = cairn_core::contract::memory_store::apply::ApplyToken::__new();
}
```

Run: `cargo nextest run -p cairn-store-sqlite --test apply_token_gate`. Expected: PASS — trybuild reports the user code fails to compile.

- [ ] **4.12: `consent_journal_atomicity.rs`**

Inside one `with_apply_tx`: stage v1, activate v1, append a `ConsentJournalEntry`, then return `Err(...)`. Assert: zero `records` rows, zero `consent_journal` rows. Repeat with `Ok(())` → both visible.

- [ ] **4.13: `rebac_visibility.rs`**

Build three records with different `scope.visibility` values: `private` for alice, `private` for bob, `team`. Stage + activate each via `with_apply_tx(test_apply_token, ...)`. With `Principal::for_user("alice")`, call `list(ListQuery::new(...))` → `rows.len() == 2` (alice's private + team), `hidden == 1`. `get(principal=alice, bob_target)` returns `None`. `Principal::system()` sees all three.

Add a helper to `cairn-test-fixtures` for `Principal::for_user(...)` if missing.

- [ ] **4.14: `fts_visibility_predicate.rs`**

Stage three records with distinct bodies: one active+visible, one tombstoned, one expired. Run an FTS query joined with the read predicate (manual SQL via direct connection or via a `search_keyword` helper if Task 3 added one). Assert only the visible record matches.

If `search_keyword` doesn't exist in #46 (it lives in #47), do the query inline:

```rust
let conn = store.conn.try_lock().unwrap();
let mut stmt = conn.prepare(
    "SELECT records.record_id FROM records_fts \
     JOIN records ON records.rowid = records_fts.rowid \
     WHERE records_fts MATCH ?1 \
       AND records.active = 1 AND records.tombstoned = 0 \
       AND (records.expired_at IS NULL OR records.expired_at > datetime('now'))",
).unwrap();
let mut rows = stmt.query(["needle"]).unwrap();
let mut count = 0;
while rows.next().unwrap().is_some() { count += 1; }
assert_eq!(count, 1);
```

- [ ] **4.15: Property tests**

Create `crates/cairn-store-sqlite/tests/proptest_roundtrip.rs`:

```rust
use proptest::prelude::*;
use cairn_test_fixtures::store_conformance::sample_private_record;
// ... full test using cairn-test-fixtures' MemoryRecord proptest strategy.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn upsert_get_roundtrip(record in cairn_test_fixtures::record::record_strategy()) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let store = cairn_store_sqlite::SqliteMemoryStore::open(
                &dir.path().join("cairn.db"),
            ).await.unwrap();
            let target = record.target_id().clone();
            store.with_apply_tx(cairn_core::wal::test_apply_token(), |tx| {
                let _ = tx.stage_version(&record)?;
                tx.activate_version(&target, 1, None)?;
                Ok(())
            }).await.unwrap();
            let got = store
                .get(&cairn_core::domain::identity::Principal::system(), &target)
                .await.unwrap().unwrap();
            prop_assert_eq!(got.body(), record.body());
            Ok(())
        }).unwrap();
    }
}
```

If a `record_strategy()` doesn't already exist in `cairn-test-fixtures`, add a minimal one (use existing constructors).

- [ ] **4.16: Run full workspace check**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: ALL PASS. Fix any clippy/check warnings inline before committing.

- [ ] **4.17: Commit**

```bash
git add -A
git commit -F - <<'EOF'
test(store-sqlite): COW versioning + tombstone + purge + token gate

Cover spec §8 in full:
- crud_roundtrip, cow_versioning (stage→activate, two-version COW)
- tombstone_preserves_history (immutable lifecycle event log)
- expire_active, fts_visibility_predicate (expiry fence)
- activate_validates_existence, activate_monotonicity (CAS guard)
- purge_audit_marker (capture-first ordering, idempotent under op_id)
- tx_rollback (Err + panic both roll back)
- edges (add/remove/upsert + tombstone non-cascade)
- capabilities (caps post-#46)
- apply_token_gate (trybuild compile-fail)
- consent_journal_atomicity (rollback drops both writes)
- rebac_visibility (per-row drop + hidden count)
- proptest_roundtrip (property-based stage→get round-trip)
- store_conformance suite re-runnable by future stores

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
```

---

## Self-review checklist

Before opening the PR, walk through each spec section and confirm coverage:

- [ ] §3 architecture — Task 1 + Task 3 file layout
- [ ] §4.1 read trait — Task 1 step 1.8
- [ ] §4.2 apply trait + token — Task 1 steps 1.6, 1.7
- [ ] §4.3 tx execution model — Task 3 step 3.4
- [ ] §5.1 tables — Task 2 step 2.3
- [ ] §5.2 records DDL — Task 2 step 2.3
- [ ] §5.3 write semantics — Task 3 step 3.4
- [ ] §5.4 version_history — Task 3 steps 3.1, 3.3
- [ ] §5.5 edge semantics — Task 3 step 3.4
- [ ] §5.6 in-tx consent journal — Task 3 step 3.4 (`append_consent_journal`)
- [ ] §6 error handling — Task 1 step 1.5 + Task 2 step 2.4
- [ ] §7 migrations — Task 2 steps 2.2-2.7
- [ ] §8 testing matrix — Task 4 steps 4.2-4.15
- [ ] §11 risks — none code-blocking; carry forward in PR description

When complete, open the PR with the brief section numbers (§3.0, §4, §5.1, §5.2, §5.6) cited in the description, the four invariants touched (5 — WAL-only writes via token; 7 — no `unsafe`; 8 — no `unwrap`/`expect` in core; 9 — privacy via rebac filter), and the verification output pasted from step 4.16.
