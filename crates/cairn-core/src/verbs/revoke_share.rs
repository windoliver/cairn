//! `revoke_share` verb (brief §12.a, §14).
//!
//! Issuer-side counterpart to [`crate::verbs::propose_share`]. Cancels a
//! previously minted [`SignedShareLink`] by:
//!
//! 1. Looking the link up in the consent timeline projection.
//! 2. Signing a body-free [`SignedRevocation`] carrying only
//!    `(issuer, key_version, link_id, revoked_at, signature)`.
//! 3. Atomically appending [`ConsentEvent::FederationRevoke`] *and*
//!    enqueueing an outbound revoke propagation job in one outbox
//!    transaction — mirroring the grant pair the propose verb writes.
//!
//! Body-free across the board: the consent payload carries only the
//! original `link_id` (`operation_id`) and a short `reason_code`. The
//! signed revocation likewise never references record bodies — the
//! receiver dedupes on `link_id` alone (brief §14).
//!
//! ## Capability gate
//!
//! The verb returns [`FederationError::CapabilityDisabled`] *before any
//! I/O* when `deps.federation_ready` is `false`.
//!
//! ## Idempotency
//!
//! Replaying a revoke against a link that already has a
//! [`ConsentKind::FederationRevoke`] row returns the original
//! `operation_id` and signed revocation byte-for-byte. The verb never
//! appends a second consent row or a second propagation job for the
//! same link.

use ulid::Ulid;

use crate::contract::consent_lookup::{ConsentLookup, StoredShareLink};
use crate::contract::federation_outbox::{FederationOutbox, FederationOutboxError};
use crate::contract::job_store::{EnqueueRequest, JobId, JobKind, RetryPolicy};
use crate::domain::canonical::canonical_bytes;
use crate::domain::federation::SignedRevocation;
use crate::domain::identity::keys::SigningKey;
use crate::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, IdentityKind, Rfc3339Timestamp,
};
use crate::error::federation::FederationError;
use crate::generated::common::{
    Ed25519Signature as WireEd25519Signature, Identity as WireIdentity,
};

/// Workflow kind used for outbound federation revoke propagation jobs.
///
/// Mirrors [`crate::verbs::propose_share::PROPAGATION_OUTBOUND_KIND`] —
/// the lone exported constant lets later tasks (T11 payload + T13
/// handler) register the same string the verb enqueues against.
pub const REVOKE_OUTBOUND_KIND: &str = "federation.propagate.outbound_revoke";

/// Caller-supplied request for [`revoke_share`].
///
/// Body-free: the verb takes only the stable share-link id (`share-…`)
/// produced by the original [`crate::verbs::propose_share`] call. Every
/// other field (issuer, `key_version`, scope, peer) is recovered from
/// the consent-timeline projection.
#[derive(Debug, Clone)]
pub struct RevokeShareRequest {
    /// Stable share-link id of the propose row that should be revoked.
    pub link_id: String,
}

/// Successful outcome of [`revoke_share`].
///
/// `Eq` is intentionally omitted because [`SignedRevocation`] is a
/// codegenerated wire type that only derives `PartialEq`; structural
/// equality over its `String`-typed signature slot is not part of the
/// contract.
#[derive(Debug, Clone, PartialEq)]
pub struct RevokeShareResponse {
    /// WAL-style operation id that ties the consent journal row, the
    /// propagation job, and the signed revocation together. A replay
    /// returns the same id the first call committed.
    pub operation_id: String,
    /// Body-free signed revocation. Carries issuer identity,
    /// `key_version`, `link_id`, `revoked_at`, and Ed25519 signature.
    pub revocation: SignedRevocation,
}

/// Per-call dependency snapshot for [`revoke_share`].
///
/// Issuer-side parallel to
/// [`crate::verbs::propose_share::ProposeShareDeps`]. The signing key,
/// identity, and `key_version` are plumbed in by the wrapper (CLI /
/// MCP) after resolving the issuer's keystore entry — `cairn-core`
/// itself never performs keystore I/O.
pub struct RevokeShareDeps<'a> {
    /// Atomic consent-event + propagation-job sink.
    pub outbox: &'a dyn FederationOutbox,
    /// Read-only access to the consent timeline. Used to look up the
    /// original propose row and to short-circuit on a prior revoke.
    pub consent_lookup: &'a dyn ConsentLookup,
    /// Loaded Ed25519 signing key for the issuer. Must match the key
    /// the propose row was signed with (the verb does not enforce this
    /// at runtime — the consent projection's `key_version` is the
    /// authoritative cross-check, surfaced as
    /// [`FederationError::InvalidShape`] on mismatch).
    pub signing_key: &'a SigningKey,
    /// Identity that owns `signing_key`. Must be a `hmn:` identity
    /// (`SignedRevocation` validation rejects non-human issuers).
    pub signer_identity: &'a Identity,
    /// Key version under which `signing_key` lives.
    pub signer_key_version: u32,
    /// Wall-clock source.
    pub clock: &'a dyn crate::domain::time::Clock,
    /// Capability gate. `false` short-circuits to
    /// [`FederationError::CapabilityDisabled`] before any I/O.
    pub federation_ready: bool,
}

/// Execute the verb.
///
/// # Errors
///
/// Returns [`FederationError`] for every shape, capability, or
/// persistence failure. See the variant docs on [`FederationError`] for
/// the precise mapping.
#[tracing::instrument(
    skip(req, deps),
    err,
    fields(
        verb = "revoke_share",
        link_id = req.link_id.as_str(),
    ),
)]
pub async fn revoke_share(
    req: RevokeShareRequest,
    deps: &RevokeShareDeps<'_>,
) -> Result<RevokeShareResponse, FederationError> {
    // 1. Capability gate — must run *before* any I/O so a half-wired
    //    deployment cannot leak federation state or audit rows.
    if !deps.federation_ready {
        return Err(FederationError::CapabilityDisabled);
    }

    // 2. Issuer shape — revocations must be signed by humans, mirroring
    //    the propose-side `NotHuman` gate and the wire
    //    `SignedRevocation` shape rules in `accept_share`.
    if deps.signer_identity.kind() != IdentityKind::Human {
        return Err(FederationError::NotHuman);
    }

    // 3. Idempotency — if the link was already revoked, replay the
    //    original `(operation_id, SignedRevocation)` byte-for-byte. The
    //    consent journal is the canonical source so retries never
    //    surface a fresh signature with the same op_id.
    if let Some(prior) = deps
        .consent_lookup
        .find_revocation(&req.link_id)
        .await
        .map_err(|_| FederationError::InvalidShape)?
    {
        return Ok(RevokeShareResponse {
            operation_id: prior.operation_id,
            revocation: prior.revocation,
        });
    }

    // 4. Look the original propose row up. A missing row maps to
    //    `UnknownLink` (distinct from `InvalidShape`) — the caller
    //    asked us to revoke something we never minted.
    let stored = deps
        .consent_lookup
        .find_share_link(&req.link_id)
        .await
        .map_err(|_| FederationError::InvalidShape)?
        .ok_or(FederationError::UnknownLink)?;

    // 5. Mint the operation id + RFC-3339 `revoked_at`. The id is the
    //    sole minting site so the WAL row, the propagation job, and any
    //    future replay all share one identifier.
    let operation_id = Ulid::new().to_string();
    let revoked_at = clock_to_rfc3339(deps.clock)?;

    // 6. Build + sign the body-free revocation. Canonicalisation
    //    mirrors `accept_share::revocation_signed_payload_bytes` so the
    //    receiver verifies the same bytes we sign.
    let revocation = build_signed_revocation(
        deps.signer_identity,
        deps.signer_key_version,
        &stored.payload.operation_id,
        &revoked_at,
        deps.signing_key,
    )?;

    // 7. Build the consent event + propagation job. Both share the
    //    same operation_id + dedupe_key so the journal and queue tables
    //    stay in lockstep, and a second call collapses to the same
    //    dedup hit.
    let consent_event = build_consent_event(&stored, &revocation, &operation_id, &revoked_at)?;
    let job = build_revoke_job(&req.link_id, &revocation, &stored, &operation_id)?;

    // 8. Atomic write. The outbox guarantees both rows land together —
    //    a half-written revoke would strand the audit trail without a
    //    propagation job, exactly the failure mode brief §5.6 forbids.
    deps.outbox
        .record_share_revoke_grant(&consent_event, job)
        .await
        .map_err(|err| map_outbox_error(&err))?;

    Ok(RevokeShareResponse {
        operation_id,
        revocation,
    })
}

// ---------- helpers ---------------------------------------------------

fn clock_to_rfc3339(
    clock: &dyn crate::domain::time::Clock,
) -> Result<Rfc3339Timestamp, FederationError> {
    let now = clock.now();
    let formatted = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    Rfc3339Timestamp::parse(formatted).map_err(|_| FederationError::InvalidShape)
}

fn build_signed_revocation(
    issuer: &Identity,
    key_version: u32,
    link_id: &str,
    revoked_at: &Rfc3339Timestamp,
    signing_key: &SigningKey,
) -> Result<SignedRevocation, FederationError> {
    // The wire `link_id` slot on `SignedRevocation` is the original
    // ULID-shaped operation id — the same value the receiver uses to
    // dedupe in `accept_share::accept_revoke`. The outer `share-…` id
    // never crosses the wire on a revocation.
    let payload = serde_json::json!({
        "issuer": issuer.as_str(),
        "key_version": i64::from(key_version),
        "link_id": link_id,
        "revoked_at": revoked_at.as_str(),
    });
    let bytes = canonical_bytes(&payload).map_err(|_| FederationError::InvalidShape)?;
    let signature = signing_key.sign(&bytes);
    let mut hex = String::with_capacity(128);
    for byte in signature.to_bytes() {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    let signature_string = format!("ed25519:{hex}");
    Ok(SignedRevocation {
        issuer: WireIdentity(issuer.as_str().to_owned()),
        key_version: i64::from(key_version),
        link_id: link_id.to_owned(),
        revoked_at: revoked_at.as_str().to_owned(),
        signature: WireEd25519Signature(signature_string),
    })
}

fn build_consent_event(
    stored: &StoredShareLink,
    revocation: &SignedRevocation,
    operation_id: &str,
    revoked_at: &Rfc3339Timestamp,
) -> Result<ConsentEvent, FederationError> {
    let consent_id = Ulid::new().to_string();
    let event = ConsentEvent {
        consent_id,
        kind: ConsentKind::FederationRevoke,
        actor: stored.payload.issuer.clone(),
        subject: format!("share_link:{}", stored.link_id.to_ascii_lowercase()),
        scope: stored.payload.scope.canonical_wire(),
        op_id: Some(operation_id.to_owned()),
        sensor_id: None,
        payload: ConsentPayload::FederationRevoke {
            // The consent journal's `link_id` field is the
            // ULID-shaped operation id (validated by
            // `validate_link_id`), matching the receiver-side
            // event emitted by `accept_share::accept_revoke`.
            link_id: revocation.link_id.clone(),
            reason_code: "issuer_revoked".to_owned(),
        },
        decided_at: revoked_at.clone(),
        expires_at: None,
    };
    event.validate().map_err(|err| {
        tracing::warn!(
            target: "cairn_core::verbs::revoke_share",
            "revoke_share built an invalid consent event: {err}",
        );
        FederationError::InvalidShape
    })?;
    Ok(event)
}

fn build_revoke_job(
    link_id: &str,
    revocation: &SignedRevocation,
    stored: &StoredShareLink,
    operation_id: &str,
) -> Result<EnqueueRequest, FederationError> {
    // Payload is the canonical JSON of the signed revocation — the
    // propagation handler (T13) deserialises and dispatches to the
    // transport. The original peer (when recorded on the propose row)
    // is carried via the queue_key so the scheduler can route to the
    // same destination the propose pair targeted.
    let payload = canonical_bytes(revocation).map_err(|_| FederationError::InvalidShape)?;
    let job_id = JobId::new(format!("job-{operation_id}"));
    let kind = JobKind::new(REVOKE_OUTBOUND_KIND);
    let queue_key = stored.peer.as_ref().map_or_else(
        || format!("federation:{link_id}"),
        |peer| format!("federation:{peer}:{link_id}"),
    );
    Ok(EnqueueRequest {
        job_id,
        kind,
        payload,
        queue_key: Some(queue_key),
        // Dedupe on the original link's operation_id so two parallel
        // revoke attempts against the same link collapse into one
        // queued job. The verb's own `find_revocation` short-circuit
        // already handles the common replay path; this is a belt-and-
        // braces line against a race between the lookup and the
        // write.
        dedupe_key: Some(format!("revoke:{}", stored.payload.operation_id)),
        // Eligible immediately — the scheduler's clock authority owns
        // the actual epoch, matching propose_share.
        not_before_ms: 0,
        retry: RetryPolicy::DEFAULT,
    })
}

fn map_outbox_error(err: &FederationOutboxError) -> FederationError {
    match err {
        // A duplicate dedupe key means a parallel call won the race
        // between our `find_revocation` lookup and the outbox commit.
        // We surface this as `DuplicateNonce` so the caller treats it
        // the same as the propose-side replay path; the next retry
        // will hit the `find_revocation` short-circuit and reply with
        // the original signed revocation.
        FederationOutboxError::DuplicateJob => FederationError::DuplicateNonce,
        FederationOutboxError::ConsentRejected { .. } | FederationOutboxError::Backend { .. } => {
            FederationError::InvalidShape
        }
    }
}
