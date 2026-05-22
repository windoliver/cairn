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
use cairn_core::contract::consent_lookup::{
    ConsentLookup, ConsentLookupError, FederationAcceptRecord,
};
use cairn_core::contract::federation_outbox::{FederationOutbox, FederationOutboxError};
use cairn_core::contract::job_store::EnqueueRequest;
use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, Edge, EdgeDir, EdgeKey, KeywordSearchArgs, KeywordSearchPage, ListArgs,
    ListPage, MemoryStore, MemoryStoreCapabilities, RecordVersion, StoreError, TombstoneReason,
    UpsertOutcome,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::consent_timeline::ConsentTimelineEvent;
use cairn_core::domain::federation::{
    DedupKey, FederationEnvelope, FederationEnvelopeKind, PeerEndpoint,
};
use cairn_core::domain::identity::keys::SigningKey;
use cairn_core::domain::sharing::ShareLinkPayload;
use cairn_core::domain::time::{Clock, FixedClock};
use cairn_core::domain::{
    ActorChainEntry, BodyHash, ChainRole, ConsentEvent, ConsentKind, Ed25519Signature,
    EvidenceVector, Identity, MemoryClass, MemoryKind, MemoryRecord, MemoryVisibility, Provenance,
    RecordId, Rfc3339Timestamp, ScopeTuple, SourceId, TargetId,
};
use cairn_core::rebac::{RebacAction, RebacContext, RebacRelation};
use cairn_core::verbs::accept_share::AcceptShareDeps;
use cairn_core::verbs::propose_share::ProposeShareDeps;
use chrono::{DateTime, Utc};
use sha2::{Digest as _, Sha256};
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
    upserts: Vec<MemoryRecord>,
    tombstones: Vec<String>,
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

    async fn record_share_accept(
        &self,
        event: &ConsentEvent,
        upserts: &[MemoryRecord],
    ) -> Result<(), FederationOutboxError> {
        let mut guard = self
            .inner
            .lock()
            .expect("invariant: outbox mutex poisoned only on prior test panic");
        guard.events.push(event.clone());
        guard.upserts.extend(upserts.iter().cloned());
        Ok(())
    }

    async fn record_share_revoke(
        &self,
        event: &ConsentEvent,
        tombstone_ids: &[String],
    ) -> Result<(), FederationOutboxError> {
        let mut guard = self
            .inner
            .lock()
            .expect("invariant: outbox mutex poisoned only on prior test panic");
        guard.events.push(event.clone());
        guard.tombstones.extend(tombstone_ids.iter().cloned());
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

    pub fn count_events(&self, link_id: &str, kind: ConsentKind) -> usize {
        let guard = self
            .inner
            .lock()
            .expect("invariant: outbox mutex poisoned only on prior test panic");
        let expected_subject = format!("share_link:{}", link_id.to_ascii_lowercase());
        guard
            .events
            .iter()
            .filter(|e| e.kind == kind && e.subject == expected_subject)
            .count()
    }

    pub fn upserts(&self) -> Vec<MemoryRecord> {
        let guard = self
            .inner
            .lock()
            .expect("invariant: outbox mutex poisoned only on prior test panic");
        guard.upserts.clone()
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

    /// Build a [`ProposeShareDeps`] snapshot from the current ctx — used
    /// by the `propose_share` (issuer) tests.
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

// ---------- ReceiverCtx (T8: accept_share) ----------------------------

/// In-memory [`ConsentLookup`] fake. The accept-share verb uses two of
/// its methods: `find_federation_accept` (idempotency) and
/// `is_link_revoked` (revocation). The fake keeps both as `Mutex<...>`
/// so tests can prime them mid-flight (e.g. mark a link revoked between
/// two `accept_share` calls).
#[derive(Default)]
pub struct InMemoryConsentLookup {
    accepts: Mutex<BTreeMap<String, FederationAcceptRecord>>,
    revoked: Mutex<std::collections::HashSet<String>>,
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
        let guard = self
            .accepts
            .lock()
            .expect("invariant: consent-lookup mutex poisoned only on prior test panic");
        Ok(guard.get(&dedup_string(&dedup)).cloned())
    }

    async fn is_link_revoked(&self, link_id: &str) -> Result<bool, ConsentLookupError> {
        let guard = self
            .revoked
            .lock()
            .expect("invariant: consent-lookup mutex poisoned only on prior test panic");
        Ok(guard.contains(link_id))
    }
}

impl InMemoryConsentLookup {
    pub fn record_accept(
        &self,
        issuer_key_id: &str,
        link_id: &str,
        nonce: &str,
        record: FederationAcceptRecord,
    ) {
        let mut guard = self
            .accepts
            .lock()
            .expect("invariant: consent-lookup mutex poisoned only on prior test panic");
        guard.insert(dedup_string_owned(issuer_key_id, link_id, nonce), record);
    }

    pub fn mark_revoked(&self, link_id: &str) {
        let mut guard = self
            .revoked
            .lock()
            .expect("invariant: consent-lookup mutex poisoned only on prior test panic");
        guard.insert(link_id.to_owned());
    }
}

fn dedup_string(d: &DedupKey<'_>) -> String {
    dedup_string_owned(d.issuer_key_id, d.link_id, d.nonce)
}

fn dedup_string_owned(issuer_key_id: &str, link_id: &str, nonce: &str) -> String {
    format!("{issuer_key_id}|{link_id}|{nonce}")
}

/// Receiver-side test context for [`accept_share`].
///
/// `issuer_signing_key` is the **sender's** key (Alice). The receiver
/// (Bob) holds only the verifying key. The verb does not call the
/// issuer signing key — it lives here so the test can mint envelopes
/// signed by the issuer on the fly.
pub struct ReceiverCtx {
    pub store: InMemoryStore,
    pub outbox: InMemoryOutbox,
    pub consent_lookup: InMemoryConsentLookup,
    pub receiver_identity: Identity,
    pub receiver_signing_key: SigningKey,
    pub inbound_sensor: Identity,
    pub issuer_identity: Identity,
    pub issuer_signing_key: SigningKey,
    pub issuer_verifying_key_cached: ed25519_dalek::VerifyingKey,
    pub clock: FixedClock,
    pub rebac: RebacContext,
    pub federation_ready: bool,
    pub scope: ScopeTuple,
}

impl ReceiverCtx {
    #[must_use]
    pub fn receiver_with_federation_ready() -> Self {
        Self::build(true, true)
    }

    #[must_use]
    pub fn receiver_without_write_relation() -> Self {
        Self::build(false, true)
    }

    fn build(with_relation: bool, federation_ready: bool) -> Self {
        let store = InMemoryStore::default();
        let outbox = InMemoryOutbox::default();
        let consent_lookup = InMemoryConsentLookup::default();

        let receiver_identity =
            Identity::parse("hmn:bob").expect("invariant: valid human identity");
        let receiver_signing_key = SigningKey::from_bytes(&[3_u8; 32]);

        let issuer_identity =
            Identity::parse("hmn:alice").expect("invariant: valid human identity");
        let issuer_signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let issuer_verifying_key_cached = issuer_signing_key.verifying_key();

        let inbound_sensor =
            Identity::parse("snr:local:federation:inbound:v1").expect("invariant: valid sensor");

        let scope = ScopeTuple {
            tenant: Some("default".to_owned()),
            workspace: Some("vault-b".to_owned()),
            ..ScopeTuple::default()
        };

        let relations = if with_relation {
            vec![RebacRelation::new(
                receiver_identity.clone(),
                RebacAction::Write,
                scope.clone(),
                MemoryVisibility::Team,
            )]
        } else {
            Vec::new()
        };
        let rebac = RebacContext::new(receiver_identity.clone(), relations);

        let clock = FixedClock(
            "2026-05-21T12:00:00Z"
                .parse::<DateTime<Utc>>()
                .expect("invariant: hard-coded RFC-3339 literal must parse"),
        );

        Self {
            store,
            outbox,
            consent_lookup,
            receiver_identity,
            receiver_signing_key,
            inbound_sensor,
            issuer_identity,
            issuer_signing_key,
            issuer_verifying_key_cached,
            clock,
            rebac,
            federation_ready,
            scope,
        }
    }

    #[must_use]
    pub fn deps(&self) -> AcceptShareDeps<'_> {
        AcceptShareDeps {
            store: &self.store,
            outbox: &self.outbox,
            consent_lookup: &self.consent_lookup,
            local_signing_key: &self.receiver_signing_key,
            receiver_identity: &self.receiver_identity,
            issuer_verifying_key: &self.issuer_verifying_key_cached,
            rebac: &self.rebac,
            clock: &self.clock,
            inbound_sensor: &self.inbound_sensor,
            federation_ready: self.federation_ready,
        }
    }

    #[must_use]
    pub fn issuer_verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.issuer_signing_key.verifying_key()
    }

    /// Build a signed propose envelope carrying a single record stub
    /// at `stub_visibility`. The link is signed with the issuer's
    /// signing key; the envelope is ready to feed into `accept_share`.
    pub async fn build_signed_propose_envelope_with_one_record(
        &self,
        stub_visibility: MemoryVisibility,
    ) -> FederationEnvelope {
        tokio::task::yield_now().await;
        self.build_signed_propose_envelope_inner(stub_visibility, false, false)
    }

    /// Build a propose envelope whose link is expired (issued in the
    /// past, expires before `clock.now()`).
    pub async fn build_expired_propose_envelope(&self) -> FederationEnvelope {
        tokio::task::yield_now().await;
        self.build_signed_propose_envelope_inner(MemoryVisibility::Team, true, false)
    }

    /// Build a propose envelope whose payload was tampered with after
    /// signing — the signature will not verify.
    pub async fn build_tampered_propose_envelope(&self) -> FederationEnvelope {
        tokio::task::yield_now().await;
        self.build_signed_propose_envelope_inner(MemoryVisibility::Team, false, true)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Test scaffold: builds the full wire-form envelope inline so the failure mode is local to one function."
    )]
    fn build_signed_propose_envelope_inner(
        &self,
        stub_visibility: MemoryVisibility,
        expired: bool,
        tamper: bool,
    ) -> FederationEnvelope {
        use cairn_core::domain::canonical::canonical_bytes;
        use cairn_core::generated::common::{
            Ed25519Signature as WireEd25519Signature, Identity as WireIdentity,
            MemoryRecordStub as WireStub, MemoryRecordStubVisibility as WireStubVis, Nonce16Base64,
            ScopeTuple as WireScope, ShareLinkPayload as WirePayload,
            ShareLinkPayloadGrantTier as WireGrant, SignedShareLink as WireLink, Ulid as WireUlid,
        };

        // Operation id + link id + nonce. Use a fixed nonce per call so
        // dedup tests can correlate envelopes across two calls.
        let operation_id = format!("01HQZX9F5N0{}", "0".repeat(15));
        let link_id = format!("share-{operation_id}");
        let nonce_raw = "AAAAAAAAAAAAAAAAAAAAAA==".to_owned();

        let record_id_str = format!("01HQZX9F5N1{}", "0".repeat(15));

        // Hash the record id the same way the verb expects.
        let mut hasher = Sha256::new();
        hasher.update(record_id_str.as_bytes());
        let target_id_hash = format!("sha256:{:x}", hasher.finalize());

        let (issued_at, expires_at) = if expired {
            (
                "2026-05-21T10:00:00Z".to_owned(),
                "2026-05-21T11:00:00Z".to_owned(),
            )
        } else {
            (
                "2026-05-21T11:00:00Z".to_owned(),
                "2026-05-21T13:00:00Z".to_owned(),
            )
        };

        // Build the **domain** payload, sign it canonically, then mirror
        // it into the wire form. This way the bytes we sign match the
        // bytes the receiver verifies, regardless of struct field order.
        let domain_scope = self.scope.clone();
        let domain_payload = ShareLinkPayload {
            operation_id: operation_id.clone(),
            nonce: nonce_raw.clone(),
            target_hash: format!("sha256:{}", "0".repeat(64)),
            target_id_hashes: vec![target_id_hash.clone()],
            scope: domain_scope.clone(),
            grant_tier: MemoryVisibility::Team,
            grantee: None,
            issuer: self.issuer_identity.clone(),
            issued_at: Rfc3339Timestamp::parse(issued_at.clone())
                .expect("invariant: literal must parse"),
            expires_at: Rfc3339Timestamp::parse(expires_at.clone())
                .expect("invariant: literal must parse"),
            key_version: 1,
        };
        let bytes = canonical_bytes(&domain_payload).expect("canonical bytes");
        let signature = self.issuer_signing_key.sign(&bytes);
        let mut hex = String::with_capacity(128);
        for byte in signature.to_bytes() {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        let mut signature_string = format!("ed25519:{hex}");
        if tamper {
            // Flip one nibble of the signature so verify_signature fails
            // while the shape stays valid. The signature is ASCII hex,
            // so going through Vec<u8> stays UTF-8-safe without an
            // `unsafe` block (the workspace forbids `unsafe_code`).
            let mut bytes = signature_string.into_bytes();
            let last = bytes.len() - 1;
            bytes[last] = if bytes[last] == b'0' { b'1' } else { b'0' };
            signature_string =
                String::from_utf8(bytes).expect("invariant: ed25519 sig hex stays ASCII");
        }

        let wire_payload = WirePayload {
            expires_at,
            grant_tier: WireGrant::Team,
            grantee: None,
            issued_at,
            issuer: WireIdentity(self.issuer_identity.as_str().to_owned()),
            key_version: 1,
            nonce: Nonce16Base64(nonce_raw),
            operation_id: WireUlid(operation_id.clone()),
            scope: WireScope {
                agent: domain_scope.agent.clone(),
                entity: domain_scope.entity.clone(),
                project: domain_scope.project.clone(),
                session_id: domain_scope.session_id.clone(),
                tenant: domain_scope.tenant.clone(),
                user: domain_scope.user.clone(),
                workspace: domain_scope.workspace.clone(),
            },
            target_hash: format!("sha256:{}", "0".repeat(64)),
            target_id_hashes: vec![target_id_hash],
        };
        let wire_link = WireLink {
            link_id: link_id.clone(),
            payload: wire_payload,
            signature: WireEd25519Signature(signature_string),
        };

        let stub_wire_visibility = match stub_visibility {
            MemoryVisibility::Private => WireStubVis::Private,
            MemoryVisibility::Session => WireStubVis::Session,
            MemoryVisibility::Project => WireStubVis::Project,
            MemoryVisibility::Team => WireStubVis::Team,
            MemoryVisibility::Org => WireStubVis::Org,
            MemoryVisibility::Public => WireStubVis::Public,
            // `MemoryVisibility` is `#[non_exhaustive]`; new variants
            // should not silently leak into share envelopes.
            _ => panic!("invariant: unhandled MemoryVisibility variant in test envelope builder"),
        };
        let stub = WireStub {
            body: Some("inbound body".to_owned()),
            body_hash: format!("sha256:{}", "1".repeat(64)),
            kind: "user".to_owned(),
            record_id: WireUlid(record_id_str.clone()),
            scope: WireScope {
                agent: domain_scope.agent.clone(),
                entity: domain_scope.entity.clone(),
                project: domain_scope.project.clone(),
                session_id: domain_scope.session_id.clone(),
                tenant: domain_scope.tenant.clone(),
                user: domain_scope.user.clone(),
                workspace: domain_scope.workspace.clone(),
            },
            tags: Some(Vec::new()),
            visibility: stub_wire_visibility,
        };

        FederationEnvelope {
            issuer_key_id: WireIdentity(self.issuer_identity.as_str().to_owned()),
            kind: FederationEnvelopeKind::Propose,
            link: Some(wire_link),
            manifest: Some(vec![stub]),
            revocation: None,
        }
    }

    pub async fn mark_link_revoked_for_test(&self, link_id: &str) {
        tokio::task::yield_now().await;
        self.consent_lookup.mark_revoked(link_id);
    }

    pub async fn fetch(&self, record_id: &str) -> MemoryRecord {
        tokio::task::yield_now().await;
        // The outbox does the upserts in the accept path; mirror them
        // into the store so `fetch` returns the canonical record.
        for r in self.outbox.upserts() {
            if r.id.as_str() == record_id {
                return r;
            }
        }
        panic!("ReceiverCtx::fetch: no record {record_id} in outbox upserts");
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

    pub async fn has_consent_event_for_envelope(
        &self,
        envelope: &FederationEnvelope,
        kind: ConsentKind,
    ) -> bool {
        tokio::task::yield_now().await;
        let Some(link) = envelope.link.as_ref() else {
            return false;
        };
        self.outbox.has_event(&link.link_id, kind)
    }

    pub async fn count_consent_events_for_envelope(
        &self,
        envelope: &FederationEnvelope,
        kind: ConsentKind,
    ) -> usize {
        tokio::task::yield_now().await;
        let Some(link) = envelope.link.as_ref() else {
            return 0;
        };
        self.outbox.count_events(&link.link_id, kind)
    }
}

// Bridge: when the receiver-side test applies an envelope it can also
// teach the consent lookup that the link/nonce pair is now on disk,
// closing the loop for idempotency. Used by the `accept_is_idempotent`
// test below.
impl ReceiverCtx {
    pub fn record_accept_for_idempotency(
        &self,
        envelope: &FederationEnvelope,
        applied_records: Vec<String>,
    ) {
        let Some(link) = envelope.link.as_ref() else {
            return;
        };
        self.consent_lookup.record_accept(
            envelope.issuer_key_id.0.as_str(),
            link.link_id.as_str(),
            link.payload.nonce.0.as_str(),
            FederationAcceptRecord {
                link_id: link.link_id.clone(),
                applied_records,
            },
        );
    }
}
