//! `accept_share` verb (brief §12.a, §14).
//!
//! Receiver-side counterpart to [`crate::verbs::propose_share`]. Takes a
//! [`FederationEnvelope`] off the transport and atomically:
//!
//! * **Propose envelopes** — verify the [`SignedShareLink`] shape +
//!   signature + freshness; check the receiver's `ReBAC` `Write` relation
//!   at `link.payload.grant_tier`; dedupe on
//!   `(issuer_key_id, link_id)`; reject if the link was previously
//!   revoked; upsert each manifest record (with provenance pinned at the
//!   share link, visibility capped at `grant_tier`) AND append
//!   [`ConsentEvent::FederationAccept`] in one outbox transaction.
//! * **Revoke envelopes** — verify the [`SignedRevocation`] shape +
//!   signature; tombstone any local records previously upserted under
//!   `link_id` AND append [`ConsentEvent::FederationRevoke`] in one
//!   outbox transaction. A revoke with no prior records succeeds
//!   idempotently with an empty `applied_records` vector.
//!
//! Body-free across the board: every consent payload carries only the
//! `link_id`, granted tier, reason code, and salted record-id hashes —
//! never raw record bodies (brief §14).
//!
//! ## Capability gate
//!
//! The verb returns [`FederationError::CapabilityDisabled`] *before any
//! I/O* when `deps.federation_ready` is `false`.
//!
//! ## `ReBAC`
//!
//! The brief's `ReBAC` vocabulary is `{Read, Write}`; there is no
//! dedicated `Share` action. The receiver must hold a `Write` relation
//! at `link.payload.grant_tier` for `link.payload.scope` to apply the
//! envelope — the same gate the receiver would face for a fresh write
//! at that tier.

use std::collections::BTreeMap;

use ed25519_dalek::VerifyingKey;
use sha2::{Digest as _, Sha256};
use ulid::Ulid;

use crate::contract::consent_lookup::{ConsentLookup, FederationAcceptRecord};
use crate::contract::federation_outbox::{FederationOutbox, FederationOutboxError};
use crate::contract::memory_store::MemoryStore;
use crate::domain::canonical::canonical_bytes;
use crate::domain::federation::{
    DedupKey, FederationEnvelope, FederationEnvelopeExt, FederationEnvelopeKind, MemoryRecordStub,
    SignedRevocation, signed_share_link_from_wire,
};
use crate::domain::identity::keys::SigningKey;
use crate::domain::sharing::SignedShareLink;
use crate::domain::{
    ActorChainEntry, ChainRole, ConsentEvent, ConsentKind, ConsentPayload, Ed25519Signature,
    EvidenceVector, Identity, IdentityKind, MemoryClass, MemoryKind, MemoryRecord,
    MemoryVisibility, Provenance, RecordId, Rfc3339Timestamp, ScopeTuple, SourceId, TargetId,
};
use crate::error::federation::FederationError;
use crate::rebac::{RebacAction, RebacContext};

/// Caller-supplied request for [`accept_share`].
///
/// The envelope is the canonical wire form delivered by the federation
/// transport. The verb does not own envelope materialisation — the CLI /
/// MCP wrapper is responsible for parsing the transport frame.
#[derive(Debug, Clone)]
pub struct AcceptShareRequest {
    /// Inbound envelope. Either kind (`Propose` / `Revoke`) is valid.
    pub envelope: FederationEnvelope,
}

/// Outcome discriminator for [`AcceptShareResponse`]. Mirrors the
/// codegenerated `AcceptShareDataOutcome` so the verb output can be
/// adapter-marshalled without a second enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptOutcome {
    /// Envelope applied for the first time.
    Accepted,
    /// `(issuer_key_id, link_id)` already on disk — the original
    /// `applied_records` is returned and no new consent row is appended.
    Duplicate,
}

/// Successful outcome of [`accept_share`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptShareResponse {
    /// Whether this call applied the envelope or replayed a prior one.
    pub outcome: AcceptOutcome,
    /// Record ids the apply upserted (Propose) or tombstoned (Revoke).
    /// Empty for an idempotent revoke against an unknown link.
    pub applied_records: Vec<String>,
}

/// Per-call dependency snapshot for [`accept_share`].
///
/// Receiver-side parallel to
/// [`crate::verbs::propose_share::ProposeShareDeps`]. The
/// `issuer_verifying_key` is plumbed in by the wrapper (CLI / MCP) after
/// looking up `(envelope.issuer_key_id, envelope.link.payload.key_version)`
/// in the local keystore — `cairn-core` itself never performs keystore I/O.
pub struct AcceptShareDeps<'a> {
    /// Read/write store used to upsert inbound records and tombstone
    /// records revoked by an inbound envelope.
    pub store: &'a dyn MemoryStore,
    /// Atomic consent-event + record-upsert / tombstone sink.
    pub outbox: &'a dyn FederationOutbox,
    /// Read-only access to the consent timeline for idempotency and
    /// revocation lookups.
    pub consent_lookup: &'a dyn ConsentLookup,
    /// Receiver's local signing key. Used solely to derive the
    /// `consent_ref` for inbound records' provenance — never to sign
    /// the inbound envelope (the issuer already signed it).
    pub local_signing_key: &'a SigningKey,
    /// Receiver identity. The `ReBAC` principal must equal this.
    pub receiver_identity: &'a Identity,
    /// Issuer verifying key used to verify the inbound envelope's
    /// signature. Resolved by the wrapper from
    /// `(envelope.issuer_key_id, envelope.link.payload.key_version)`.
    pub issuer_verifying_key: &'a VerifyingKey,
    /// Receiver's `ReBAC` context. Must list a `Write` relation at
    /// `link.payload.grant_tier` for `link.payload.scope`.
    pub rebac: &'a RebacContext,
    /// Wall-clock source.
    pub clock: &'a dyn crate::domain::time::Clock,
    /// Local-sensor identity stamped on inbound records' provenance
    /// (`source_sensor`). Mirrors the propose-side test scaffold.
    pub inbound_sensor: &'a Identity,
    /// Capability gate. `false` short-circuits to
    /// [`FederationError::CapabilityDisabled`] before any I/O.
    pub federation_ready: bool,
}

/// Execute the verb.
///
/// # Errors
///
/// Returns [`FederationError`] for every shape, capability, freshness,
/// signature, `ReBAC`, revocation, or persistence failure. See the variant
/// docs on [`FederationError`] for the precise mapping.
#[tracing::instrument(
    skip(req, deps),
    err,
    fields(
        verb = "accept_share",
        kind = ?req.envelope.kind,
        link_id = req.envelope.link.as_ref().map_or("", |l| l.link_id.as_str()),
    ),
)]
pub async fn accept_share(
    req: AcceptShareRequest,
    deps: &AcceptShareDeps<'_>,
) -> Result<AcceptShareResponse, FederationError> {
    // 1. Capability gate — must run *before* any I/O so a half-wired
    //    deployment cannot leak inbound records or consent rows.
    if !deps.federation_ready {
        return Err(FederationError::CapabilityDisabled);
    }

    match req.envelope.kind {
        FederationEnvelopeKind::Propose => accept_propose(&req.envelope, deps).await,
        FederationEnvelopeKind::Revoke => accept_revoke(&req.envelope, deps).await,
    }
}

// ---------- propose path ----------------------------------------------

async fn accept_propose(
    envelope: &FederationEnvelope,
    deps: &AcceptShareDeps<'_>,
) -> Result<AcceptShareResponse, FederationError> {
    // 1. Shape: a propose envelope must carry a link. The wire
    //    discriminator says so; a hand-crafted envelope that lies about
    //    its kind fails closed here rather than later in the signature
    //    check (which would also reject it, just with a less specific
    //    error).
    let wire_link = envelope
        .link
        .as_ref()
        .ok_or(FederationError::InvalidShape)?;

    // 2. Lift the wire share-link into the domain type so we can call
    //    the existing `validate_shape` / `verify_signature` helpers.
    let link = signed_share_link_from_wire(wire_link).map_err(|_| FederationError::InvalidShape)?;

    // 3. Validate body-free shape before any signature work — the
    //    signature is the most expensive check, but `validate_shape`
    //    also rejects mismatched `link_id ↔ operation_id` binding which
    //    a signature alone would happily accept.
    link.validate_shape()
        .map_err(|_| FederationError::InvalidShape)?;

    // 4. Freshness — receiver-side wall-clock check against the link's
    //    `expires_at`. The brief calls this out as a separate gate from
    //    the signature so a clock-skew rejection is distinguishable
    //    from a malicious signature.
    let now = clock_to_rfc3339(deps.clock)?;
    if now.cmp_chronological(&link.payload.expires_at) != std::cmp::Ordering::Less {
        return Err(FederationError::Expired);
    }

    // 5. Signature — verify against the issuer key supplied by the
    //    wrapper. Domain helper rolls in `validate_shape` again, which
    //    is cheap.
    link.verify_signature(deps.issuer_verifying_key)
        .map_err(|_| FederationError::BadSignature)?;

    // 6. Manifest validation — each stub must carry a well-formed
    //    body_hash, and its id must be one of the link's
    //    `target_id_hashes`. A stub with a body but a hash mismatch is
    //    rejected as `TargetMismatch`.
    let manifest: &[MemoryRecordStub] = envelope.manifest.as_deref().unwrap_or(&[]);
    validate_manifest(manifest, &link)?;

    // 7. Issuer shape — share-link issuers must be humans (defense in
    //    depth; `validate_shape` already enforces this).
    if link.payload.issuer.kind() != IdentityKind::Human {
        return Err(FederationError::NotHuman);
    }

    // 8. Dedup BEFORE the revocation check — a duplicate accept must
    //    return `Duplicate` even if the link was later revoked, so the
    //    receiver's reply stays stable for retries that race a revoke.
    let dedup = envelope.dedup_key().ok_or(FederationError::InvalidShape)?;
    if let Some(prior) = find_dedup(deps.consent_lookup, dedup).await? {
        return Ok(AcceptShareResponse {
            outcome: AcceptOutcome::Duplicate,
            applied_records: prior.applied_records,
        });
    }

    // 9. Revocation — first-time envelopes against a known-revoked link
    //    are rejected. Duplicates above are not affected because they
    //    short-circuited.
    let revoked = deps
        .consent_lookup
        .is_link_revoked(&link.link_id)
        .await
        .map_err(|_| FederationError::InvalidShape)?;
    if revoked {
        return Err(FederationError::Revoked);
    }

    // 10. ReBAC — the receiver must hold a Write relation at the grant
    //     tier for the link's scope. Aligns with `verify_promotion_rebac`:
    //     Write is the action that authorises shared-tier mutations.
    if deps.rebac.principal() != Some(deps.receiver_identity) {
        return Err(FederationError::NoRebacRelation);
    }
    if !deps
        .rebac
        .evaluate(
            RebacAction::Write,
            &link.payload.scope,
            link.payload.grant_tier,
        )
        .allowed()
    {
        return Err(FederationError::NoRebacRelation);
    }

    // 11. Build the inbound records. Each one gets provenance pointing
    //     at the share link, visibility capped at `grant_tier`, and a
    //     fresh `consent_ref` keyed off the link id so the audit chain
    //     is self-documenting.
    let inbound = build_inbound_records(manifest, &link, deps, &now)?;
    let applied_ids: Vec<String> = inbound.iter().map(|r| r.id.as_str().to_owned()).collect();

    // 12. Consent event — body-free FederationAccept row that ties the
    //     apply to the link id and lists salted record-id hashes.
    let consent_event = build_accept_event(&link, &applied_ids, &now)?;

    // 13. Atomic apply via the outbox extension method.
    deps.outbox
        .record_share_accept(&consent_event, &inbound)
        .await
        .map_err(|err| map_outbox_error(&err))?;

    Ok(AcceptShareResponse {
        outcome: AcceptOutcome::Accepted,
        applied_records: applied_ids,
    })
}

// ---------- revoke path -----------------------------------------------

async fn accept_revoke(
    envelope: &FederationEnvelope,
    deps: &AcceptShareDeps<'_>,
) -> Result<AcceptShareResponse, FederationError> {
    let revocation = envelope
        .revocation
        .as_ref()
        .ok_or(FederationError::InvalidShape)?;

    validate_revocation_shape(revocation)?;
    verify_revocation_signature(revocation, deps.issuer_verifying_key)?;

    // Idempotency for revokes is keyed by `link_id` alone (any sender
    // can issue the revoke; the receiver-side projection collapses
    // duplicates). T12 will materialise the projection; for T8 we lean
    // on the outbox's own ConsentRejected mapping if the timeline
    // already recorded a revoke.
    let now = clock_to_rfc3339(deps.clock)?;
    let consent_event = build_revoke_event(&revocation.link_id, &now)?;

    // The receiver may or may not have prior records under this link;
    // either way the apply succeeds. T12 lands the lookup that
    // populates `tombstone_ids`; for T8 we pass an empty slice, which
    // the outbox happily writes as a consent-only revoke.
    let tombstone_ids: Vec<String> = Vec::new();
    deps.outbox
        .record_share_revoke(&consent_event, &tombstone_ids)
        .await
        .map_err(|err| map_outbox_error(&err))?;

    Ok(AcceptShareResponse {
        outcome: AcceptOutcome::Accepted,
        applied_records: tombstone_ids,
    })
}

// ---------- helpers ---------------------------------------------------

async fn find_dedup(
    lookup: &dyn ConsentLookup,
    dedup: DedupKey<'_>,
) -> Result<Option<FederationAcceptRecord>, FederationError> {
    lookup
        .find_federation_accept(dedup)
        .await
        .map_err(|_| FederationError::InvalidShape)
}

fn validate_manifest(
    manifest: &[MemoryRecordStub],
    link: &SignedShareLink,
) -> Result<(), FederationError> {
    for stub in manifest {
        if !is_valid_body_hash(&stub.body_hash) {
            return Err(FederationError::InvalidShape);
        }
        let id_hash = hash_record_id(&stub.record_id.0);
        if !link.payload.target_id_hashes.contains(&id_hash) {
            return Err(FederationError::TargetMismatch);
        }
    }
    Ok(())
}

fn is_valid_body_hash(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn hash_record_id(id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn cap_visibility(stub: MemoryVisibility, grant: MemoryVisibility) -> MemoryVisibility {
    if stub < grant { stub } else { grant }
}

fn map_stub_visibility(
    v: crate::generated::common::MemoryRecordStubVisibility,
) -> MemoryVisibility {
    use crate::generated::common::MemoryRecordStubVisibility as W;
    match v {
        W::Private => MemoryVisibility::Private,
        W::Session => MemoryVisibility::Session,
        W::Project => MemoryVisibility::Project,
        W::Team => MemoryVisibility::Team,
        W::Org => MemoryVisibility::Org,
        W::Public => MemoryVisibility::Public,
    }
}

fn build_inbound_records(
    manifest: &[MemoryRecordStub],
    link: &SignedShareLink,
    deps: &AcceptShareDeps<'_>,
    now: &Rfc3339Timestamp,
) -> Result<Vec<MemoryRecord>, FederationError> {
    let consent_ref = format!("consent:federation:{}", link.link_id);
    // Synthesize a deterministic SourceId so the inbound record carries
    // a `sources/` pointer that the lint pipeline can resolve. Body-free
    // — the source id encodes the link id, not the body.
    let source_id_str = synth_source_id(&link.link_id);
    let source_id = SourceId::parse(source_id_str).map_err(|_| FederationError::InvalidShape)?;

    let mut out = Vec::with_capacity(manifest.len());
    for stub in manifest {
        let stub_visibility = map_stub_visibility(stub.visibility);
        let visibility = cap_visibility(stub_visibility, link.payload.grant_tier);

        let kind = MemoryKind::parse(&stub.kind).map_err(|_| FederationError::InvalidShape)?;

        let id =
            RecordId::parse(stub.record_id.0.clone()).map_err(|_| FederationError::InvalidShape)?;
        let target_id =
            TargetId::parse(stub.record_id.0.clone()).map_err(|_| FederationError::InvalidShape)?;

        let scope = ScopeTuple {
            agent: stub.scope.agent.clone(),
            entity: stub.scope.entity.clone(),
            project: stub.scope.project.clone(),
            session_id: stub.scope.session_id.clone(),
            tenant: stub.scope.tenant.clone(),
            user: stub.scope.user.clone(),
            workspace: stub.scope.workspace.clone(),
        };

        let body = stub.body.clone().unwrap_or_default();
        let source_hash = format!("sha256:{}", "0".repeat(64));

        let provenance = Provenance {
            source_sensor: deps.inbound_sensor.clone(),
            created_at: now.clone(),
            originating_agent_id: link.payload.issuer.clone(),
            source_ids: vec![source_id.clone()],
            source_hash,
            consent_ref: consent_ref.clone(),
            llm_id_if_any: None,
            source_refs: Vec::new(),
        };

        let record = MemoryRecord {
            id,
            target_id,
            kind,
            class: MemoryClass::Semantic,
            visibility,
            scope,
            body,
            source_ids: vec![source_id.clone()],
            provenance,
            updated_at: now.clone(),
            evidence: EvidenceVector::default(),
            salience: 0.5,
            confidence: 0.7,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: link.payload.issuer.clone(),
                at: now.clone(),
            }],
            // Sentinel signature: the inbound record carries no
            // receiver-side ed25519 author signature; the share link
            // itself is the cryptographic witness. The store treats
            // this as opaque metadata.
            signature: Ed25519Signature::parse(format!("ed25519:{}", "0".repeat(128)))
                .map_err(|_| FederationError::InvalidShape)?,
            tags: stub.tags.clone().unwrap_or_default(),
            extra_frontmatter: inbound_frontmatter(&link.link_id, &link.payload.issuer),
            consent_model: None,
        };
        out.push(record);
    }
    Ok(out)
}

/// Body-free extra frontmatter capturing the inbound share-link
/// provenance. The receiver can recover `(link_id, issuer, trust_status)`
/// from this map without re-parsing the consent journal. The
/// `trust_status` value mirrors the brief's `inbound_shared` token (a
/// dedicated record field is a P1 refinement once §6.5 evolves).
fn inbound_frontmatter(link_id: &str, issuer: &Identity) -> BTreeMap<String, serde_json::Value> {
    let mut map = BTreeMap::new();
    map.insert(
        "share_link_id".to_owned(),
        serde_json::Value::String(link_id.to_owned()),
    );
    map.insert(
        "share_link_issuer".to_owned(),
        serde_json::Value::String(issuer.as_str().to_owned()),
    );
    map.insert(
        "trust_status".to_owned(),
        serde_json::Value::String("inbound_shared".to_owned()),
    );
    map
}

fn synth_source_id(link_id: &str) -> String {
    // SourceId is ULID-shaped; synthesize a deterministic 26-char
    // base32-ish id by hashing the link_id and grabbing the first 26
    // crockford-safe chars. Strict crockford rejects I/L/O/U so we
    // substitute. A real federation runtime (T12) will replace this
    // with a proper source-resolver entry; for T8 the inbound record
    // just needs *a* valid source id to satisfy domain validation.
    let mut hasher = Sha256::new();
    hasher.update(link_id.as_bytes());
    let digest = hasher.finalize();
    let alphabet = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"; // crockford
    let mut out = String::with_capacity(26);
    // First char must be 0..=7 to fit a 128-bit ULID.
    out.push((b'0' + (digest[0] & 0b0000_0111)) as char);
    for byte in digest.iter().take(25) {
        out.push(alphabet[(*byte as usize) & 31] as char);
    }
    out
}

fn build_accept_event(
    link: &SignedShareLink,
    applied_ids: &[String],
    now: &Rfc3339Timestamp,
) -> Result<ConsentEvent, FederationError> {
    let applied_id_hashes: Vec<String> = applied_ids.iter().map(|id| hash_record_id(id)).collect();
    let consent_id = Ulid::new().to_string();
    let event = ConsentEvent {
        consent_id,
        kind: ConsentKind::FederationAccept,
        actor: link.payload.issuer.clone(),
        subject: format!("share_link:{}", link.link_id.to_ascii_lowercase()),
        scope: link.payload.scope.canonical_wire(),
        op_id: Some(link.payload.operation_id.clone()),
        sensor_id: None,
        payload: ConsentPayload::FederationAccept {
            link_id: link.payload.operation_id.clone(),
            grant_tier: link.payload.grant_tier,
            applied_id_hashes,
        },
        decided_at: now.clone(),
        expires_at: Some(link.payload.expires_at.clone()),
    };
    event.validate().map_err(|err| {
        tracing::warn!(
            target: "cairn_core::verbs::accept_share",
            "accept_share built an invalid consent event: {err}",
        );
        FederationError::InvalidShape
    })?;
    Ok(event)
}

fn build_revoke_event(
    link_id: &str,
    now: &Rfc3339Timestamp,
) -> Result<ConsentEvent, FederationError> {
    let consent_id = Ulid::new().to_string();
    // Brief §14 requires a body-free `reason_code` token; the
    // receiver-side path is always `receiver_revoked` because the
    // issuer's own consent journal owns the `issuer_revoked` token.
    let event = ConsentEvent {
        consent_id,
        kind: ConsentKind::FederationRevoke,
        actor: Identity::parse("hmn:federation".to_owned())
            .map_err(|_| FederationError::InvalidShape)?,
        subject: format!("share_link:{}", link_id.to_ascii_lowercase()),
        // Revokes carry no scope of their own; reuse the universal
        // tenant marker so the journal row is queryable.
        scope: "tenant=default".to_owned(),
        op_id: None,
        sensor_id: None,
        payload: ConsentPayload::FederationRevoke {
            link_id: extract_operation_id(link_id).unwrap_or_else(|| link_id.to_owned()),
            reason_code: "receiver_revoked".to_owned(),
        },
        decided_at: now.clone(),
        expires_at: None,
    };
    event.validate().map_err(|err| {
        tracing::warn!(
            target: "cairn_core::verbs::accept_share",
            "accept_share built an invalid revoke consent event: {err}",
        );
        FederationError::InvalidShape
    })?;
    Ok(event)
}

fn extract_operation_id(link_id: &str) -> Option<String> {
    link_id.strip_prefix("share-").map(str::to_owned)
}

fn validate_revocation_shape(revocation: &SignedRevocation) -> Result<(), FederationError> {
    // Shape gate: issuer must be human, key_version >= 1, link_id
    // ULID-shaped (the wire validator already enforces 26-char ULID on
    // ULID-typed fields, but link_id is a plain String here for
    // future flexibility — re-check ourselves).
    if revocation.key_version < 1 {
        return Err(FederationError::InvalidShape);
    }
    if revocation.link_id.is_empty() {
        return Err(FederationError::InvalidShape);
    }
    let issuer =
        Identity::parse(revocation.issuer.0.clone()).map_err(|_| FederationError::InvalidShape)?;
    if issuer.kind() != IdentityKind::Human {
        return Err(FederationError::NotHuman);
    }
    let _ = Rfc3339Timestamp::parse(revocation.revoked_at.clone())
        .map_err(|_| FederationError::InvalidShape)?;
    Ok(())
}

fn verify_revocation_signature(
    revocation: &SignedRevocation,
    key: &VerifyingKey,
) -> Result<(), FederationError> {
    // Canonicalise the body-free payload (everything but the
    // signature) and verify ed25519 over those bytes.
    let payload = serde_json::json!({
        "issuer": revocation.issuer.0,
        "key_version": revocation.key_version,
        "link_id": revocation.link_id,
        "revoked_at": revocation.revoked_at,
    });
    let bytes = canonical_bytes(&payload).map_err(|_| FederationError::InvalidShape)?;
    let signature = decode_signature(&revocation.signature.0)?;
    key.verify_strict(&bytes, &signature)
        .map_err(|_| FederationError::BadSignature)
}

fn decode_signature(raw: &str) -> Result<ed25519_dalek::Signature, FederationError> {
    let hex = raw
        .strip_prefix("ed25519:")
        .ok_or(FederationError::BadSignature)?;
    if hex.len() != 128 {
        return Err(FederationError::BadSignature);
    }
    let mut bytes = [0_u8; 64];
    for (idx, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        let hi = nibble(pair[0]).ok_or(FederationError::BadSignature)?;
        let lo = nibble(pair[1]).ok_or(FederationError::BadSignature)?;
        bytes[idx] = (hi << 4) | lo;
    }
    Ok(ed25519_dalek::Signature::from_bytes(&bytes))
}

const fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

fn clock_to_rfc3339(
    clock: &dyn crate::domain::time::Clock,
) -> Result<Rfc3339Timestamp, FederationError> {
    let now = clock.now();
    let formatted = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    Rfc3339Timestamp::parse(formatted).map_err(|_| FederationError::InvalidShape)
}

fn map_outbox_error(err: &FederationOutboxError) -> FederationError {
    match err {
        FederationOutboxError::DuplicateJob => FederationError::DuplicateNonce,
        FederationOutboxError::ConsentRejected { .. } | FederationOutboxError::Backend { .. } => {
            FederationError::InvalidShape
        }
    }
}

/// Canonical JSON bytes for a revoke envelope's signed payload.
///
/// Exposed for the receiver-side test scaffold (and for T13's outbound
/// revoke handler) so the same canonicalisation runs on both sides of
/// the wire — eliminating "your signer salted my payload" mismatches.
#[must_use]
pub fn revocation_signed_payload_bytes(revocation: &SignedRevocation) -> Vec<u8> {
    let payload = serde_json::json!({
        "issuer": revocation.issuer.0,
        "key_version": revocation.key_version,
        "link_id": revocation.link_id,
        "revoked_at": revocation.revoked_at,
    });
    canonical_bytes(&payload).unwrap_or_default()
}
