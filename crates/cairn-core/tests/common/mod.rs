//! Shared test helpers for federation verb integration tests (T7..T9).
//!
//! `TestCtx` collects every dependency the federation verbs ask for —
//! an in-memory `MemoryStore`, an atomic [`FederationOutbox`], a
//! deterministic `Ed25519` signing key, a fixed `ReBAC` context, and a
//! `FixedClock`. T7's `federation_propose.rs` exercises the
//! happy-path + four denial branches; T8 / T9 extend the surface by
//! adding e.g. inbound-envelope helpers without reshaping the core
//! builder.
//!
//! ## Boundary note
//!
//! `cairn-core` may not depend on any other workspace crate, including
//! `cairn-test-fixtures` (enforced by `scripts/check-core-boundary.sh`).
//! Every helper below is therefore inlined here rather than reaching
//! for `FixtureStore` from the test-fixtures crate.

#![allow(dead_code)] // T8/T9 will exercise more of the surface than T7 does.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use cairn_core::contract::federation_outbox::{FederationOutbox, FederationOutboxError};
use cairn_core::contract::job_store::EnqueueRequest;
use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, Edge, EdgeDir, EdgeKey, KeywordSearchArgs, KeywordSearchPage, ListArgs,
    ListPage, MemoryStore, MemoryStoreCapabilities, RecordVersion, StoreError, TombstoneReason,
    UpsertOutcome,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::federation::PeerEndpoint;
use cairn_core::domain::identity::keys::SigningKey;
use cairn_core::domain::time::{Clock, FixedClock};
use cairn_core::domain::{
    ActorChainEntry, BodyHash, ChainRole, ConsentEvent, ConsentKind, Ed25519Signature,
    EvidenceVector, Identity, MemoryClass, MemoryKind, MemoryRecord, MemoryVisibility, Provenance,
    RecordId, Rfc3339Timestamp, ScopeTuple, SourceId, TargetId,
};
use cairn_core::rebac::{RebacAction, RebacContext, RebacRelation};
use cairn_core::verbs::propose_share::ProposeShareDeps;
use chrono::{DateTime, Utc};
use ulid::Ulid;

// ---------- minimal in-memory MemoryStore -----------------------------

/// Tiny `HashMap`-backed [`MemoryStore`] kept inside the test crate so
/// `cairn-core` does not have to depend on `cairn-test-fixtures`
/// (the `check-core-boundary.sh` invariant). Only the verb's actual
/// access pattern is implemented; every other trait method is a
/// stub.
#[derive(Default)]
pub struct InMemoryStore {
    inner: Mutex<HashMap<String, MemoryRecord>>,
}

#[async_trait]
impl MemoryStore for InMemoryStore {
    fn name(&self) -> &'static str {
        "in-memory-test"
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
        let mut guard = self.inner.lock().expect("test store mutex poisoned");
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
        let guard = self.inner.lock().expect("test store mutex poisoned");
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
        let guard = self.inner.lock().expect("test store mutex poisoned");
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
        Err("InMemoryStore: search_keyword unavailable".into())
    }
}

// ---------- atomic outbox fake ----------------------------------------

/// In-memory implementation of [`FederationOutbox`]. Both writes share
/// one mutex so partial commits are impossible — mirroring the `SQLite`
/// `with_tx` guarantee the production adapter will land in task T13 of
/// issue #123.
#[derive(Default)]
pub struct InMemoryOutbox {
    inner: Mutex<OutboxState>,
}

#[derive(Default)]
struct OutboxState {
    events: Vec<ConsentEvent>,
    jobs: Vec<EnqueueRequest>,
}

#[async_trait]
impl FederationOutbox for InMemoryOutbox {
    async fn record_share_grant(
        &self,
        event: &ConsentEvent,
        job: EnqueueRequest,
    ) -> Result<(), FederationOutboxError> {
        let mut guard = self
            .inner
            .lock()
            .expect("invariant: outbox mutex poisoned only on prior test panic");
        if let Some(dedupe_key) = job.dedupe_key.as_ref()
            && guard
                .jobs
                .iter()
                .any(|j| j.dedupe_key.as_deref() == Some(dedupe_key.as_str()))
        {
            return Err(FederationOutboxError::DuplicateJob);
        }
        guard.events.push(event.clone());
        guard.jobs.push(job);
        Ok(())
    }
}

impl InMemoryOutbox {
    pub fn jobs_of_kind(&self, kind: &str) -> Vec<EnqueueRequest> {
        let guard = self
            .inner
            .lock()
            .expect("invariant: outbox mutex poisoned only on prior test panic");
        guard
            .jobs
            .iter()
            .filter(|j| j.kind.as_str() == kind)
            .cloned()
            .collect()
    }

    pub fn has_event(&self, link_id: &str, kind: ConsentKind) -> bool {
        let guard = self
            .inner
            .lock()
            .expect("invariant: outbox mutex poisoned only on prior test panic");
        let expected_subject = format!("share_link:{}", link_id.to_ascii_lowercase());
        guard
            .events
            .iter()
            .any(|e| e.kind == kind && e.subject == expected_subject)
    }
}

// ---------- TestCtx ---------------------------------------------------

/// Stable test context shared across federation verb integration tests.
pub struct TestCtx {
    pub store: InMemoryStore,
    pub outbox: InMemoryOutbox,
    pub signing_key: SigningKey,
    pub signer: Identity,
    pub clock: FixedClock,
    pub rebac: RebacContext,
    pub federation_ready: bool,
    pub scope: ScopeTuple,
}

impl TestCtx {
    #[must_use]
    pub fn issuer_with_federation_ready() -> Self {
        Self::build(true, true)
    }

    #[must_use]
    pub fn issuer_with_federation_off() -> Self {
        Self::build(true, false)
    }

    #[must_use]
    pub fn issuer_without_share_relation() -> Self {
        Self::build(false, true)
    }

    fn build(with_relation: bool, federation_ready: bool) -> Self {
        let store = InMemoryStore::default();
        let outbox = InMemoryOutbox::default();
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let signer = Identity::parse("hmn:alice").expect("invariant: valid human identity");

        let scope = ScopeTuple {
            tenant: Some("default".to_owned()),
            workspace: Some("vault-a".to_owned()),
            ..ScopeTuple::default()
        };

        let relations = if with_relation {
            vec![RebacRelation::new(
                signer.clone(),
                RebacAction::Write,
                scope.clone(),
                MemoryVisibility::Team,
            )]
        } else {
            Vec::new()
        };
        let rebac = RebacContext::new(signer.clone(), relations);

        let clock = FixedClock(
            "2026-05-21T12:00:00Z"
                .parse::<DateTime<Utc>>()
                .expect("invariant: hard-coded RFC-3339 literal must parse"),
        );

        Self {
            store,
            outbox,
            signing_key,
            signer,
            clock,
            rebac,
            federation_ready,
            scope,
        }
    }

    /// Insert a body-bearing record at `Team` tier (the lowest shared
    /// tier) under the test scope. Returns the freshly-minted record id.
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
                originating_agent_id: self.signer.clone(),
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
                identity: self.signer.clone(),
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

    #[must_use]
    pub fn bob_identity(&self) -> Identity {
        // `&self` is unused today but T8/T9 may want a per-ctx grantee
        // (e.g. derived from the issuer's tenant). Suppress the lint
        // so the helper signature stays stable across T7→T9.
        let _ = self;
        Identity::parse("agt:cairn-cli:default:reader:v1").expect("invariant: valid agent")
    }

    #[must_use]
    pub fn scope(&self) -> ScopeTuple {
        self.scope.clone()
    }

    #[must_use]
    pub fn in_one_hour(&self) -> Rfc3339Timestamp {
        let _ = self;
        Rfc3339Timestamp::parse("2026-05-21T13:00:00Z").expect("invariant: literal must parse")
    }

    #[must_use]
    pub fn peer_endpoint(&self) -> PeerEndpoint {
        let _ = self;
        PeerEndpoint("loopback://node-b".into())
    }

    pub async fn pending_jobs(&self, kind: &str) -> Vec<EnqueueRequest> {
        // `async` here matches the plan's helper signature; T8/T9
        // implementations may actually await on a real adapter, but
        // for T7 the data is already in memory. Yield once so the
        // future is non-trivial and clippy doesn't flag `async fn`
        // with no awaits.
        tokio::task::yield_now().await;
        self.outbox.jobs_of_kind(kind)
    }

    pub async fn has_consent_event(&self, link_id: &str, kind: ConsentKind) -> bool {
        tokio::task::yield_now().await;
        self.outbox.has_event(link_id, kind)
    }

    pub fn clock(&self) -> &dyn Clock {
        &self.clock
    }

    pub fn deps(&self) -> ProposeShareDeps<'_> {
        ProposeShareDeps {
            store: &self.store,
            outbox: &self.outbox,
            signing_key: &self.signing_key,
            signer_identity: &self.signer,
            signer_key_version: 1,
            rebac: &self.rebac,
            clock: &self.clock,
            federation_ready: self.federation_ready,
        }
    }
}
