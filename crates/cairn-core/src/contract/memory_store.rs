//! `MemoryStore` contract (brief §4 row 1).
//!
//! P0 scaffold: surface only — `name`, `capabilities`,
//! `supported_contract_versions`. CRUD/FTS/ANN/graph methods land in #46.

use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `MemoryStore`. Bumps when the trait surface changes.
/// Bumped 0.1 → 0.2 in #46 when CRUD/edge/search/tx methods landed.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 2, 0);

/// Errors raised by `MemoryStore` implementations. Adapters define their
/// own concrete type (e.g. `cairn_store_sqlite::StoreError`); this is the
/// trait-level alias to avoid leaking adapter types into core.
///
/// At the trait level, callers see `StoreError`. Concrete adapters
/// substitute their own enum with `From` impls covering the trait surface.
pub type StoreError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Static capability declaration for a `MemoryStore` impl.
///
/// Cairn queries this before dispatching ANN-, FTS-, or graph-using verbs;
/// missing capability → `CapabilityUnavailable` (brief §4.1).
// Four capability flags mirror the four distinct store dimensions; a state
// machine would add indirection with no gain here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryStoreCapabilities {
    /// Whether full-text search (FTS5) is supported.
    pub fts: bool,
    /// Whether vector/ANN search is supported.
    pub vector: bool,
    /// Whether graph edge storage and traversal is supported.
    pub graph_edges: bool,
    /// Whether ACID transactions are supported.
    pub transactions: bool,
}

/// Storage contract — typed CRUD + ANN + FTS + graph over `MemoryRecord`.
///
/// Brief §4 row 1: P0 default is pure `SQLite` + FTS5; P1 default is the
/// Nexus sandbox profile. Method bodies arrive in #46 once `MemoryRecord`
/// (sub-issue #37) lands.
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
    async fn tombstone(&self, id: &RecordId, reason: TombstoneReason) -> Result<(), StoreError>;

    /// Full version history for a target, oldest → newest. Includes
    /// active and inactive rows.
    async fn versions(&self, target: &TargetId) -> Result<Vec<RecordVersion>, StoreError>;

    // ── Edges (#46) ───────────────────────────────────────────────────────

    /// Insert or replace an edge. `updates`-edge invariants are enforced
    /// by schema triggers (distinct `target_id`s, non-tombstoned endpoints,
    /// post-insert immutability) and surface as
    /// [`StoreError`] when violated.
    async fn put_edge(&self, edge: &Edge) -> Result<(), StoreError>;

    /// Remove an edge. Returns `true` if a row was deleted, `false`
    /// otherwise. `updates` edges are immutable and removal returns a
    /// trigger error wrapped in [`StoreError`].
    async fn remove_edge(&self, key: &EdgeKey) -> Result<bool, StoreError>;

    /// Edges adjacent to `id`. `EdgeDir::Out` returns outgoing edges,
    /// `EdgeDir::In` incoming, `EdgeDir::Both` the union. Endpoints
    /// pointing into superseded or tombstoned records are dropped.
    async fn neighbours(&self, id: &RecordId, dir: EdgeDir) -> Result<Vec<Edge>, StoreError>;

    // ── Search (#47, stubbed in PR-A) ─────────────────────────────────────

    /// Keyword search over `body` + `path` returning ranking-input
    /// candidates. The shared ranker (brief §5.1) is a separate pure
    /// function in `cairn-core`; this method does not produce a final
    /// score. Returns a capability-unavailable error when the `fts`
    /// capability is off.
    async fn search_keyword(
        &self,
        args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError>;
}

/// Static identity descriptor for a [`MemoryStore`] plugin (§4.1).
///
/// This companion trait carries the two associated consts that the
/// `register_plugin_with!` macro checks **before construction** — the
/// stable plugin name and the supported contract-version range.
///
/// Separating these consts from [`MemoryStore`] is required by stable Rust:
/// associated consts in a trait break `dyn` compatibility unless gated by
/// `where Self: Sized` (an unstable feature as of 1.95). Placing them in a
/// `Sized`-bounded companion trait keeps `dyn MemoryStore` valid while still
/// allowing the macro to enforce `<Impl as MemoryStorePlugin>::NAME ==
/// registered_name` at compile time.
///
/// Every concrete [`MemoryStore`] implementation should also implement
/// `MemoryStorePlugin`. The blanket-compatible methods `fn name` and
/// `fn supported_contract_versions` on [`MemoryStore`] should delegate to
/// these consts (e.g. `fn name(&self) -> &str { Self::NAME }`).
pub trait MemoryStorePlugin: MemoryStore + Sized {
    /// Stable plugin name, checked statically before construction (§4.1).
    ///
    /// Must match the `name` literal passed to `register_plugin!` /
    /// `register_plugin_with!`.
    const NAME: &'static str;

    /// Version range checked statically before construction (§4.1).
    const SUPPORTED_VERSIONS: VersionRange;
}

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
    /// Stable lowercase label persisted in the `tombstone_reason` column.
    #[must_use]
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Expire => "expire",
            Self::Forget => "forget",
            Self::Purge => "purge",
        }
    }

    /// Inverse of [`TombstoneReason::as_db_str`]. Returns `None` for
    /// unrecognized labels — callers should treat that as a schema/version
    /// mismatch.
    #[must_use]
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
    /// Identifier of the record row produced (or re-used) by the upsert.
    pub record_id: RecordId,
    /// Stable target identity the record belongs to.
    pub target_id: TargetId,
    /// Monotonic version index for this `target_id` after the upsert.
    pub version: u32,
    /// `false` when the store deduplicated against the prior body hash.
    pub content_changed: bool,
    /// Body hash of the previous active version, if any.
    pub prior_hash: Option<BodyHash>,
}

/// Filter args for `list`. All `Option` fields are AND-combined.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListArgs {
    /// Restrict to a single `MemoryKind`.
    pub kind: Option<MemoryKind>,
    /// Restrict to a single `MemoryClass`.
    pub class: Option<MemoryClass>,
    /// Visibility values the caller is allowed to see; empty = no filter.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Maximum number of records to return in this page.
    pub limit: usize,
    /// Optional resume cursor from the previous page.
    pub cursor: Option<ListCursor>,
}

/// Opaque keyset cursor for `list`. Encoded base64-json on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListCursor {
    /// `updated_at` epoch-seconds boundary of the previous page's tail.
    pub updated_at: i64,
    /// Tie-breaker record id from the previous page's tail row.
    pub record_id: RecordId,
}

/// One page of records returned by `list`.
#[derive(Debug, Clone, PartialEq)]
pub struct ListPage {
    /// Records in the page, ordered newest-first by `(updated_at, record_id)`.
    pub records: Vec<MemoryRecord>,
    /// Cursor to fetch the next page, or `None` when exhausted.
    pub next_cursor: Option<ListCursor>,
}

/// One row from `versions(target)` — schema-level metadata only, not the
/// full hydrated record. Callers that want the body call `get(record_id)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordVersion {
    /// Identifier of this version row.
    pub record_id: RecordId,
    /// Target identity this version belongs to.
    pub target_id: TargetId,
    /// Monotonic version index within the target.
    pub version: u32,
    /// Epoch-seconds when the row was created.
    pub created_at: i64,
    /// Epoch-seconds of the most recent metadata mutation.
    pub updated_at: i64,
    /// `true` if this row is the current active version for its target.
    pub active: bool,
    /// `true` if this row is tombstoned and excluded from queries.
    pub tombstoned: bool,
    /// Why the row was tombstoned, if applicable.
    pub tombstone_reason: Option<TombstoneReason>,
    /// blake3 body hash of the persisted payload.
    pub body_hash: BodyHash,
}

/// Edge kinds supported at P0. Exhaustive — adding a new kind is a
/// brief-level change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EdgeKind {
    /// Fact-supersession (brief §3 line ~409). Endpoints must be
    /// non-tombstoned with distinct `target_id`s; the store schema enforces
    /// this with triggers.
    Updates,
    /// Cross-reference / mention.
    Mentions,
    /// Supports / corroborates.
    Supports,
}

impl EdgeKind {
    /// Stable lowercase label persisted in the `kind` column.
    #[must_use]
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Updates => "updates",
            Self::Mentions => "mentions",
            Self::Supports => "supports",
        }
    }

    /// Inverse of [`EdgeKind::as_db_str`]. Returns `None` for unrecognized
    /// labels — callers should treat that as a schema/version mismatch.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "updates" => Some(Self::Updates),
            "mentions" => Some(Self::Mentions),
            "supports" => Some(Self::Supports),
            _ => None,
        }
    }
}

/// Directed edge between two records.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    /// Source endpoint of the edge.
    pub src: RecordId,
    /// Destination endpoint of the edge.
    pub dst: RecordId,
    /// Edge kind discriminator.
    pub kind: EdgeKind,
    /// Optional weight in `[0.0, 1.0]`; semantics depend on `kind`.
    pub weight: Option<f32>,
}

/// Composite key identifying an edge (without its weight).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EdgeKey {
    /// Source endpoint of the edge.
    pub src: RecordId,
    /// Destination endpoint of the edge.
    pub dst: RecordId,
    /// Edge kind discriminator.
    pub kind: EdgeKind,
}

/// Direction selector for edge queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeDir {
    /// Outgoing edges (`src = pivot`).
    Out,
    /// Incoming edges (`dst = pivot`).
    In,
    /// Union of outgoing and incoming edges.
    Both,
}

// ── Search types (used by trait stub here; impl in PR-B) ──────────────────────

/// Args for the keyword (FTS5) branch of `search`.
///
/// Carries the lifetime of the borrowed [`ValidatedFilter`] so callers can
/// validate once and pass the proof-token down to the store without
/// allocation. `PartialEq` is intentionally omitted: `ValidatedFilter`
/// holds a borrowed reference whose equality semantics are caller-defined.
#[derive(Debug, Clone)]
pub struct KeywordSearchArgs<'a> {
    /// Raw FTS5 expression. Store does not validate FTS5 syntax; `SQLite`
    /// surfaces parse errors which the store re-wraps in PR-B as a typed
    /// FTS error variant on `StoreError`.
    pub query: String,
    /// Pre-validated filter tree from
    /// [`crate::domain::filter::validate_filter`].
    pub filter: Option<ValidatedFilter<'a>>,
    /// Visibility values the caller is allowed to see; empty = no filter.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Maximum number of candidates to return in this page.
    pub limit: usize,
    /// Optional resume cursor from the previous page.
    pub cursor: Option<KeywordCursor>,
}

/// Opaque keyset cursor for keyword search. Encoded base64-json on the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct KeywordCursor {
    /// BM25 score boundary of the previous page's tail row.
    pub bm25: f64,
    /// Tie-breaker record id from the previous page's tail row.
    pub record_id: RecordId,
}

/// One page of candidates returned by the keyword branch of `search`.
#[derive(Debug, Clone, PartialEq)]
pub struct KeywordSearchPage {
    /// Candidates ordered by ascending BM25 (lower = better in `SQLite` FTS5).
    pub candidates: Vec<SearchCandidate>,
    /// Cursor to fetch the next page, or `None` when exhausted.
    pub next_cursor: Option<KeywordCursor>,
}

/// A single candidate row from a search query, with the signal columns the
/// reranker needs.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchCandidate {
    /// Identifier of the candidate record.
    pub record_id: RecordId,
    /// Target identity the candidate belongs to.
    pub target_id: TargetId,
    /// Scope tuple of the candidate.
    pub scope: ScopeTuple,
    /// Memory kind of the candidate.
    pub kind: MemoryKind,
    /// Memory class of the candidate.
    pub class: MemoryClass,
    /// Visibility of the candidate.
    pub visibility: MemoryVisibility,
    /// FTS5 BM25 score (lower = better).
    pub bm25: f64,
    /// Seconds since the candidate's `updated_at`.
    pub recency_seconds: i64,
    /// Confidence value cached on the row (`[0.0, 1.0]`).
    pub confidence: f32,
    /// Salience value cached on the row (`[0.0, 1.0]`).
    pub salience: f32,
    /// Seconds since the candidate's last refresh; used for staleness penalty.
    pub staleness_seconds: i64,
    /// Snippet excerpt produced by FTS5 `snippet()`.
    pub snippet: String,
    /// Serialized `MemoryRecord` for callers that want full hydration
    /// without a second round-trip. Never logged above `trace`.
    pub record_json: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubStore;

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
            Ok(ListPage {
                records: vec![],
                next_cursor: None,
            })
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
        async fn neighbours(&self, _id: &RecordId, _d: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            Ok(vec![])
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            Err("stub: search_keyword not implemented".into())
        }
    }

    impl MemoryStorePlugin for StubStore {
        const NAME: &'static str = "stub";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 3, 0));
    }

    #[test]
    fn dyn_compatible() {
        let s: Box<dyn MemoryStore> = Box::new(StubStore);
        assert_eq!(s.name(), "stub");
        assert!(s.capabilities().fts);
        assert!(s.supported_contract_versions().accepts(CONTRACT_VERSION));
    }

    #[test]
    fn static_consts_accessible() {
        assert_eq!(StubStore::NAME, "stub");
        assert!(StubStore::SUPPORTED_VERSIONS.accepts(CONTRACT_VERSION));
    }
}
