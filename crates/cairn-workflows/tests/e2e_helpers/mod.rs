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
use std::sync::Mutex;

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
use cairn_core::domain::time::FixedClock;
use cairn_core::domain::{
    ActorChainEntry, BodyHash, ChainRole, Ed25519Signature, EvidenceVector, Identity, MemoryClass,
    MemoryKind, MemoryRecord, MemoryVisibility, Provenance, RecordId, Rfc3339Timestamp, ScopeTuple,
    SourceId, TargetId,
};
use cairn_core::rebac::{RebacAction, RebacContext, RebacRelation};
use cairn_core::verbs::accept_share::AcceptShareDeps;
use cairn_core::verbs::propose_share::ProposeShareDeps;
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

    async fn tombstone(&self, _id: &RecordId, _reason: TombstoneReason) -> Result<(), StoreError> {
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

#[derive(Default)]
pub struct InMemoryOutbox {
    inner: Mutex<OutboxState>,
}

#[derive(Default)]
struct OutboxState {
    jobs: Vec<EnqueueRequest>,
    upserts: Vec<MemoryRecord>,
}

#[async_trait]
impl FederationOutbox for InMemoryOutbox {
    async fn record_share_grant(
        &self,
        _event: &cairn_core::domain::ConsentEvent,
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
        guard.jobs.push(job);
        Ok(())
    }

    async fn record_share_accept(
        &self,
        _event: &cairn_core::domain::ConsentEvent,
        upserts: &[MemoryRecord],
    ) -> Result<(), FederationOutboxError> {
        let mut guard = self.inner.lock().expect("outbox mutex poisoned");
        guard.upserts.extend(upserts.iter().cloned());
        Ok(())
    }

    async fn record_share_revoke(
        &self,
        _event: &cairn_core::domain::ConsentEvent,
        _tombstone_ids: &[String],
    ) -> Result<(), FederationOutboxError> {
        Ok(())
    }

    async fn record_share_revoke_grant(
        &self,
        _event: &cairn_core::domain::ConsentEvent,
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
}

// ─── In-memory ConsentLookup ──────────────────────────────────────────

#[derive(Default)]
pub struct InMemoryConsentLookup {
    accepts: Mutex<BTreeMap<String, FederationAcceptRecord>>,
    revoked: Mutex<std::collections::HashSet<String>>,
    share_links: Mutex<BTreeMap<String, StoredShareLink>>,
    revocations: Mutex<BTreeMap<String, StoredRevocation>>,
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
        let guard = self.revoked.lock().expect("consent-lookup mutex poisoned");
        Ok(guard.contains(link_id))
    }

    async fn find_share_link(
        &self,
        link_id: &str,
    ) -> Result<Option<StoredShareLink>, ConsentLookupError> {
        let guard = self
            .share_links
            .lock()
            .expect("consent-lookup mutex poisoned");
        Ok(guard.get(link_id).cloned())
    }

    async fn find_revocation(
        &self,
        link_id: &str,
    ) -> Result<Option<StoredRevocation>, ConsentLookupError> {
        let guard = self
            .revocations
            .lock()
            .expect("consent-lookup mutex poisoned");
        Ok(guard.get(link_id).cloned())
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

    /// Teach the consent-lookup fake about the first apply so a second
    /// `accept_share` call sees the dedup hit. In production the
    /// adapter (T12) materialises this from the consent_timeline
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

    Node {
        store: InMemoryStore::default(),
        outbox: InMemoryOutbox::default(),
        consent_lookup: InMemoryConsentLookup::default(),
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

    Node {
        store: InMemoryStore::default(),
        outbox: InMemoryOutbox::default(),
        consent_lookup: InMemoryConsentLookup::default(),
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
