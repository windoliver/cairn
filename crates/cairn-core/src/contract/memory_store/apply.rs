//! Token-gated apply (write) surface for `MemoryStore`.
//!
//! [`ApplyToken`] is defined in `crate::wal` (where the constructor lives)
//! and re-exported here so downstream crates see it at the
//! `contract::memory_store::apply::ApplyToken` path. Only code inside
//! `crate::wal` can construct one; non-WAL callers cannot compile against
//! `with_apply_tx`.
//!
//! The sealing supertrait prevents third-party implementations of the write
//! traits outside this crate. Adapter crates implement the seal via
//! `impl cairn_core::contract::memory_store::apply::private::Sealed for
//! MyStore {}` — the `private` module name plus `#[doc(hidden)]` make
//! third-party impls explicitly transgressive rather than accidental.

pub use crate::wal::ApplyToken;

use super::error::StoreError;
use super::types::{
    ConsentJournalEntry, ConsentJournalRowId, Edge, EdgeKind, OpId, PurgeOutcome, RecordId,
    TargetId,
};
use crate::domain::{actor_ref::ActorRef, record::MemoryRecord, timestamp::Rfc3339Timestamp};

/// Sealing module — prevents third-party `MemoryStoreApply` /
/// `MemoryStoreApplyTx` impls from outside `cairn-core`.
///
/// Adapter crates may implement `private::Sealed` to opt in; the
/// `#[doc(hidden)]` and `private` name signal that doing so without
/// `cairn-core`'s knowledge is explicitly transgressive.
#[doc(hidden)]
pub mod private {
    /// Seal supertrait. Implement this for a type to mark it as an
    /// approved implementor of the apply traits.
    pub trait Sealed {}
}

/// Apply entrypoint trait. Sealed — only `cairn-core`-blessed types can
/// implement it.
///
/// To implement this trait on an adapter type, also implement
/// `cairn_core::contract::memory_store::apply::private::Sealed` for it.
///
/// `with_apply_tx` is `async` so adapters can dispatch the synchronous
/// closure to `tokio::task::spawn_blocking` and await the join handle.
/// The closure itself is synchronous to match `rusqlite::Transaction`'s
/// thread-affinity requirement.
#[async_trait::async_trait]
pub trait MemoryStoreApply: private::Sealed + Send + Sync {
    /// Run a synchronous closure inside one database transaction.
    ///
    /// - **Commit** on `Ok(t)`.
    /// - **Rollback** on `Err(e)` or panic inside `f`.
    ///
    /// The `token` parameter is consumed, forcing the caller to hold an
    /// `ApplyToken` issued by the WAL executor (or by
    /// `cairn_core::wal::test_apply_token()` in tests).
    async fn with_apply_tx<F, T>(&self, token: ApplyToken, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut dyn MemoryStoreApplyTx) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static;
}

/// Synchronous write methods executed inside a single database transaction.
///
/// Implementations run on a blocking thread (e.g. `spawn_blocking`) while
/// holding a live database transaction handle. All methods are synchronous.
///
/// Implementations are NOT required to be `Send`: the object is created
/// inside `spawn_blocking`, passed by `&mut` to the closure `F`, and dropped
/// before the blocking task returns. It never crosses a thread boundary.
///
/// To implement this trait on an adapter type, also implement
/// `cairn_core::contract::memory_store::apply::private::Sealed` for it.
pub trait MemoryStoreApplyTx: private::Sealed {
    /// Stage a new version of a record with `active = 0`.
    ///
    /// `target_id` is the stable logical identity for the record across all
    /// versions. The caller must supply it explicitly — `MemoryRecord` does not
    /// carry a `target_id` field, so deriving it from `record.id` would assign
    /// each call a fresh logical identity and break copy-on-write versioning.
    ///
    /// `created_by` is the **trusted** actor performing the write, captured
    /// by the WAL executor before the apply transaction starts. It is
    /// persisted into the audit columns and surfaced through
    /// [`HistoryEntry::Update`]. It must NOT be derived from
    /// caller-controlled record fields (e.g. `record.actor_chain`), which
    /// are payload data and forgeable.
    ///
    /// The store computes `version = max(existing) + 1` internally. The
    /// deterministic per-version `record_id = BLAKE3(target_id || '#' || version)`
    /// is computed inside this method. Returns the generated `RecordId`.
    ///
    /// On `(target_id, version)` collision returns
    /// `StoreError::Conflict { kind: VersionAlreadyStaged }`.
    fn stage_version(
        &mut self,
        target_id: &TargetId,
        record: &MemoryRecord,
        created_by: &ActorRef,
    ) -> Result<RecordId, StoreError>;

    /// Atomically flip `active` so exactly one version of `target_id` is
    /// active.
    ///
    /// `expected_prior`: if `Some(v)`, the current active version must be
    /// `v`; if `None`, the target must currently have no active row
    /// (first activation). Violations return
    /// `StoreError::Conflict { kind: ActivationRaced }`.
    ///
    /// `activated_by` is the **trusted** actor performing the
    /// activation, supplied by the WAL executor. Stage and activate are
    /// independent writes that may be issued by different actors; the
    /// audit trail must record who promoted the version, not who staged
    /// it. Treat `record.actor_chain` as opaque payload — never derive
    /// the activator from it.
    fn activate_version(
        &mut self,
        target_id: &TargetId,
        version: u64,
        expected_prior: Option<u64>,
        activated_by: &ActorRef,
    ) -> Result<(), StoreError>;

    /// Set `tombstoned = 1` on **every** version of `target_id`.
    ///
    /// Idempotent: re-tombstoning is a no-op. `actor` is recorded in
    /// `tombstoned_by` / `tombstoned_at` columns (Phase A forget).
    fn tombstone_target(
        &mut self,
        target_id: &TargetId,
        actor: &ActorRef,
    ) -> Result<(), StoreError>;

    /// Set `expired_at` on the currently active version of `target_id`.
    ///
    /// Subsequent reads filter rows where
    /// `expired_at IS NULL OR expired_at > now()`, unless
    /// `ListQuery::include_expired` is set.
    fn expire_active(
        &mut self,
        target_id: &TargetId,
        at: Rfc3339Timestamp,
    ) -> Result<(), StoreError>;

    /// Phase B purge primitive.
    ///
    /// In one transaction:
    /// 1. Capture the per-version `record_id` set for `target_id`.
    /// 2. INSERT a metadata-only marker into `record_purges` keyed by
    ///    `(target_id, op_id)` — idempotent; returns
    ///    `PurgeOutcome::AlreadyPurged` if the marker already exists.
    /// 3. DELETE from `edges` / `edge_versions` where `from_id` or `to_id`
    ///    is in the captured set.
    /// 4. DELETE from `records_fts` where `record_id` is in the set.
    /// 5. DELETE from `records` where `target_id = target_id`.
    fn purge_target(
        &mut self,
        target_id: &TargetId,
        op_id: &OpId,
        actor: &ActorRef,
    ) -> Result<PurgeOutcome, StoreError>;

    /// Insert or update a graph edge between two per-version record ids.
    fn add_edge(&mut self, edge: &Edge) -> Result<(), StoreError>;

    /// Remove a graph edge and append a history marker to `edge_versions`.
    fn remove_edge(
        &mut self,
        from: &RecordId,
        to: &RecordId,
        kind: EdgeKind,
    ) -> Result<(), StoreError>;

    /// Insert a consent journal row in the current transaction.
    ///
    /// The brief (§5.6 line 2029) requires the consent row and the state
    /// change (e.g. `activate_version`) to commit atomically. The WAL
    /// executor calls this inside the same `with_apply_tx` closure that
    /// performs the state change.
    fn append_consent_journal(
        &mut self,
        entry: &ConsentJournalEntry,
    ) -> Result<ConsentJournalRowId, StoreError>;
}
