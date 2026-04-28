//! Shared types for the `MemoryStore` contract surface.
//!
//! These types compose the read-API parameters and return shapes plus
//! the apply-API method arguments. Domain types (`MemoryRecord`,
//! `Principal`, `ActorRef`, `Rfc3339Timestamp`, `Scope`) live in
//! `cairn_core::domain`.

use crate::domain::{
    actor_ref::ActorRef, principal::Principal, record::MemoryRecord, timestamp::Rfc3339Timestamp,
};
use serde::{Deserialize, Serialize};

/// Stable logical record identity. Distinct from per-version `RecordId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TargetId(pub String);

impl TargetId {
    /// Construct a new `TargetId` from any string-convertible value.
    #[must_use]
    pub fn new<S: Into<String>>(s: S) -> Self {
        Self(s.into())
    }

    /// View as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TargetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Per-version record id. Computed `BLAKE3(target_id || '#' || version)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordId(pub String);

impl RecordId {
    /// Compute the deterministic per-version id from a stable `target_id`
    /// and its monotonic `version` number.
    #[must_use]
    pub fn from_target_version(target: &TargetId, version: u64) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(target.as_str().as_bytes());
        hasher.update(b"#");
        hasher.update(version.to_string().as_bytes());
        Self(hasher.finalize().to_hex().to_string())
    }

    /// View as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RecordId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// WAL operation id. Ferried through purge/journal flows for idempotency.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OpId(pub String);

impl OpId {
    /// Construct a new `OpId` from any string-convertible value.
    #[must_use]
    pub fn new<S: Into<String>>(s: S) -> Self {
        Self(s.into())
    }

    /// View as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Edge kind. Closed enum at P0; `#[non_exhaustive]` for forward-compat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EdgeKind {
    /// This record refines / specialises another.
    Refines,
    /// This record contradicts another.
    Contradicts,
    /// This record was derived from another.
    DerivedFrom,
    /// Informational cross-reference.
    SeeAlso,
    /// This record mentions another.
    Mentions,
}

/// One graph edge between two per-version [`RecordId`]s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    /// Source per-version record id.
    pub from: RecordId,
    /// Destination per-version record id.
    pub to: RecordId,
    /// Relationship kind.
    pub kind: EdgeKind,
    /// Edge weight in `[0.0, 1.0]`.
    pub weight: f32,
    /// Opaque metadata; the store round-trips the JSON value unchanged.
    pub metadata: serde_json::Value,
}

/// Lifecycle change kind.
///
/// `Update` covers stage + activate; `Tombstone` / `Expire` / `Purge`
/// correspond to the brief's forget-pipeline events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ChangeKind {
    /// Version was created or re-activated.
    Update,
    /// Target was tombstoned (Phase A forget).
    Tombstone,
    /// Active version's `expired_at` was set.
    Expire,
    /// All versions were physically purged (Phase B forget).
    Purge,
}

/// One immutable lifecycle event on a record version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordEvent {
    /// Type of lifecycle change.
    pub kind: ChangeKind,
    /// When the change occurred. `None` when the store row's audit column is
    /// `NULL` (e.g. a row that was staged but never activated).
    pub at: Option<Rfc3339Timestamp>,
    /// Who performed the change (none for system-driven expiry).
    pub actor: Option<ActorRef>,
}

/// One concrete version of a record as returned by `version_history`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordVersion {
    /// Per-version deterministic id.
    pub record_id: RecordId,
    /// Stable logical target id.
    pub target_id: TargetId,
    /// Monotonic version number.
    pub version: u64,
    /// Whether this version is the currently active one.
    pub active: bool,
    /// Ordered lifecycle events for this version, ascending by timestamp.
    pub events: Vec<RecordEvent>,
}

/// Audit marker for a fully-purged target (no `records` rows remain).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PurgeMarker {
    /// Stable logical target id.
    pub target_id: TargetId,
    /// WAL operation id used for idempotency.
    pub op_id: OpId,
    /// Purge lifecycle event (`kind = Purge`).
    pub event: RecordEvent,
    /// Random salt stored alongside the purge marker for audit.
    pub body_hash_salt: String,
}

/// Element of `version_history` return value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HistoryEntry {
    /// A concrete record version row (may be superseded or tombstoned).
    Version(RecordVersion),
    /// An audit marker from `record_purges` (body is gone).
    Purge(PurgeMarker),
}

/// Outcome of `purge_target`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PurgeOutcome {
    /// The target was successfully purged.
    Purged,
    /// A purge with the same `op_id` already completed; this call was a no-op.
    AlreadyPurged,
}

/// Range/list query carrying a principal and pre-resolved filters.
///
/// Visibility and tier filtering are evaluated at the store layer;
/// the SQL filters here narrow the candidate set before rebac evaluation.
#[derive(Debug, Clone)]
pub struct ListQuery {
    /// Caller's principal — drives per-row rebac decisions.
    pub principal: Principal,
    /// Optional prefix filter on `target_id`.
    pub target_prefix: Option<TargetId>,
    /// Optional taxonomy `kind` filter.
    pub kind_filter: Option<String>,
    /// Maximum number of visible rows to return.
    pub max_results: Option<usize>,
    /// Surface tombstoned rows (forensic / audit path).
    pub include_tombstoned: bool,
    /// Surface expired rows (forensic / audit path).
    pub include_expired: bool,
}

impl ListQuery {
    /// Construct a minimal query for the given principal with all filters
    /// at their defaults (no prefix, no kind filter, no row limit, active
    /// only).
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

/// Return envelope for `list`. `hidden` reports the count of rows the
/// rebac filter dropped (brief line 4136: `results_hidden: N`).
#[derive(Debug, Clone, PartialEq)]
pub struct ListResult {
    /// Rows visible to the caller.
    pub rows: Vec<MemoryRecord>,
    /// Number of candidate rows dropped by rebac before return.
    pub hidden: usize,
}

/// Append-only consent journal entry. The store JSON-serialises this on
/// insert and round-trips the bytes on read; the content is opaque to the
/// store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsentJournalEntry {
    /// WAL operation id.
    pub op_id: OpId,
    /// Free-form discriminator (e.g. `"activate"`, `"tombstone"`).
    pub kind: String,
    /// Optional target record id.
    pub target_id: Option<TargetId>,
    /// Actor who triggered this consent event.
    pub actor: ActorRef,
    /// Arbitrary payload (hashes, references — no raw body).
    pub payload: serde_json::Value,
    /// Wall-clock time of the event.
    pub at: Rfc3339Timestamp,
}

/// Primary key of a freshly-written `consent_journal` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentJournalRowId(pub i64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_id_deterministic() {
        let tid = TargetId::new("my-target");
        let r1 = RecordId::from_target_version(&tid, 1);
        let r2 = RecordId::from_target_version(&tid, 1);
        assert_eq!(r1, r2);
        // Different version → different id.
        let r3 = RecordId::from_target_version(&tid, 2);
        assert_ne!(r1, r3);
    }

    #[test]
    fn target_id_display() {
        let tid = TargetId::new("test");
        assert_eq!(tid.to_string(), "test");
    }
}
