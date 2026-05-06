//! `MemoryStore` contract (brief §4 row 1).

use crate::contract::version::{ContractVersion, SchemaVersion, VersionRange};
use crate::domain::record::MemoryRecord;
use crate::search::ScoreExplain;

/// Contract version for `MemoryStore`. Bumps when the trait surface changes.
/// Bumped 0.1 → 0.2 in #46 when CRUD/edge/search/tx methods landed.
/// Bumped 0.2 → 0.3 in #48 when `search_semantic` + `SemanticSearchArgs`
/// landed. Co-evolved within 0.3 in #253 when
/// `MemoryStoreCapabilities::per_record_consent_model` and
/// `MemoryStore::list_consent_models` were added for the §6.5
/// receipt-timeline gate. Co-evolved again within 0.3 in #186 when
/// the bitemporal knowledge-graph methods (`upsert_entity`,
/// `upsert_entity_edge`, `graph_edges`, `resolve_contradiction`,
/// `link_entity_episode`) landed with default-error implementations.
/// Co-evolved again within 0.3 in #49 when search args/pages gained
/// explain plumbing (`with_explain` bool on `*SearchArgs`,
/// `Option<Vec<ScoreExplain>>` on each matching page struct) — all
/// new fields default-initialize.
/// Bumped 0.3 → 0.4 in #258 when `StoredRecord.schema_version` and
/// `RecordVersion.schema_version` landed for the §6.4 stale-schema
/// lint — adding required public fields is a struct-construction
/// break, so the handshake range shifts to `[0.4.0, 0.5.0)`.
/// Bumped 0.4 → 0.5 in #191 when `auth_scope: ScopeTuple` landed as
/// a required public field on `KeywordSearchArgs`, `SemanticSearchArgs`,
/// `HybridSearchArgs`, and the new `GraphNeighborsArgs`. Promoting
/// `auth_scope` out of the user `filter` is the structural break:
/// authorization now flows through a dedicated field that applies
/// identically to lexical legs, graph traversal, and graph hydration,
/// while `filter` keeps its narrowing-only role. The companion
/// `MemoryStoreCapabilities::graph_search` flag and the
/// `search_graph_neighbors` trait method (default impl returns
/// `CapabilityUnavailable`) ship in the same bump.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 5, 0);

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
// Five capability flags mirror the five distinct store dimensions; a state
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
    /// Whether the adapter persists per-record `consent_model` and
    /// returns it via [`MemoryStore::list_consent_models`] (Issue #253).
    /// `false` means lint cannot enforce §6.5 per-row gating on this
    /// adapter and must surface that as a coverage gap; `true` means
    /// `list_consent_models` is the authoritative read path and any
    /// active record missing from its result is corruption.
    pub per_record_consent_model: bool,
    /// Whether `search_graph_neighbors` (1-hop entity-graph expansion) is
    /// supported. Issue #191. `false` means the hybrid orchestrator skips
    /// the graph leg with a [`crate::search::DegradedLeg::Graph`] entry
    /// in the response; `true` means the adapter has all of the
    /// `entity_edges` / `entity_episodes` / `records` columns the §5.1
    /// SQL needs (probed at startup) and `carray` / `interrupt`
    /// extensions are loaded.
    pub graph_search: bool,
}

/// A `MemoryRecord` at a specific store version.
///
/// `version` is the monotonic per-`target_id` counter from the DB COW model
/// (brief §3.0). Projection and resync use it for optimistic concurrency
/// checks without touching the DB row directly.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRecord {
    /// The stored memory record.
    pub record: MemoryRecord,
    /// Monotonic version counter. `1` for a record's first write.
    pub version: u32,
    /// Schema-version stamp set by the store at write time
    /// (`SchemaVersion::current()` for fresh writes, the historical
    /// stamp for older rows). `None` for unstamped legacy rows
    /// (pre-Issue #258 migration); the §6.4 `stale_schema` lint
    /// surfaces those as `Warning` so operators can rewrite them.
    /// Not part of the canonical record bytes.
    pub schema_version: Option<SchemaVersion>,
}

/// Counts of derived-index rows vs canonical records, for the lint
/// `index_drift` check (brief §8 row 7). Counts only — content sampling is
/// out of scope for v0.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct IndexStats {
    /// Active, non-tombstoned canonical records.
    pub records_active: u64,
    /// Rows in the FTS5 mirror.
    pub fts5_rows: u64,
}

impl IndexStats {
    /// Construct an [`IndexStats`] value.
    ///
    /// Provided so that adapter crates outside `cairn-core` can build the
    /// struct despite the `#[non_exhaustive]` attribute.
    #[must_use]
    pub fn new(records_active: u64, fts5_rows: u64) -> Self {
        Self {
            records_active,
            fts5_rows,
        }
    }
}

/// Storage contract — typed CRUD over `MemoryRecord`.
///
/// Brief §4 row 1. Method bodies arrive in #46 (`SQLite` impl);
/// `FixtureStore` in `cairn-test-fixtures` serves tests.
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    /// Returns the store's human-readable name (e.g., `"sqlite"`, `"fixture"`).
    fn name(&self) -> &str;
    /// Returns the static capability advertisement for this store instance.
    fn capabilities(&self) -> &MemoryStoreCapabilities;
    /// Returns the range of contract versions this store implementation accepts.
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

    /// Convenience: fetch the active row for `target` as a [`StoredRecord`].
    /// The default impl walks `versions(target)` for the newest active row,
    /// then `get(record_id)` for its body. Adapters that can answer with one
    /// query (e.g. via the `records_active_target_idx` partial unique index
    /// in `cairn-store-sqlite`) should override.
    ///
    /// Returns `Ok(None)` when no active row exists for the target.
    async fn get_active_by_target(
        &self,
        target: &TargetId,
    ) -> Result<Option<StoredRecord>, StoreError> {
        let history = self.versions(target).await?;
        let Some(v) = history.iter().rev().find(|v| v.active && !v.tombstoned) else {
            return Ok(None);
        };
        let Some(record) = self.get(&v.record_id).await? else {
            return Ok(None);
        };
        Ok(Some(StoredRecord {
            record,
            version: v.version,
            schema_version: v.schema_version,
        }))
    }

    /// Convenience: page through every active record and pair each with its
    /// store version. Used by callers that need a `Vec<StoredRecord>` (e.g.
    /// `cairn lint --fix-markdown`, which feeds the markdown projector).
    /// Default impl follows `next_cursor` until exhausted, then resolves
    /// each record's active version via one `versions()` round-trip;
    /// adapters with a one-shot active+version query should override.
    /// `args.cursor` is overwritten on every iteration; `args.limit` of
    /// `0` means "use the adapter's own page size".
    async fn list_active_stored(&self, args: &ListArgs) -> Result<Vec<StoredRecord>, StoreError> {
        let mut records: Vec<MemoryRecord> = Vec::new();
        let mut cursor = args.cursor.clone();
        loop {
            let page_args = ListArgs {
                cursor: cursor.clone(),
                ..args.clone()
            };
            let page = self.list(&page_args).await?;
            records.extend(page.records);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        let mut out = Vec::with_capacity(records.len());
        for record in records {
            let history = self.versions(&record.target_id).await?;
            let active = history.iter().rev().find(|v| v.active);
            let version = active.map_or(1, |v| v.version);
            let schema_version = active.and_then(|v| v.schema_version);
            out.push(StoredRecord {
                record,
                version,
                schema_version,
            });
        }
        Ok(out)
    }

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

    /// Keyword search over the indexed `body` column returning
    /// ranking-input candidates. The shared ranker (brief §5.1) is a
    /// separate pure function in `cairn-core`; this method does not
    /// produce a final score. Returns a capability-unavailable error
    /// when the `fts` capability is off.
    ///
    /// **Scope is the caller's responsibility.** This method does NOT
    /// derive a scope tuple from its arguments. Callers (the verb-layer
    /// dispatch in `cairn-cli`) MUST resolve the authorized scope (brief
    /// §5.1 Scope Resolve stage) before invoking and fold it into the
    /// query in one of two ways:
    ///
    /// 1. **`visibility_allowlist`** — a tier-only narrowing. Sufficient
    ///    when the authorized scope is a single tier (e.g. an org-public
    ///    search for an unauthenticated agent).
    /// 2. **`filter`** — compose a [`crate::domain::filter::ValidatedFilter`]
    ///    that includes equality predicates over the scope-tuple
    ///    dimensions. The filter DSL exposes `scope_tenant`,
    ///    `scope_workspace`, `scope_session_id`, `scope_entity`,
    ///    `scope_user`, and `scope_agent` for exactly this purpose; they
    ///    compile to `json_extract(scope, '$.<dim>')` against the
    ///    canonical `ScopeTuple` JSON.
    ///
    /// Calling `search_keyword` against a shared multi-tenant DB with
    /// neither narrowing applied returns every row matching the keyword
    /// regardless of scope — the verb layer is the policy boundary that
    /// prevents this in production.
    async fn search_keyword(
        &self,
        args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError>;

    /// Semantic (ANN) search over the sqlite-vec `record_vectors` table.
    ///
    /// Returns `CapabilityUnavailable` when `capabilities().vector` is `false`
    /// (model absent or `search.local_embeddings: false`). Default impl
    /// returns `CapabilityUnavailable` so adapters that don't support vectors
    /// compile without boilerplate.
    ///
    /// Scope is the caller's responsibility — same rules as `search_keyword`.
    async fn search_semantic(
        &self,
        args: &SemanticSearchArgs<'_>,
    ) -> Result<SemanticSearchPage, StoreError> {
        let _ = args;
        Err(String::from("capability unavailable: vector").into())
    }

    /// Hybrid search: parallel keyword + semantic legs, RRF fusion, optional
    /// cosine re-rank.
    ///
    /// Returns `CapabilityUnavailable` when `capabilities().vector` is `false`
    /// (model absent or `search.local_embeddings: false`). Default impl
    /// returns an error so adapters that don't support hybrid retrieval
    /// compile without boilerplate.
    ///
    /// Scope is the caller's responsibility — same rules as `search_keyword`.
    async fn search_hybrid(
        &self,
        args: &HybridSearchArgs<'_>,
    ) -> Result<HybridSearchPage, StoreError> {
        let _ = args;
        Err(String::from("capability unavailable: vector").into())
    }

    /// 1-hop entity-graph neighborhood expansion (Issue #191).
    ///
    /// Returns records whose entities are 1-hop neighbors of the seed set
    /// in the bitemporal entity graph, with edge confidence preserved as
    /// the RRF weight via `GraphCandidate::edge_confidence_score`. The
    /// orchestrator uses this as the third leg of hybrid search; see spec
    /// §5.1 for the SQL.
    ///
    /// `args.auth_scope` and `args.visibility_allowlist` are applied to
    /// BOTH the edge provenance record and the neighbor record;
    /// `args.filter` is applied only to the neighbor (recall narrowing).
    ///
    /// Returns `CapabilityUnavailable` when
    /// `capabilities().graph_search` is `false`. The default impl returns
    /// `CapabilityUnavailable` so adapters that don't ship the §8 capability
    /// probe compile without boilerplate.
    ///
    /// # Errors
    /// Default impl returns `"capability unavailable: graph_search"`.
    async fn search_graph_neighbors(
        &self,
        args: &GraphNeighborsArgs<'_>,
    ) -> Result<Vec<crate::search::GraphCandidate>, StoreError> {
        let _ = args;
        Err("capability unavailable: graph_search".into())
    }

    // ── Lint support (#96) ────────────────────────────────────────────────────

    /// Counts that drive the `lint` index-drift check. Default impl returns
    /// an unsupported error so adapters can opt in incrementally;
    /// the production `SqliteMemoryStore` (Task 4) and `FixtureStore` (Task 5)
    /// override.
    async fn index_stats(&self) -> Result<IndexStats, StoreError> {
        Err("index_stats: not supported by this store adapter".into())
    }

    /// Per-record consent storage model (Issue #253).
    ///
    /// The default impl returns the literal `"not supported by this store
    /// adapter"` error so adapters that have not opted in cannot silently
    /// downgrade every record to `LegacyEvent` and disable §6.5
    /// enforcement. Callers in `lint` recognize this sentinel and surface
    /// it as an error-severity finding rather than treating the empty
    /// state as authoritative — see `cairn-cli::verbs::lint::lint_handler`.
    /// Adapters that genuinely have no per-record consent metadata (test
    /// fixtures, legacy-only stores) override this to return
    /// `Ok(HashMap::new())` and accept the legacy-only posture explicitly.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the underlying read fails, or the
    /// "not supported" sentinel for adapters that have not opted in.
    async fn list_consent_models(
        &self,
    ) -> Result<
        std::collections::HashMap<RecordId, crate::domain::consent_timeline::ConsentModel>,
        StoreError,
    > {
        Err("list_consent_models: not supported by this store adapter".into())
    }

    /// Borrow the adapter's consent-timeline reader, when it ships one.
    ///
    /// Issue #253 (round 4): `ConsentLookup` is a separate trait, but
    /// the plugin registry only stores `Arc<dyn MemoryStore>` — once a
    /// store is registered, the host has no way to recover a
    /// `&dyn ConsentLookup` from that erased object. This default
    /// returns `None` so adapters built against the 0.2 surface remain
    /// dyn-compatible; adapters that implement `ConsentLookup` (e.g.
    /// `SqliteMemoryStore`) override this to return `Some(self)` so
    /// `lint` can resolve covering grants without a side channel.
    fn as_consent_lookup(&self) -> Option<&dyn crate::contract::consent_lookup::ConsentLookup> {
        None
    }

    // ── Bitemporal knowledge graph (#186) ─────────────────────────────────

    /// Upsert an entity node. Deduplication is by `name_norm` (UNIQUE in
    /// storage): if a row with this `name_norm` exists, return its id;
    /// otherwise insert a fresh row and return the new id. Idempotent.
    ///
    /// # Errors
    /// Default impl returns `"capability unavailable: bitemporal_graph"`.
    /// Concrete adapters return [`StoreError`] on backend failures.
    async fn upsert_entity(
        &self,
        node: &crate::domain::graph::EntityNode,
    ) -> Result<crate::domain::graph::EntityId, StoreError> {
        let _ = node;
        Err("capability unavailable: bitemporal_graph".into())
    }

    /// Upsert a bitemporal entity edge.
    ///
    /// If a live edge for `(source_id, target_id, relation)` exists with a
    /// different `body_hash`, contradiction-resolves: invalidates the old
    /// (sets `invalid_at = new.valid_at`) and inserts the new in one atomic
    /// op. Identical-body re-upsert is a no-op (`body_was_unchanged: true`,
    /// no WAL row written) — mirrors [`MemoryStore::upsert`] idempotency.
    ///
    /// # Errors
    /// Default impl returns `"capability unavailable: bitemporal_graph"`.
    async fn upsert_entity_edge(
        &self,
        edge: &crate::domain::graph::EntityEdge,
    ) -> Result<crate::domain::graph::EntityEdgeOutcome, StoreError> {
        let _ = edge;
        Err("capability unavailable: bitemporal_graph".into())
    }

    /// Read edges adjacent to a node. Supports direction (in/out/both),
    /// relation-filter, and bitemporal as-of slicing.
    ///
    /// # Errors
    /// Default impl returns `"capability unavailable: bitemporal_graph"`.
    async fn graph_edges(
        &self,
        args: &crate::domain::graph::GraphEdgesArgs<'_>,
    ) -> Result<Vec<crate::domain::graph::EntityEdge>, StoreError> {
        let _ = args;
        Err("capability unavailable: bitemporal_graph".into())
    }

    /// Explicit contradiction resolution — invalidate `old_edge_id` and
    /// insert `new_edge` in one atomic op. Mostly an internal hook used by
    /// [`MemoryStore::upsert_entity_edge`]; exposed for batch contradiction
    /// callers (e.g. `lint --fix-graph`).
    ///
    /// # Errors
    /// Default impl returns `"capability unavailable: bitemporal_graph"`.
    async fn resolve_contradiction(
        &self,
        old_edge_id: &crate::domain::graph::EntityEdgeId,
        new_edge: &crate::domain::graph::EntityEdge,
    ) -> Result<crate::domain::graph::EntityEdgeOutcome, StoreError> {
        let _ = (old_edge_id, new_edge);
        Err("capability unavailable: bitemporal_graph".into())
    }

    /// Link an entity to a record that mentions it. Idempotent — returns
    /// `Ok(true)` when a new link was inserted, `Ok(false)` when the link
    /// already existed.
    ///
    /// # Errors
    /// Default impl returns `"capability unavailable: bitemporal_graph"`.
    async fn link_entity_episode(
        &self,
        entity_id: &crate::domain::graph::EntityId,
        record_id: &RecordId,
    ) -> Result<bool, StoreError> {
        let _ = (entity_id, record_id);
        Err("capability unavailable: bitemporal_graph".into())
    }
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
    BodyHash, RecordId, ScopeTuple, TargetId,
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
    /// Schema version stamped by the store at write time, or `None`
    /// for unstamped legacy rows (pre-Issue #258 migration). Drives
    /// the §6.4 `stale_schema` lint; not part of the canonical record
    /// bytes.
    pub schema_version: Option<SchemaVersion>,
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
///
/// Scope tuple narrowing — tenant / workspace / entity / user / agent /
/// session — is NOT a field on this struct. Callers must compose scope
/// constraints into the `filter` tree or the `visibility_allowlist` before
/// invoking the store. See the docstring on
/// [`MemoryStore::search_keyword`] for the rationale.
#[derive(Debug, Clone)]
pub struct KeywordSearchArgs<'a> {
    /// Raw FTS5 expression. Store does not validate FTS5 syntax; `SQLite`
    /// surfaces parse errors which the store re-wraps in PR-B as a typed
    /// FTS error variant on `StoreError`.
    pub query: String,
    /// Pre-validated filter tree from
    /// [`crate::domain::filter::validate_filter`]. Recall-narrowing only
    /// (kind/class/tags/timestamps). Authorization predicates (scope,
    /// visibility) MUST go through `auth_scope` and `visibility_allowlist`
    /// — see Issue #191 for the rationale.
    pub filter: Option<ValidatedFilter<'a>>,
    /// Authorization scope tuple — the security predicate. The store
    /// applies it identically across this leg, the semantic leg, the
    /// graph leg, and graph hydration so policy cannot drift between
    /// retrieval paths. Use [`ScopeTuple::default()`] when no narrowing
    /// is required; a populated tuple narrows on each non-`None`
    /// dimension. Issue #191.
    pub auth_scope: ScopeTuple,
    /// Visibility values the caller is allowed to see; empty = no filter.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Maximum number of candidates to return in this page.
    pub limit: usize,
    /// Optional resume cursor from the previous page.
    pub cursor: Option<KeywordCursor>,
    /// When true, the store populates the page's `explain` block.
    /// Callers are expected to set this only when `--explain` was requested
    /// (and the `policy_trace` capability is advertised — gating happens
    /// in the verb dispatcher, not the store).
    pub with_explain: bool,
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
    /// Optional per-candidate score-component explanations. Present only
    /// when the matching args' `with_explain` was true. For the keyword
    /// page, only `bm25_rank` is populated.
    pub explain: Option<Vec<ScoreExplain>>,
}

/// Args for the semantic (ANN) branch of `search`.
///
/// No cursor: ANN is top-K only at v0.1. Scope-resolution rules are
/// identical to [`KeywordSearchArgs`] — callers fold scope into `filter`
/// or `visibility_allowlist` before invoking.
#[derive(Debug, Clone)]
pub struct SemanticSearchArgs<'a> {
    /// Raw user query. The embedder applies any asymmetric prefix internally.
    pub query: String,
    /// Pre-validated filter tree. Same semantics as in [`KeywordSearchArgs`].
    pub filter: Option<ValidatedFilter<'a>>,
    /// Authorization scope tuple — see [`KeywordSearchArgs::auth_scope`].
    pub auth_scope: ScopeTuple,
    /// Visibility values the caller is allowed to see; empty = no filter.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Number of nearest neighbours to return (top-K).
    pub limit: usize,
    /// Label of the active embedding model (e.g., `"bge-small-en-v1.5"`).
    /// The store skips rows whose `record_vectors.model` column differs —
    /// they were produced by a stale model and will be rebuilt by the reindex drain.
    pub model_label: String,
    /// When true, the store populates the page's `explain` block. See
    /// [`KeywordSearchArgs::with_explain`].
    pub with_explain: bool,
}

/// One page of candidates returned by the semantic branch of `search`.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchPage {
    /// Candidates ordered by ascending L2 distance (smaller = more similar).
    pub candidates: Vec<SearchCandidate>,
    /// Optional per-candidate score-component explanations. Present only
    /// when the matching args' `with_explain` was true. For the semantic
    /// page, only `semantic_rank` is populated.
    pub explain: Option<Vec<ScoreExplain>>,
}

/// Args for the hybrid (RRF + cosine re-rank) branch of `search`.
///
/// Composes the keyword and semantic legs with their shared filter /
/// visibility narrowing, then carries the RRF + re-rank tuning knobs
/// (`rrf_k`, `rerank_topk`, `blend`) through to the orchestrator. Scope
/// resolution is the caller's responsibility — same rules as
/// [`KeywordSearchArgs`] / [`SemanticSearchArgs`].
#[derive(Debug, Clone)]
pub struct HybridSearchArgs<'a> {
    /// Raw user query.
    pub query: String,
    /// Pre-validated filter tree from
    /// [`crate::domain::filter::validate_filter`]. Same semantics as in
    /// [`KeywordSearchArgs`].
    pub filter: Option<ValidatedFilter<'a>>,
    /// Authorization scope tuple — see [`KeywordSearchArgs::auth_scope`].
    pub auth_scope: ScopeTuple,
    /// Visibility values the caller is allowed to see; empty = no filter.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Number of results.
    pub limit: usize,
    /// Active embedding model label. Vectors with a different label are excluded.
    pub model_label: String,
    /// Blend coefficient (0.0–1.0). `1.0` skips cosine re-rank.
    pub blend: f32,
    /// RRF constant. Canonical default `60`.
    pub rrf_k: usize,
    /// Top-K from RRF to second-pass re-rank with cosine. Canonical default `20`.
    pub rerank_topk: usize,
    /// When true, the store populates the page's `explain` block. See
    /// [`KeywordSearchArgs::with_explain`].
    pub with_explain: bool,
    /// Floor on per-graph-candidate confidence weight in the RRF leg.
    /// Default `1e-3`. Issue #191.
    pub confidence_floor: f32,
}

/// Args for [`MemoryStore::search_graph_neighbors`] (Issue #191, spec §4.3).
///
/// Authorization predicates (`auth_scope`, `visibility_allowlist`) apply to
/// both the edge provenance record and the neighbor record; `filter` is
/// recall-narrowing only and applies to the neighbor record. See spec
/// §4.3 "Predicate application" for the full table.
#[derive(Debug, Clone)]
pub struct GraphNeighborsArgs<'a> {
    /// Record ids from auth-only seed retrieval (UNION-ed across keyword
    /// and semantic legs), up to `2 * GRAPH_SEED_OVERFETCH = 400`.
    /// Seeds the graph traversal; an empty list yields an empty result.
    pub seed_record_ids: Vec<RecordId>,
    /// Record ids actually fused into RRF (top of each filtered lexical
    /// leg, UNION-ed, ≤ 100). Used as the dedup set in step 4 of §5.1 —
    /// graph results exclude these so RRF cannot double-count, but seeds
    /// not in this list (overfetched rank 51-200 records) remain
    /// eligible for rank-rescue via graph evidence.
    pub ranked_record_ids: Vec<RecordId>,
    /// Pre-validated user-narrowing filter. Applied **only** to the
    /// returned neighbor record. User narrowing must not erase
    /// otherwise-authorized edges based on where the edge was observed.
    pub filter: Option<ValidatedFilter<'a>>,
    /// Authorization scope tuple — applied to BOTH provenance and
    /// neighbor. See [`KeywordSearchArgs::auth_scope`].
    pub auth_scope: ScopeTuple,
    /// Visibility values the caller is allowed to see. Applied to BOTH
    /// the neighbor record and the edge provenance record.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Max candidates returned. Equals `HYBRID_LEG_LIMIT` from the
    /// orchestrator.
    pub limit: usize,
    /// `confidence_score` floor applied SQL-side to the edge step.
    /// Edges below this threshold are excluded entirely.
    pub confidence_min: f32,
}

/// One page of hybrid candidates.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridSearchPage {
    /// Candidates, sorted descending by blended `final_score`.
    pub candidates: Vec<SearchCandidate>,
    /// Optional per-candidate score-component explanations. Present only
    /// when the matching args' `with_explain` was true. For the hybrid
    /// page, all fields are populated where applicable.
    pub explain: Option<Vec<ScoreExplain>>,
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
    /// L2 distance from the query vector. `None` on keyword-only candidates.
    /// Used by the hybrid ranker (follow-up issue); always `None` from `search_keyword`.
    pub semantic_distance: Option<f32>,
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
                per_record_consent_model: false,
                graph_search: false,
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
            VersionRange::new(ContractVersion::new(0, 5, 0), ContractVersion::new(0, 6, 0));
    }

    #[tokio::test]
    async fn dyn_compatible() {
        let s: Box<dyn MemoryStore> = Box::new(StubStore);
        assert_eq!(s.name(), "stub");
        assert!(s.capabilities().fts);
        assert!(s.supported_contract_versions().accepts(CONTRACT_VERSION));
        let id = RecordId::parse("01HQZX9F5N0000000000000000".to_owned()).expect("valid id");
        assert!(s.get(&id).await.unwrap().is_none());
        assert!(
            s.list(&ListArgs::default())
                .await
                .unwrap()
                .records
                .is_empty()
        );
        let sem_result = s
            .search_semantic(&SemanticSearchArgs {
                query: "test".into(),
                filter: None,
                auth_scope: ScopeTuple::default(),
                visibility_allowlist: vec![],
                limit: 10,
                model_label: "bge-small-en-v1.5".into(),
                with_explain: false,
            })
            .await;
        assert!(
            sem_result.is_err(),
            "default search_semantic must return error"
        );
        let hybrid_result = s
            .search_hybrid(&HybridSearchArgs {
                query: "test".into(),
                filter: None,
                auth_scope: ScopeTuple::default(),
                visibility_allowlist: vec![],
                limit: 10,
                model_label: "bge-small-en-v1.5".into(),
                blend: 0.7,
                rrf_k: 60,
                rerank_topk: 20,
                with_explain: false,
                confidence_floor: 1e-3,
            })
            .await;
        assert!(
            hybrid_result.is_err(),
            "default search_hybrid must return error"
        );
    }

    #[test]
    fn static_consts_accessible() {
        assert_eq!(StubStore::NAME, "stub");
        assert!(StubStore::SUPPORTED_VERSIONS.accepts(CONTRACT_VERSION));
    }

    #[test]
    fn index_stats_is_constructible_and_carries_counts() {
        let s = IndexStats {
            records_active: 10,
            fts5_rows: 10,
        };
        assert_eq!(s.records_active, 10);
        assert_eq!(s.fts5_rows, 10);
    }

    #[tokio::test]
    async fn issue_186_default_impls_report_capability_unavailable() {
        use crate::domain::graph::{
            EdgeConfidence, EntityEdge, EntityEdgeId, EntityId, EntityNode, GraphEdgesArgs,
            normalize_entity_name,
        };
        use crate::domain::record::RecordId;

        let store = StubStore;

        let node = EntityNode {
            id: EntityId::from("01HZE7JV5N0000000000000001"),
            name: "alice".into(),
            name_norm: normalize_entity_name("alice").expect("non-empty literal"),
            summary: None,
            created_at: 1,
            embedding_id: None,
        };
        let err = store.upsert_entity(&node).await.unwrap_err();
        assert!(err.to_string().contains("bitemporal_graph"));

        let edge = EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N0000000000000002"),
            source_id: node.id.clone(),
            target_id: node.id.clone(),
            relation: "self".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 1,
            invalid_at: None,
            created_at: 1,
            source_record_id: None,
        };
        let err = store.upsert_entity_edge(&edge).await.unwrap_err();
        assert!(err.to_string().contains("bitemporal_graph"));

        let id = EntityId::from("01HZE7JV5N0000000000000001");
        let args = GraphEdgesArgs {
            node_id: &id,
            direction: EdgeDir::Both,
            relation_filter: None,
            as_of_event_time: None,
            as_of_ingest_time: None,
            include_invalidated: false,
        };
        let err = store.graph_edges(&args).await.unwrap_err();
        assert!(err.to_string().contains("bitemporal_graph"));

        let old_id = EntityEdgeId::from("01HZE7JV5N0000000000000003");
        let err = store
            .resolve_contradiction(&old_id, &edge)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("bitemporal_graph"));

        let rec_id = RecordId::parse("01HQZX9F5N0000000000000000".to_owned()).unwrap();
        let err = store
            .link_entity_episode(&node.id, &rec_id)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("bitemporal_graph"));
    }

    /// `CONTRACT_VERSION` for the `MemoryStore` trait is locked to 0.5.0.
    /// #258 bumped 0.3→0.4 (per-row `schema_version`). #191 bumped 0.4→0.5
    /// (`auth_scope`, `graph_search` capability + trait method).
    #[test]
    fn contract_version_locked_to_0_5_0() {
        assert_eq!(CONTRACT_VERSION, ContractVersion::new(0, 5, 0));
    }
}
