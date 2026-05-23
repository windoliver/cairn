//! In-memory two-node setup for federation propose→drain→accept e2e tests.
//!
//! `tests/common/mod.rs` lives under `cairn-core/tests/`, which is NOT
//! visible from `cairn-workflows/tests/` (Cargo test crates are isolated
//! per-package). The federation verb fakes are small enough to duplicate
//! here rather than hoist into a shared crate; a follow-up may
//! consolidate if the duplication starts to drift.
//!
//! ## Layout
//!
//! - [`InMemoryStore`] — `MemoryStore` fake (the e2e tests only touch
//!   `upsert` + `get`).
//! - [`InMemoryOutbox`] — `FederationOutbox` fake. Captures both consent
//!   events and propagation jobs; tests drain jobs by `kind`.
//! - [`InMemoryConsentLookup`] — `ConsentLookup` fake supporting dedup +
//!   revocation lookups on the receiver side.
//! - [`Node`] — bundles store/outbox/consent-lookup with the identity,
//!   key, clock, and `ReBAC` context for one side of the federation.
//!
//! Two nodes (issuer + receiver) are constructed by [`build_issuer`] and
//! [`build_receiver`]. They share NO state — each is a complete vault.

#![allow(
    dead_code,
    reason = "shared helper module; not every helper is used by every test"
)]

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cairn_core::contract::consent_lookup::{
    ConsentLookup, ConsentLookupError, FederationAcceptRecord, StoredRevocation, StoredShareLink,
};
use cairn_core::contract::federation_outbox::{FederationOutbox, FederationOutboxError};
use cairn_core::contract::job_store::{EnqueueRequest, JobPayload};
use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, Edge, EdgeDir, EdgeKey, KeywordSearchArgs, KeywordSearchPage, ListArgs,
    ListPage, MemoryStore, MemoryStoreCapabilities, RecordVersion, StoreError, TombstoneReason,
    UpsertOutcome,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::consent_timeline::ConsentTimelineEvent;
use cairn_core::domain::federation::{DedupKey, PeerEndpoint};
use cairn_core::domain::identity::keys::SigningKey;
use cairn_core::domain::sharing::SignedShareLink;
use cairn_core::domain::time::FixedClock;
use cairn_core::domain::{
    ActorChainEntry, BodyHash, ChainRole, ConsentEvent, ConsentKind, Ed25519Signature,
    EvidenceVector, Identity, MemoryClass, MemoryKind, MemoryRecord, MemoryVisibility, Provenance,
    RecordId, Rfc3339Timestamp, ScopeTuple, SourceId, TargetId,
};
use cairn_core::rebac::{RebacAction, RebacContext, RebacRelation};
use cairn_core::verbs::accept_share::AcceptShareDeps;
use cairn_core::verbs::propose_share::ProposeShareDeps;
use cairn_core::verbs::revoke_share::RevokeShareDeps;
use chrono::{DateTime, Utc};
use ulid::Ulid;

// ─── In-memory MemoryStore ────────────────────────────────────────────

#[derive(Default)]
pub struct InMemoryStore {
    inner: Mutex<HashMap<String, MemoryRecord>>,
}

#[async_trait]
impl MemoryStore for InMemoryStore {
    fn name(&self) -> &'static str {
        "in-memory-e2e"
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: false,
            vector: false,
            graph_edges: false,
            transactions: false,
            per_record_consent_model: false,
            graph_search: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(
            CONTRACT_VERSION,
            ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
        )
    }

    async fn upsert(&self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        let mut guard = self.inner.lock().expect("store mutex poisoned");
        guard.insert(record.id.as_str().to_owned(), record.clone());
        Ok(UpsertOutcome {
            record_id: record.id.clone(),
            target_id: record.target_id.clone(),
            version: 1,
            content_changed: true,
            prior_hash: None,
        })
    }

    async fn get(&self, id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
        let guard = self.inner.lock().expect("store mutex poisoned");
        Ok(guard.get(id.as_str()).cloned())
    }

    async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
        Ok(ListPage {
            records: Vec::new(),
            next_cursor: None,
        })
    }

    async fn tombstone(&self, id: &RecordId, _reason: TombstoneReason) -> Result<(), StoreError> {
        let mut guard = self.inner.lock().expect("store mutex poisoned");
        guard.remove(id.as_str());
        Ok(())
    }

    async fn versions(&self, target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
        let guard = self.inner.lock().expect("store mutex poisoned");
        let out: Vec<_> = guard
            .values()
            .filter(|r| r.target_id == *target)
            .map(|r| RecordVersion {
                record_id: r.id.clone(),
                target_id: r.target_id.clone(),
                version: 1,
                created_at: 0,
                updated_at: 0,
                active: true,
                tombstoned: false,
                tombstone_reason: None,
                body_hash: BodyHash::compute(&r.body),
                schema_version: None,
            })
            .collect();
        Ok(out)
    }

    async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
        Ok(())
    }

    async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
        Ok(false)
    }

    async fn neighbours(&self, _id: &RecordId, _dir: EdgeDir) -> Result<Vec<Edge>, StoreError> {
        Ok(Vec::new())
    }

    async fn search_keyword(
        &self,
        _args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError> {
        Err("e2e InMemoryStore: search_keyword unavailable".into())
    }
}

// ─── In-memory FederationOutbox ───────────────────────────────────────

/// Federation state shared between [`InMemoryOutbox`] and
/// [`InMemoryConsentLookup`]. Mirrors what a real adapter (T12) would
/// materialise in the consent-timeline projection from inside the same
/// outbox transaction: a row per stored share link, per revocation, and
/// per applied accept.
///
/// Wrapped in `Arc<Mutex<…>>` so the outbox can append on the write
/// path while the lookup queries on the read path without each side
/// owning its own copy.
#[derive(Default)]
struct FederationProjection {
    /// Share links the issuer-side outbox saw on `record_share_grant`
    /// (parsed from the job payload, which is the canonical
    /// `SignedShareLink`).
    share_links: BTreeMap<String, StoredShareLink>,
    /// Revocations the issuer-side outbox saw on
    /// `record_share_revoke_grant`.
    revocations: BTreeMap<String, StoredRevocation>,
    /// Receiver-side accept rows recorded on `record_share_accept`,
    /// keyed by `(issuer_key_id, link_id)`.
    accepts: BTreeMap<String, FederationAcceptRecord>,
    /// Set of link ids the receiver has revoked (for
    /// `is_link_revoked`).
    revoked_link_ids: std::collections::HashSet<String>,
    /// All consent events the outbox has been asked to commit, in
    /// arrival order. Used by tests to assert "the journal saw
    /// `FederationAccept` + `FederationRevoke`" without reaching into
    /// the (still-unimplemented) timeline projection.
    consent_events: Vec<ConsentEvent>,
}

#[derive(Default)]
pub struct InMemoryOutbox {
    inner: Mutex<OutboxState>,
    projection: Arc<Mutex<FederationProjection>>,
}

impl InMemoryOutbox {
    /// Build an outbox that shares its federation projection with
    /// `projection` so an [`InMemoryConsentLookup`] backed by the same
    /// `Arc` sees grants / revokes / accepts as soon as the outbox
    /// commits them. Mirrors what T12's real adapter does inside one
    /// SQL transaction.
    #[must_use]
    fn with_projection(projection: Arc<Mutex<FederationProjection>>) -> Self {
        Self {
            inner: Mutex::new(OutboxState::default()),
            projection,
        }
    }
}

#[derive(Default)]
struct OutboxState {
    jobs: Vec<EnqueueRequest>,
    upserts: Vec<MemoryRecord>,
    tombstoned_ids: std::collections::HashSet<String>,
}

#[async_trait]
impl FederationOutbox for InMemoryOutbox {
    async fn record_share_grant(
        &self,
        event: &cairn_core::domain::ConsentEvent,
        job: EnqueueRequest,
    ) -> Result<(), FederationOutboxError> {
        let mut guard = self.inner.lock().expect("outbox mutex poisoned");
        if let Some(dedupe_key) = job.dedupe_key.as_ref()
            && guard
                .jobs
                .iter()
                .any(|j| j.dedupe_key.as_deref() == Some(dedupe_key.as_str()))
        {
            return Err(FederationOutboxError::DuplicateJob);
        }
        // Mirror what T12's adapter does: parse the canonical job
        // payload back into a SignedShareLink and stash a
        // StoredShareLink keyed by link_id so the issuer-side
        // revoke_share verb can find the original grant.
        if let Ok(link) = serde_json::from_slice::<SignedShareLink>(&job.payload) {
            let mut proj = self.projection.lock().expect("projection mutex poisoned");
            proj.share_links.insert(
                link.link_id.clone(),
                StoredShareLink {
                    link_id: link.link_id.clone(),
                    payload: link.payload.clone(),
                    peer: extract_peer_from_queue_key(job.queue_key.as_deref()),
                },
            );
            proj.consent_events.push(event.clone());
        }
        guard.jobs.push(job);
        Ok(())
    }

    async fn record_share_accept(
        &self,
        event: &cairn_core::domain::ConsentEvent,
        upserts: &[MemoryRecord],
    ) -> Result<(), FederationOutboxError> {
        let mut guard = self.inner.lock().expect("outbox mutex poisoned");
        guard.upserts.extend(upserts.iter().cloned());
        drop(guard);
        let mut proj = self.projection.lock().expect("projection mutex poisoned");
        proj.consent_events.push(event.clone());
        Ok(())
    }

    async fn record_share_revoke(
        &self,
        event: &cairn_core::domain::ConsentEvent,
        tombstone_ids: &[String],
    ) -> Result<(), FederationOutboxError> {
        // Track tombstoned record IDs so `try_fetch` can filter them
        // out after a revoke.
        let mut guard = self.inner.lock().expect("outbox mutex poisoned");
        guard.tombstoned_ids.extend(tombstone_ids.iter().cloned());
        drop(guard);
        let mut proj = self.projection.lock().expect("projection mutex poisoned");
        // Mark the link as revoked from the receiver's point of view
        // so a follow-up propose against the same link short-circuits
        // via `is_link_revoked`.
        if let cairn_core::domain::ConsentPayload::FederationRevoke { link_id, .. } = &event.payload
        {
            proj.revoked_link_ids.insert(link_id.clone());
        }
        proj.consent_events.push(event.clone());
        Ok(())
    }

    async fn record_share_revoke_grant(
        &self,
        event: &cairn_core::domain::ConsentEvent,
        job: EnqueueRequest,
    ) -> Result<(), FederationOutboxError> {
        let mut guard = self.inner.lock().expect("outbox mutex poisoned");
        if let Some(dedupe_key) = job.dedupe_key.as_ref()
            && guard
                .jobs
                .iter()
                .any(|j| j.dedupe_key.as_deref() == Some(dedupe_key.as_str()))
        {
            return Err(FederationOutboxError::DuplicateJob);
        }
        // Mirror what T12's adapter does for the revoke side: parse
        // the canonical job payload back into a SignedRevocation and
        // stash a StoredRevocation so the verb's idempotent replay
        // can find it. The op_id on the consent event ties the row to
        // the WAL operation id.
        if let Ok(revocation) =
            serde_json::from_slice::<cairn_core::domain::federation::SignedRevocation>(&job.payload)
        {
            let mut proj = self.projection.lock().expect("projection mutex poisoned");
            let operation_id = event.op_id.clone().unwrap_or_default();
            proj.revocations.insert(
                revocation.link_id.clone(),
                StoredRevocation {
                    operation_id,
                    revocation,
                },
            );
            proj.consent_events.push(event.clone());
        }
        guard.jobs.push(job);
        Ok(())
    }
}

impl InMemoryOutbox {
    pub fn jobs_of_kind(&self, kind: &str) -> Vec<EnqueueRequest> {
        let guard = self.inner.lock().expect("outbox mutex poisoned");
        guard
            .jobs
            .iter()
            .filter(|j| j.kind.as_str() == kind)
            .cloned()
            .collect()
    }

    pub fn upserts(&self) -> Vec<MemoryRecord> {
        let guard = self.inner.lock().expect("outbox mutex poisoned");
        guard.upserts.clone()
    }

    pub fn is_tombstoned(&self, record_id: &str) -> bool {
        let guard = self.inner.lock().expect("outbox mutex poisoned");
        guard.tombstoned_ids.contains(record_id)
    }
}

/// Recover the symbolic peer code from the
/// `federation:<peer>:<link_id>` queue key shape that
/// `propose_share::build_propagation_job` writes. Best-effort — if the
/// shape doesn't match we just return `None` and the revoke job
/// inherits the default `federation:<link>` shape.
fn extract_peer_from_queue_key(queue_key: Option<&str>) -> Option<String> {
    let key = queue_key?;
    let rest = key.strip_prefix("federation:")?;
    let (peer, _link) = rest.split_once(':')?;
    Some(peer.to_owned())
}

// ─── In-memory ConsentLookup ──────────────────────────────────────────

/// Lookup fake backed by a [`FederationProjection`] shared with the
/// outbox. Every write the outbox commits is visible here on the next
/// read — matching T12's adapter, which materialises the projection in
/// the same SQL transaction as the journal row.
#[derive(Default)]
pub struct InMemoryConsentLookup {
    /// Test-only side channel for accepts the test itself records via
    /// [`Self::record_accept`] (the receiver's outbox does not yet
    /// surface a parsed `FederationAcceptRecord`, so the
    /// dedup-replay test feeds it in manually).
    accepts: Mutex<BTreeMap<String, FederationAcceptRecord>>,
    projection: Arc<Mutex<FederationProjection>>,
}

impl InMemoryConsentLookup {
    #[must_use]
    fn with_projection(projection: Arc<Mutex<FederationProjection>>) -> Self {
        Self {
            accepts: Mutex::new(BTreeMap::new()),
            projection,
        }
    }
}

#[async_trait]
impl ConsentLookup for InMemoryConsentLookup {
    async fn timeline(
        &self,
        _consent_ref: &str,
    ) -> Result<Vec<ConsentTimelineEvent>, ConsentLookupError> {
        Ok(Vec::new())
    }

    async fn find_federation_accept(
        &self,
        dedup: DedupKey<'_>,
    ) -> Result<Option<FederationAcceptRecord>, ConsentLookupError> {
        let guard = self.accepts.lock().expect("consent-lookup mutex poisoned");
        Ok(guard.get(&dedup_key_string(&dedup)).cloned())
    }

    async fn is_link_revoked(&self, link_id: &str) -> Result<bool, ConsentLookupError> {
        let proj = self.projection.lock().expect("projection mutex poisoned");
        // The `ConsentPayload::FederationRevoke.link_id` field stores the
        // inner operation-id ULID (i.e. without the `"share-"` prefix),
        // because `accept_share::build_revoke_event` strips it via
        // `extract_operation_id`. Accept both the outer `"share-<ULID>"` form
        // and the raw inner ULID so callers using either form hit the same
        // bucket.
        if proj.revoked_link_ids.contains(link_id) {
            return Ok(true);
        }
        if let Some(inner) = link_id.strip_prefix("share-") {
            return Ok(proj.revoked_link_ids.contains(inner));
        }
        Ok(false)
    }

    async fn find_share_link(
        &self,
        link_id: &str,
    ) -> Result<Option<StoredShareLink>, ConsentLookupError> {
        let proj = self.projection.lock().expect("projection mutex poisoned");
        Ok(proj.share_links.get(link_id).cloned())
    }

    async fn find_revocation(
        &self,
        link_id: &str,
    ) -> Result<Option<StoredRevocation>, ConsentLookupError> {
        let proj = self.projection.lock().expect("projection mutex poisoned");
        // The verb keys revocations by the *outer* `share-…` link id
        // (the same value `find_share_link` accepts), but T9 stores
        // them under the inner ULID operation id (the wire
        // `SignedRevocation.link_id`). Accept either to keep the
        // adapter contract honest under both keying conventions.
        if let Some(found) = proj.revocations.get(link_id) {
            return Ok(Some(found.clone()));
        }
        if let Some(inner) = link_id.strip_prefix("share-")
            && let Some(found) = proj.revocations.get(inner)
        {
            return Ok(Some(found.clone()));
        }
        Ok(None)
    }
}

impl InMemoryConsentLookup {
    pub fn record_accept(
        &self,
        issuer_key_id: &str,
        link_id: &str,
        record: FederationAcceptRecord,
    ) {
        let mut guard = self.accepts.lock().expect("consent-lookup mutex poisoned");
        guard.insert(dedup_key_string_owned(issuer_key_id, link_id), record);
    }
}

fn dedup_key_string(d: &DedupKey<'_>) -> String {
    dedup_key_string_owned(d.issuer_key_id, d.link_id)
}

fn dedup_key_string_owned(issuer_key_id: &str, link_id: &str) -> String {
    format!("{issuer_key_id}|{link_id}")
}

// ─── Node bundle ──────────────────────────────────────────────────────

/// One side of the federation: store, outbox, consent-lookup, identity,
/// key, clock, and `ReBAC` context. Two nodes are built independently
/// (no shared state) — they represent two separate vaults.
pub struct Node {
    pub store: InMemoryStore,
    pub outbox: InMemoryOutbox,
    pub consent_lookup: InMemoryConsentLookup,
    pub identity: Identity,
    pub signing_key: SigningKey,
    /// Issuer-side: own verifying key. Receiver-side: ISSUER's verifying
    /// key (cached so the receiver can verify inbound envelope
    /// signatures without re-deriving every call).
    pub issuer_verifying_key: ed25519_dalek::VerifyingKey,
    pub inbound_sensor: Identity,
    pub clock: FixedClock,
    pub rebac: RebacContext,
    pub scope: ScopeTuple,
    pub federation_ready: bool,
}

impl Node {
    #[must_use]
    pub fn identity(&self) -> Identity {
        self.identity.clone()
    }

    #[must_use]
    pub fn scope(&self) -> ScopeTuple {
        self.scope.clone()
    }

    /// Fixed expiry one hour after the clock's "now" — handy for share
    /// links whose `expires_at` must be strictly after `issued_at`.
    #[must_use]
    pub fn in_one_hour(&self) -> Rfc3339Timestamp {
        let _ = self;
        Rfc3339Timestamp::parse("2026-05-21T13:00:00Z").expect("invariant: literal must parse")
    }

    /// Insert a body-bearing record at `Team` tier so the issuer's
    /// `propose_share` can find it via `store.get`.
    pub async fn insert_project_record(&self, body: &str) -> String {
        let id = format!(
            "01HQZX9F5N0{}",
            Ulid::new().to_string().chars().take(15).collect::<String>()
        );
        let now = Rfc3339Timestamp::parse("2026-05-21T11:00:00Z").expect("ts");
        let source_id = SourceId::parse("01HQZX9F5N0000000000000001").expect("source id");
        let record = MemoryRecord {
            id: RecordId::parse(id.clone()).expect("record id"),
            target_id: TargetId::parse(id.clone()).expect("target id"),
            kind: MemoryKind::User,
            class: MemoryClass::Semantic,
            visibility: MemoryVisibility::Team,
            scope: self.scope.clone(),
            body: body.to_owned(),
            source_ids: vec![source_id.clone()],
            provenance: Provenance {
                source_sensor: Identity::parse("snr:local:cli:test:v1").expect("sensor"),
                created_at: now.clone(),
                originating_agent_id: self.identity.clone(),
                source_ids: vec![source_id],
                source_hash: format!("sha256:{}", "a".repeat(64)),
                consent_ref: "consent:test:propose".to_owned(),
                llm_id_if_any: None,
                source_refs: Vec::new(),
            },
            updated_at: now.clone(),
            evidence: EvidenceVector::default(),
            salience: 0.5,
            confidence: 0.7,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: self.identity.clone(),
                at: now,
            }],
            signature: Ed25519Signature::parse(format!("ed25519:{}", "0".repeat(128)))
                .expect("sig"),
            tags: Vec::new(),
            extra_frontmatter: BTreeMap::new(),
            consent_model: None,
        };
        self.store.upsert(&record).await.expect("seed record");
        id
    }

    /// Drain the first pending job of `kind` out of the outbox and
    /// return its raw payload bytes — the same bytes
    /// `PropagationHandler::handle` expects.
    pub async fn next_pending_payload(&self, kind: &str) -> JobPayload {
        tokio::task::yield_now().await;
        let jobs = self.outbox.jobs_of_kind(kind);
        let job = jobs
            .into_iter()
            .next()
            .expect("expected at least one pending propagation job");
        job.payload
    }

    /// Issuer-side dependency snapshot for `propose_share`.
    #[must_use]
    pub fn propose_deps(&self) -> ProposeShareDeps<'_> {
        ProposeShareDeps {
            store: &self.store,
            outbox: &self.outbox,
            signing_key: &self.signing_key,
            signer_identity: &self.identity,
            signer_key_version: 1,
            rebac: &self.rebac,
            clock: &self.clock,
            federation_ready: self.federation_ready,
        }
    }

    /// Receiver-side dependency snapshot for `accept_share`.
    #[must_use]
    pub fn accept_deps(&self) -> AcceptShareDeps<'_> {
        AcceptShareDeps {
            store: &self.store,
            outbox: &self.outbox,
            consent_lookup: &self.consent_lookup,
            local_signing_key: &self.signing_key,
            receiver_identity: &self.identity,
            issuer_verifying_key: &self.issuer_verifying_key,
            rebac: &self.rebac,
            clock: &self.clock,
            inbound_sensor: &self.inbound_sensor,
            federation_ready: self.federation_ready,
        }
    }

    /// Issuer-side dependency snapshot for `revoke_share`.
    ///
    /// Issuer-side parallel to [`Self::propose_deps`]. The signing
    /// key, `key_version`, and identity must match the original
    /// propose so the revocation signature verifies against the same
    /// issuer pubkey the receiver cached when applying the link.
    #[must_use]
    pub fn revoke_deps(&self) -> RevokeShareDeps<'_> {
        RevokeShareDeps {
            outbox: &self.outbox,
            consent_lookup: &self.consent_lookup,
            signing_key: &self.signing_key,
            signer_identity: &self.identity,
            signer_key_version: 1,
            clock: &self.clock,
            federation_ready: self.federation_ready,
        }
    }

    /// Mirror the outbox upserts into a lookup-by-id view. The accept
    /// path commits inbound records via the outbox extension method
    /// (`record_share_accept`) rather than `store.upsert`, so the store
    /// itself stays empty on the receiver side. Tests should call this
    /// to find the applied record by id.
    ///
    /// # Panics
    ///
    /// Panics if no upserted record matches `record_id`.
    pub async fn fetch_applied(&self, record_id: &str) -> MemoryRecord {
        tokio::task::yield_now().await;
        for r in self.outbox.upserts() {
            if r.id.as_str() == record_id {
                return r;
            }
        }
        panic!("Node::fetch_applied: no record {record_id} in outbox upserts");
    }

    #[must_use]
    pub fn has_share_link_provenance(&self, record: &MemoryRecord) -> bool {
        let _ = self;
        record.extra_frontmatter.contains_key("share_link_id")
            && record
                .extra_frontmatter
                .get("trust_status")
                .and_then(|v| v.as_str())
                == Some("inbound_shared")
    }

    /// Count records the outbox has applied that bear share-link
    /// provenance (= records minted by inbound `accept_share`).
    #[must_use]
    pub fn records_with_share_link_provenance(&self) -> usize {
        self.outbox
            .upserts()
            .iter()
            .filter(|r| self.has_share_link_provenance(r))
            .count()
    }

    /// Receiver-side fetch by record id. Returns the most recently
    /// upserted record under that id, or `None` if it was tombstoned
    /// or never applied.
    #[must_use]
    pub fn try_fetch(&self, record_id: &str) -> Option<MemoryRecord> {
        // Check if the record was tombstoned by the revoke path.
        if self.outbox.is_tombstoned(record_id) {
            return None;
        }
        self.outbox
            .upserts()
            .into_iter()
            .rfind(|r| r.id.as_str() == record_id)
    }

    /// Receiver-side: enumerate the consent-event kinds the outbox
    /// has committed for `link_id`. Matches against the
    /// `share_link:<lower(link_id)>` subject convention every
    /// federation kind uses, plus the inner ULID `op_id` on grant /
    /// accept events.
    ///
    /// Used by the revoke e2e test to assert "the receiver's journal
    /// records both an Accept and a Revoke under the same link" — a
    /// looser check than walking the timeline projection (which T12's
    /// adapter owns) but enough to keep the verb honest.
    #[must_use]
    pub fn consent_event_kinds_for_link(&self, link_id: &str) -> Vec<ConsentKind> {
        let lower_outer = format!("share_link:{}", link_id.to_ascii_lowercase());
        let inner_op = link_id.strip_prefix("share-").unwrap_or(link_id);
        let lower_inner = format!("share_link:{}", inner_op.to_ascii_lowercase());
        let proj = self
            .outbox
            .projection
            .lock()
            .expect("projection mutex poisoned");
        proj.consent_events
            .iter()
            .filter(|ev| {
                // Propose/accept events subject the outer `share-…`
                // link id; the receiver-side revoke event subjects the
                // inner ULID (T8 strips `share-` before building the
                // event). Match either so the caller can query by
                // whichever form they have.
                ev.subject == lower_outer
                    || ev.subject == lower_inner
                    || ev.op_id.as_deref() == Some(inner_op)
            })
            .map(|ev| ev.kind)
            .collect()
    }

    /// Teach the consent-lookup fake about the first apply so a second
    /// `accept_share` call sees the dedup hit. In production the
    /// adapter (T12) materialises this from the `consent_timeline`
    /// projection inside the same outbox transaction.
    pub fn record_accept_for_idempotency(
        &self,
        envelope: &cairn_core::domain::federation::FederationEnvelope,
        applied_records: Vec<String>,
    ) {
        let Some(link) = envelope.link.as_ref() else {
            return;
        };
        self.consent_lookup.record_accept(
            envelope.issuer_key_id.0.as_str(),
            link.link_id.as_str(),
            FederationAcceptRecord {
                link_id: link.link_id.clone(),
                applied_records,
            },
        );
    }
}

/// Build the issuer node (Alice). The `signing_key` is deterministic so
/// the receiver's cached `issuer_verifying_key` always matches.
#[must_use]
pub fn build_issuer() -> Node {
    let identity = Identity::parse("hmn:alice").expect("invariant: valid human identity");
    let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    let issuer_verifying_key = signing_key.verifying_key();

    let scope = ScopeTuple {
        tenant: Some("default".to_owned()),
        workspace: Some("vault-a".to_owned()),
        ..ScopeTuple::default()
    };
    let rebac = RebacContext::new(
        identity.clone(),
        vec![RebacRelation::new(
            identity.clone(),
            RebacAction::Write,
            scope.clone(),
            MemoryVisibility::Team,
        )],
    );
    let clock = FixedClock(
        "2026-05-21T12:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("invariant: hard-coded RFC-3339 literal must parse"),
    );

    let projection = Arc::new(Mutex::new(FederationProjection::default()));
    Node {
        store: InMemoryStore::default(),
        outbox: InMemoryOutbox::with_projection(Arc::clone(&projection)),
        consent_lookup: InMemoryConsentLookup::with_projection(projection),
        identity,
        signing_key,
        issuer_verifying_key,
        inbound_sensor: Identity::parse("snr:local:cli:test:v1").expect("sensor"),
        clock,
        rebac,
        scope,
        federation_ready: true,
    }
}

/// Build the receiver node (Bob). The `issuer_verifying_key` is derived
/// from the same deterministic seed as `build_issuer`'s signing key, so
/// envelopes signed by the issuer verify cleanly here.
#[must_use]
pub fn build_receiver() -> Node {
    let identity = Identity::parse("hmn:bob").expect("invariant: valid human identity");
    // Bob has his own signing key — only used to derive `consent_ref`s
    // for inbound records' provenance, never to sign envelopes.
    let signing_key = SigningKey::from_bytes(&[3_u8; 32]);
    // Issuer's verifying key — must mirror `build_issuer`'s seed.
    let issuer_signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    let issuer_verifying_key = issuer_signing_key.verifying_key();

    // Receiver's scope mirrors the issuer's (same tenant/workspace) so
    // the inbound envelope's `link.payload.scope` matches what the
    // receiver's ReBAC context authorises.
    let scope = ScopeTuple {
        tenant: Some("default".to_owned()),
        workspace: Some("vault-a".to_owned()),
        ..ScopeTuple::default()
    };
    let rebac = RebacContext::new(
        identity.clone(),
        vec![RebacRelation::new(
            identity.clone(),
            RebacAction::Write,
            scope.clone(),
            MemoryVisibility::Team,
        )],
    );
    let clock = FixedClock(
        "2026-05-21T12:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("invariant: hard-coded RFC-3339 literal must parse"),
    );

    let projection = Arc::new(Mutex::new(FederationProjection::default()));
    Node {
        store: InMemoryStore::default(),
        outbox: InMemoryOutbox::with_projection(Arc::clone(&projection)),
        consent_lookup: InMemoryConsentLookup::with_projection(projection),
        identity,
        signing_key,
        issuer_verifying_key,
        inbound_sensor: Identity::parse("snr:local:federation:inbound:v1").expect("sensor"),
        clock,
        rebac,
        scope,
        federation_ready: true,
    }
}

// ─── Envelope enrichment helper ───────────────────────────────────────

/// The `PropagationHandler` currently emits a propose envelope with
/// `manifest: None` — its docs flag this as a T14 follow-up. The e2e
/// tests still need to exercise the receiver's apply path, which
/// requires at least one manifest stub matching the link's
/// `target_id_hashes`. This helper builds a single stub for `record_id`
/// (whose `target_id` is the same string) so the enriched envelope
/// passes `validate_manifest` and the receiver applies the record.
#[must_use]
pub fn attach_manifest_stub(
    envelope: cairn_core::domain::federation::FederationEnvelope,
    record_id: &str,
    scope: &ScopeTuple,
    stub_visibility: MemoryVisibility,
    body: &str,
) -> cairn_core::domain::federation::FederationEnvelope {
    use cairn_core::generated::common::{
        MemoryRecordStub as WireStub, MemoryRecordStubVisibility as WireStubVis,
        ScopeTuple as WireScope, Ulid as WireUlid,
    };

    let wire_visibility = match stub_visibility {
        MemoryVisibility::Private => WireStubVis::Private,
        MemoryVisibility::Session => WireStubVis::Session,
        MemoryVisibility::Project => WireStubVis::Project,
        MemoryVisibility::Team => WireStubVis::Team,
        MemoryVisibility::Org => WireStubVis::Org,
        MemoryVisibility::Public => WireStubVis::Public,
        _ => panic!("invariant: unhandled MemoryVisibility variant"),
    };

    let stub = WireStub {
        body: Some(body.to_owned()),
        body_hash: format!("sha256:{}", "1".repeat(64)),
        kind: "user".to_owned(),
        record_id: WireUlid(record_id.to_owned()),
        scope: WireScope {
            agent: scope.agent.clone(),
            entity: scope.entity.clone(),
            project: scope.project.clone(),
            session_id: scope.session_id.clone(),
            tenant: scope.tenant.clone(),
            user: scope.user.clone(),
            workspace: scope.workspace.clone(),
        },
        tags: Some(Vec::new()),
        visibility: wire_visibility,
    };

    cairn_core::domain::federation::FederationEnvelope {
        manifest: Some(vec![stub]),
        ..envelope
    }
}

// Re-export `PeerEndpoint` so the e2e test file can use it without an
// extra `cairn_core::domain::federation::PeerEndpoint` import line.
pub use cairn_core::domain::federation::PeerEndpoint as PeerEndpointAlias;

/// Hand back the default loopback peer endpoint the handler is wired
/// against.
#[must_use]
pub fn loopback_peer() -> PeerEndpoint {
    PeerEndpoint("loopback-default".into())
}
