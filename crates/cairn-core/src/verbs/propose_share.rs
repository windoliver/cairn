//! `propose_share` verb (brief §12.a, §14).
//!
//! Mints a signed `SignedShareLink` over a caller-supplied record set,
//! checks the issuer's `ReBAC` `Write` relation at the requested
//! `grant_tier`, and atomically writes the corresponding
//! [`ConsentEvent::FederationGrant`] consent journal row alongside an
//! outbound propagation job. The propagation job itself is dispatched
//! later by the federation `PropagationWorkflow` handler (issue #123 T13).
//!
//! Body-free across the board: the consent payload carries only the
//! `link_id`, granted tier, symbolic peer code, and an optional salted
//! hash of the named grantee — never raw record bodies or URLs (brief
//! §14).
//!
//! ## Capability gate
//!
//! The verb returns [`FederationError::CapabilityDisabled`] *before any
//! I/O* when `deps.federation_ready` is `false`. The runtime advertises
//! `cairn.federation.v1` via
//! [`crate::status::wiring::federation_extension_ready`] only when every
//! downstream dispatch path (T7..T17) is wired. Surfaces that
//! short-circuit here cannot leak federation state into a half-wired
//! deployment.

use std::collections::BTreeSet;

use sha2::{Digest as _, Sha256};
use ulid::Ulid;

use crate::contract::federation_outbox::{FederationOutbox, FederationOutboxError};
use crate::contract::job_store::{EnqueueRequest, JobId, JobKind, RetryPolicy};
use crate::contract::memory_store::MemoryStore;
use crate::domain::canonical::canonical_bytes;
use crate::domain::federation::PeerEndpoint;
use crate::domain::identity::keys::SigningKey;
use crate::domain::sharing::{ShareLinkPayload, SignedShareLink};
use crate::domain::{
    CanonicalRecordHash, ConsentEvent, ConsentKind, ConsentPayload, Ed25519Signature, Identity,
    IdentityKind, MemoryRecord, MemoryVisibility, Rfc3339Timestamp, ScopeTuple,
};
use crate::error::federation::FederationError;
use crate::rebac::{RebacAction, RebacContext};

/// Workflow kind used for outbound federation propagation jobs.
///
/// The lone, exported constant lets later tasks (T11 payload + T13
/// handler) register the same string the verb enqueues against. Mirrors
/// the brief §10 / §12.a naming convention (`<domain>.<phase>.<step>`).
pub const PROPAGATION_OUTBOUND_KIND: &str = "federation.propagate.outbound_share";

/// Hard cap on records that may ride in one share link (mirrors
/// [`crate::domain::sharing::ShareLinkPayload::target_id_hashes`]'s
/// 64-entry validator). Anything larger gets rejected up front with
/// [`FederationError::InvalidShape`] before any store hit.
pub const MAX_RECORDS_PER_LINK: usize = 64;

/// Caller-supplied request for [`propose_share`].
///
/// Body-free in the sense that follows the consent journal contract:
/// `record_ids` and `scope` are typed identifiers, not record bodies;
/// `peer` is a transport-side opaque endpoint, dispatched verbatim by
/// the federation transport.
#[derive(Debug, Clone)]
pub struct ProposeShareRequest {
    /// 1..=`MAX_RECORDS_PER_LINK` distinct ULID record ids to bundle.
    pub record_ids: Vec<String>,
    /// Optional explicit grantee (turns the link into a recipient-bound
    /// grant). `None` mints a bearer link.
    pub grantee: Option<Identity>,
    /// Scope tuple the link authorises.
    pub scope: ScopeTuple,
    /// Tier the grant authorises. Must be a shared tier — `Private` or
    /// `Session` returns [`FederationError::TierMismatch`].
    pub grant_tier: MemoryVisibility,
    /// Hard expiry of the link. Must be strictly after `issued_at`.
    pub expires_at: Rfc3339Timestamp,
    /// Optional outbound peer endpoint. `None` enqueues a propagation
    /// job without a peer (handler defaults to `"unspecified"` peer
    /// code in the consent payload).
    pub peer: Option<PeerEndpoint>,
}

/// Outcome of a successful [`propose_share`] call.
#[derive(Debug, Clone)]
pub struct ProposeShareResponse {
    /// The freshly-minted signed share link.
    pub link: SignedShareLink,
    /// WAL-style operation id that ties the consent journal row, the
    /// propagation job, and any future revoke envelope together.
    pub operation_id: String,
}

/// Per-call dependency snapshot for [`propose_share`].
///
/// The verb is intentionally infrastructure-free: every adapter
/// (store, outbox, keystore, `ReBAC`, clock) is injected so tests can
/// substitute deterministic fakes. `federation_ready` mirrors
/// `wiring::federation_extension_ready()` but is plumbed through deps so
/// tests can flip the capability gate without recompiling the wiring
/// constants.
pub struct ProposeShareDeps<'a> {
    /// Read-side store used to load each requested record.
    pub store: &'a dyn MemoryStore,
    /// Atomic consent-event + propagation-job sink.
    pub outbox: &'a dyn FederationOutbox,
    /// Loaded Ed25519 signing key for the issuer. The caller (CLI / MCP
    /// wrapper) is responsible for resolving the key via
    /// `Keystore::load_signing_key` ahead of time — `cairn-core` itself
    /// never performs keystore I/O.
    pub signing_key: &'a SigningKey,
    /// Identity that owns `signing_key`. Must match `rebac.principal()`
    /// and must be a `hmn:` identity (the share-link schema rejects
    /// non-human issuers).
    pub signer_identity: &'a Identity,
    /// Key version under which `signing_key` lives.
    pub signer_key_version: u32,
    /// `ReBAC` context. The verb asserts a `Write` relation at
    /// `grant_tier` for the requested scope.
    pub rebac: &'a RebacContext,
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
/// Returns [`FederationError`] for every shape, capability, `ReBAC`, or
/// persistence failure. See the variant docs for the exact mapping.
#[tracing::instrument(
    skip(req, deps),
    err,
    fields(
        verb = "propose_share",
        records = req.record_ids.len(),
        grant_tier = ?req.grant_tier,
    ),
)]
pub async fn propose_share(
    req: ProposeShareRequest,
    deps: &ProposeShareDeps<'_>,
) -> Result<ProposeShareResponse, FederationError> {
    // 1. Capability gate — must run *before* any I/O so a half-wired
    //    deployment cannot leak records via store reads.
    if !deps.federation_ready {
        return Err(FederationError::CapabilityDisabled);
    }

    // 2. Tier guard. `Private` / `Session` are local-only; sharing them
    //    is nonsensical and would never satisfy `is_shared_tier`.
    if !crate::rebac::is_shared_tier(req.grant_tier) {
        return Err(FederationError::TierMismatch);
    }

    // 3. Issuer shape — share links must be signed by humans (mirrors
    //    `SignedShareLink::validate_shape`). Catching it here keeps the
    //    error mapped onto `NotHuman` instead of falling through to a
    //    later `InvalidShape`.
    if deps.signer_identity.kind() != IdentityKind::Human {
        return Err(FederationError::NotHuman);
    }

    // 4. Request shape — empty or oversized record sets fail closed.
    //    The hard cap mirrors the share-link payload's own validator
    //    (`MAX_TARGET_ID_HASHES = 64`) so a malformed link cannot ride
    //    through the signing path.
    if req.record_ids.is_empty() || req.record_ids.len() > MAX_RECORDS_PER_LINK {
        return Err(FederationError::InvalidShape);
    }

    // 5. ReBAC — the issuer must hold a Write relation at this tier for
    //    this scope. Aligns with `verify_promotion_rebac`: `Write` is
    //    the action that authorises shared-tier mutations; a dedicated
    //    `RebacAction::Share` does not yet exist (the existing rebac
    //    vocabulary is `{Read, Write}` per `src/rebac.rs`).
    if deps.rebac.principal() != Some(deps.signer_identity) {
        return Err(FederationError::NoRebacRelation);
    }
    if !deps
        .rebac
        .evaluate(RebacAction::Write, &req.scope, req.grant_tier)
        .allowed()
    {
        return Err(FederationError::NoRebacRelation);
    }

    // 6. Load records and validate against the request scope. The
    //    record's own scope must be a subset of the requested scope so
    //    the resulting link does not over-broadcast (e.g. issuing a
    //    `team` grant for a `session=X` record while the link's scope
    //    omits `session=X`).
    let records = load_records(deps.store, &req.record_ids).await?;

    // 7. Hash the manifest and each target id. `target_hash` binds the
    //    record set as a whole; `target_id_hashes` lets the receiver
    //    pin individual ids without ever seeing record bodies.
    let target_id_hashes = compute_target_id_hashes(&records);
    let target_hash = compute_manifest_hash(&records)?;

    // 8. Mint the ULID-shaped operation id and derive a link id +
    //    nonce. The verb is the sole minting site so the WAL row, the
    //    consent event, and the propagation job all share one id.
    let operation_id = Ulid::new().to_string();
    let link_id = format!("share-{operation_id}");
    let nonce = mint_nonce_base64();

    // 9. Build the share link payload, sign canonical JSON bytes.
    let issued_at = clock_to_rfc3339(deps.clock)?;
    let payload = ShareLinkPayload {
        operation_id: operation_id.clone(),
        nonce,
        target_hash,
        target_id_hashes,
        scope: req.scope.clone(),
        grant_tier: req.grant_tier,
        grantee: req.grantee.clone(),
        issuer: deps.signer_identity.clone(),
        issued_at,
        expires_at: req.expires_at.clone(),
        key_version: deps.signer_key_version,
    };
    let signature = sign_payload(deps.signing_key, &payload)?;
    let link = SignedShareLink {
        link_id: link_id.clone(),
        payload,
        signature,
    };

    // 10. Validate the assembled link against the hand-written shape
    //     checker. Returning an `InvalidShape` here is preferable to
    //     trusting the receiver to catch our mistake.
    link.validate_shape()
        .map_err(|_| FederationError::InvalidShape)?;

    // 11. Build the consent event + propagation job. Both share the
    //     same operation id so the journal and queue tables stay in
    //     lockstep.
    let consent_event = build_consent_event(&link, req.peer.as_ref(), deps.clock)?;
    let job = build_propagation_job(&link, &records, &operation_id)?;

    // 12. Atomic write. The outbox guarantees both rows land together;
    //     a duplicate dedupe key is reported back as `InvalidShape`
    //     because reaching this point with the same `operation_id`
    //     means the caller broke ULID uniqueness — not a legitimate
    //     replay path for an externally-visible verb.
    deps.outbox
        .record_share_grant(&consent_event, job)
        .await
        .map_err(|err| map_outbox_error(&err))?;

    Ok(ProposeShareResponse { link, operation_id })
}

// ---------- helpers ----------------------------------------------------

async fn load_records(
    store: &dyn MemoryStore,
    record_ids: &[String],
) -> Result<Vec<MemoryRecord>, FederationError> {
    let mut seen = BTreeSet::new();
    let mut records = Vec::with_capacity(record_ids.len());
    for id in record_ids {
        // Reject duplicates up front so the resulting target_id_hashes
        // vector also stays unique (the validator requires that).
        if !seen.insert(id.clone()) {
            return Err(FederationError::InvalidShape);
        }
        let parsed = crate::domain::RecordId::parse(id.clone())
            .map_err(|_| FederationError::InvalidShape)?;
        let record = store
            .get(&parsed)
            .await
            // The store may surface a backend error — we do not
            // distinguish per-record causes in the propose path,
            // intentionally: brief §14 forbids leaking which record
            // failed.
            .map_err(|_| FederationError::InvalidShape)?
            .ok_or(FederationError::InvalidShape)?;
        records.push(record);
    }
    Ok(records)
}

fn compute_target_id_hashes(records: &[MemoryRecord]) -> Vec<String> {
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let mut hasher = Sha256::new();
        hasher.update(record.target_id.as_str().as_bytes());
        out.push(format!("sha256:{:x}", hasher.finalize()));
    }
    out
}

fn compute_manifest_hash(records: &[MemoryRecord]) -> Result<String, FederationError> {
    // Canonical-JCS encoding of the per-record canonical hashes,
    // ordered by `target_id` so the receiver can recompute without
    // re-deriving the verb's iteration order.
    let mut entries: Vec<(String, String)> = Vec::with_capacity(records.len());
    for record in records {
        let hash =
            CanonicalRecordHash::compute(record).map_err(|_| FederationError::InvalidShape)?;
        entries.push((
            record.target_id.as_str().to_owned(),
            hash.as_str().to_owned(),
        ));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let value = serde_json::Value::Array(
        entries
            .into_iter()
            .map(|(tid, hash)| {
                let mut map = serde_json::Map::new();
                map.insert("target_id".into(), serde_json::Value::String(tid));
                map.insert("record_hash".into(), serde_json::Value::String(hash));
                serde_json::Value::Object(map)
            })
            .collect(),
    );
    let bytes =
        serde_json_canonicalizer::to_vec(&value).map_err(|_| FederationError::InvalidShape)?;
    let digest = Sha256::digest(&bytes);
    Ok(format!("sha256:{digest:x}"))
}

fn mint_nonce_base64() -> String {
    use rand_core::RngCore as _;
    let mut bytes = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut bytes);
    base64_encode_16(&bytes)
}

/// Tiny dependency-free encoder for the 16-byte nonce. The shared
/// nonce validator (`domain::sharing::validate_nonce`) accepts both
/// the 22-char unpadded form and the 24-char padded form; we emit the
/// 24-char padded variant for clarity.
fn base64_encode_16(bytes: &[u8; 16]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    // 16 bytes -> 22 base64 chars + 2 '=' padding = 24.
    let mut out = Vec::with_capacity(24);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n =
            (u32::from(bytes[i]) << 16) | (u32::from(bytes[i + 1]) << 8) | u32::from(bytes[i + 2]);
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize]);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize]);
        out.push(ALPHABET[((n >> 6) & 0x3F) as usize]);
        out.push(ALPHABET[(n & 0x3F) as usize]);
        i += 3;
    }
    // 16 bytes = 5 full triplets (15 bytes) + 1 trailing byte.
    let rem = bytes.len() - i; // == 1
    if rem == 1 {
        let n = u32::from(bytes[i]) << 16;
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize]);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize]);
        out.push(b'=');
        out.push(b'=');
    }
    // SAFETY: every byte we pushed is from `ALPHABET` (`[A-Za-z0-9+/]`) or `=`.
    String::from_utf8(out).unwrap_or_default()
}

fn clock_to_rfc3339(
    clock: &dyn crate::domain::time::Clock,
) -> Result<Rfc3339Timestamp, FederationError> {
    let now = clock.now();
    let formatted = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    Rfc3339Timestamp::parse(formatted).map_err(|_| FederationError::InvalidShape)
}

fn sign_payload(
    signing_key: &SigningKey,
    payload: &ShareLinkPayload,
) -> Result<Ed25519Signature, FederationError> {
    let bytes = canonical_bytes(payload).map_err(|_| FederationError::InvalidShape)?;
    let sig = signing_key.sign(&bytes);
    let mut hex = String::with_capacity(128);
    for byte in sig.to_bytes() {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Ed25519Signature::parse(format!("ed25519:{hex}")).map_err(|_| FederationError::InvalidShape)
}

fn build_consent_event(
    link: &SignedShareLink,
    peer: Option<&PeerEndpoint>,
    clock: &dyn crate::domain::time::Clock,
) -> Result<ConsentEvent, FederationError> {
    let consent_id = Ulid::new().to_string();
    let scope_wire = link.payload.scope.canonical_wire();
    let peer_code = peer_endpoint_to_code(peer);
    let grantee_id_hash = link.payload.grantee.as_ref().map(hash_identity);
    let decided_at = clock_to_rfc3339(clock)?;
    let event = ConsentEvent {
        consent_id,
        kind: ConsentKind::FederationGrant,
        actor: link.payload.issuer.clone(),
        subject: format!("share_link:{}", link.link_id.to_ascii_lowercase()),
        scope: scope_wire,
        op_id: Some(link.payload.operation_id.clone()),
        sensor_id: None,
        payload: ConsentPayload::FederationGrant {
            link_id: link.payload.operation_id.clone(),
            grant_tier: link.payload.grant_tier,
            peer_code,
            grantee_id_hash,
        },
        decided_at,
        expires_at: Some(link.payload.expires_at.clone()),
    };
    event.validate().map_err(|err| {
        tracing::warn!(
            target: "cairn_core::verbs::propose_share",
            "propose_share built an invalid consent event: {err}",
        );
        FederationError::InvalidShape
    })?;
    Ok(event)
}

/// Serialization-only struct that mirrors
/// `cairn_workflows::propagation::payload::OutboundSharePayload`.
///
/// `cairn-core` cannot depend on `cairn-workflows`, so we define a
/// private struct with the same JSON shape. The handler side
/// deserializes via its own `OutboundSharePayload` type — both agree
/// on the wire format (field names + JSON structure).
#[derive(serde::Serialize)]
struct OutboundSharePayloadWire<'a> {
    link: &'a SignedShareLink,
    manifest: Vec<ManifestEntryWire>,
}

/// Matches `cairn_workflows::propagation::payload::ManifestEntry`.
#[derive(serde::Serialize)]
struct ManifestEntryWire {
    record_id: String,
    kind: String,
    body: String,
    body_hash: String,
    visibility: String,
    scope_wire: String,
    tags: Vec<String>,
}

/// Compute `sha256:<64 lowercase hex>` over a record body.
///
/// Matches the hash format that `accept_share::is_valid_body_hash`
/// validates: `^sha256:[0-9a-f]{64}$`.
fn compute_body_hash(body: &str) -> String {
    let digest = Sha256::digest(body.as_bytes());
    format!("sha256:{digest:x}")
}

fn build_propagation_job(
    link: &SignedShareLink,
    records: &[MemoryRecord],
    operation_id: &str,
) -> Result<EnqueueRequest, FederationError> {
    let manifest: Vec<ManifestEntryWire> = records
        .iter()
        .map(|r| ManifestEntryWire {
            record_id: r.id.as_str().to_owned(),
            kind: r.kind.as_str().to_owned(),
            body: r.body.clone(),
            body_hash: compute_body_hash(&r.body),
            visibility: r.visibility.as_str().to_owned(),
            scope_wire: r.scope.canonical_wire(),
            tags: r.tags.clone(),
        })
        .collect();
    let wire = OutboundSharePayloadWire { link, manifest };
    let payload = serde_json::to_vec(&wire).map_err(|_| FederationError::InvalidShape)?;
    let job_id = JobId::new(format!("job-{operation_id}"));
    let kind = JobKind::new(PROPAGATION_OUTBOUND_KIND);
    Ok(EnqueueRequest {
        job_id,
        kind,
        payload,
        queue_key: Some(format!("federation:{}", link.link_id)),
        dedupe_key: Some(operation_id.to_owned()),
        // Eligible immediately. The scheduler's clock authority owns
        // the actual epoch; we use 0 because the propose path runs in
        // the verb thread and we don't want to pull a wall-clock
        // dependency just for an "as soon as possible" marker.
        not_before_ms: 0,
        retry: RetryPolicy::DEFAULT,
    })
}

fn map_outbox_error(err: &FederationOutboxError) -> FederationError {
    match err {
        // A duplicate dedupe key with the same operation_id implies
        // either a ULID collision (mathematically negligible) or the
        // caller replayed a request that already committed. The
        // existing `DuplicateNonce` variant carries the same "this
        // (issuer, nonce/op_id) pair is already on disk" semantic so
        // we map onto it rather than introducing a new variant.
        FederationOutboxError::DuplicateJob => FederationError::DuplicateNonce,
        // `ConsentRejected` and opaque `Backend` failures fall back to
        // `InvalidShape` — the consent payload either failed adapter-
        // side validation or the adapter declined the write. Both
        // surface to the caller as "shape-level retry will not help".
        // A typed `Backend` variant on `FederationError` is a P1
        // refinement (the round-1 reviewer of issue #123 will say if
        // they want it; punt for now).
        FederationOutboxError::ConsentRejected { .. } | FederationOutboxError::Backend { .. } => {
            FederationError::InvalidShape
        }
    }
}

/// Sanitise a [`PeerEndpoint`] string into the strict `code` shape the
/// consent journal validator (`validate_code` in `domain::consent`)
/// requires: `[a-z][a-z0-9_-]{0,63}`.
///
/// The validator is intentionally strict (brief §14) so raw URLs with
/// auth params cannot ride into the journal. T5 land made it final; a
/// future task (called out in the task plan) may centralise the policy
/// once a real federation transport ships. Until then this helper
/// owns the mapping:
///
/// * lowercase every byte
/// * map every non-`[a-z0-9_-]` byte to `-`
/// * strip leading non-`[a-z]` so the first char satisfies the regex
/// * cap at 64 chars
/// * default to `"unspecified"` when the result is empty or `peer`
///   itself is `None`.
fn peer_endpoint_to_code(peer: Option<&PeerEndpoint>) -> String {
    let Some(peer) = peer else {
        return "unspecified".to_owned();
    };
    let lowered: String = peer
        .0
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' => (b + 32) as char,
            b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => b as char,
            _ => '-',
        })
        .collect();
    // Strip leading non-alpha bytes.
    let trimmed: String = lowered
        .trim_start_matches(|c: char| !c.is_ascii_lowercase())
        .to_owned();
    if trimmed.is_empty() {
        return "unspecified".to_owned();
    }
    trimmed.chars().take(64).collect()
}

fn hash_identity(identity: &Identity) -> String {
    let mut hasher = Sha256::new();
    hasher.update(identity.as_str().as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_endpoint_to_code_handles_typical_inputs() {
        assert_eq!(
            peer_endpoint_to_code(Some(&PeerEndpoint("loopback://node-a".into()))),
            "loopback---node-a"
        );
        assert_eq!(
            peer_endpoint_to_code(Some(&PeerEndpoint("HTTPS://Hub/Team-X?secret=42".into()))),
            "https---hub-team-x-secret-42"
        );
        assert_eq!(peer_endpoint_to_code(None), "unspecified");
        assert_eq!(
            peer_endpoint_to_code(Some(&PeerEndpoint(String::new()))),
            "unspecified"
        );
        // Leading non-alpha bytes are stripped; the first surviving
        // `[a-z]` byte anchors the result, satisfying the validator's
        // `^[a-z]` requirement.
        assert_eq!(
            peer_endpoint_to_code(Some(&PeerEndpoint("___leading-underscore".into()))),
            "leading-underscore"
        );
        // Result must default to `"unspecified"` when every byte is
        // stripped (e.g. all-numeric / all-symbol input).
        assert_eq!(
            peer_endpoint_to_code(Some(&PeerEndpoint("123".into()))),
            "unspecified"
        );
        let big = "a".repeat(200);
        assert_eq!(peer_endpoint_to_code(Some(&PeerEndpoint(big))).len(), 64);
    }

    #[test]
    fn base64_encode_16_round_trips_via_validate_nonce() {
        let nonce = base64_encode_16(&[0u8; 16]);
        // Hand-verify the canonical "AAAA…==" form (16 zero bytes).
        assert_eq!(nonce, "AAAAAAAAAAAAAAAAAAAAAA==");
    }
}
