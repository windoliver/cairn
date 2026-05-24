//! Typed payloads for the two outbound federation propagation job kinds.
//!
//! `JobStore::JobPayload` bytes are canonical-JSON-encoded domain types —
//! the same encoding `canonical_bytes` in `cairn-core` produces. JSON is
//! used (not bincode) so replay logs stay human-readable, matching every
//! other workflow payload in this crate.
//!
//! ## Job kinds
//!
//! | Constant                | Kind string                                |
//! |-------------------------|--------------------------------------------|
//! | [`OUTBOUND_SHARE_KIND`] | `"federation.propagate.outbound_share"`    |
//! | [`OUTBOUND_REVOKE_KIND`] | `"federation.propagate.outbound_revoke"`  |
//!
//! ## Usage
//!
//! Enqueuers (T7 `propose_share`, T9 `revoke_share`) write canonical-JSON
//! bytes of the domain type directly into `JobPayload`. The handler (T13
//! `PropagationHandler`) reads those bytes via the parse helpers below.

use cairn_core::contract::job_store::JobPayload;
use cairn_core::domain::federation::SignedRevocation;
use cairn_core::domain::sharing::SignedShareLink;
use serde::{Deserialize, Serialize};

// ── Kind strings ──────────────────────────────────────────────────────────────

/// Job kind enqueued by `propose_share` (T7) for an outbound share link.
///
/// Mirrors [`cairn_core::verbs::propose_share::PROPAGATION_OUTBOUND_KIND`] —
/// both sides MUST use this same string so the scheduler routes the job to
/// the right handler (T13).
pub const OUTBOUND_SHARE_KIND: &str = "federation.propagate.outbound_share";

/// Job kind enqueued by `revoke_share` (T9) for an outbound revocation.
///
/// Mirrors [`cairn_core::verbs::revoke_share::REVOKE_OUTBOUND_KIND`] —
/// both sides MUST use this same string so the scheduler routes the job to
/// the right handler (T13).
pub const OUTBOUND_REVOKE_KIND: &str = "federation.propagate.outbound_revoke";

// ── Payload type aliases ───────────────────────────────────────────────────────

/// Outbound share job payload: the signed link *plus* the record bodies
/// that form the envelope manifest.
///
/// T7's `propose_share` verb serializes this with `serde_json::to_vec`.
/// The handler deserializes it back via [`parse_outbound_share`] and
/// uses the manifest entries to populate `FederationEnvelope.manifest`
/// so the receiver's `accept_share` can apply the records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboundSharePayload {
    /// The signed share link.
    pub link: SignedShareLink,
    /// Record bodies to include in the envelope manifest.
    pub manifest: Vec<ManifestEntry>,
    /// Optional per-job peer endpoint override.  When present the handler
    /// sends to this peer instead of `default_peer`.  Absent in payloads
    /// written before the peer-routing fix — the handler falls back to
    /// `default_peer` for backwards compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
}

/// One record destined for the envelope manifest.
///
/// Carries just enough data to build a [`MemoryRecordStub`] on the
/// handler side without reaching back into the store.
///
/// [`MemoryRecordStub`]: cairn_core::generated::common::MemoryRecordStub
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// ULID record id.
    pub record_id: String,
    /// Wire-form kind string (e.g. `"user"`).
    pub kind: String,
    /// Full record body text.
    pub body: String,
    /// `sha256:<64 lowercase hex>` hash of the body.
    pub body_hash: String,
    /// Wire-form visibility string (e.g. `"team"`).
    pub visibility: String,
    /// Scope tuple in canonical wire form.
    pub scope_wire: String,
    /// Record tags (may be empty).
    pub tags: Vec<String>,
}

/// Target deserialization type for `OUTBOUND_REVOKE_KIND` job payloads.
///
/// T9's `revoke_share` verb serializes a bare
/// [`cairn_core::domain::federation::SignedRevocation`] with
/// `canonical_bytes()`. The handler deserializes the same type back via
/// [`parse_outbound_revoke`].
pub type OutboundRevokePayload = SignedRevocation;

// ── Parse helpers ─────────────────────────────────────────────────────────────

/// Deserialize an `OUTBOUND_SHARE_KIND` job payload.
///
/// # Errors
///
/// Returns [`serde_json::Error`] when the bytes are not valid JSON or do
/// not match the [`OutboundSharePayload`] schema. A non-canonical byte
/// ordering is not an error at this layer — `serde_json` deserializes any
/// valid JSON encoding.
pub fn parse_outbound_share(bytes: &[u8]) -> Result<OutboundSharePayload, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// Deserialize an `OUTBOUND_REVOKE_KIND` job payload.
///
/// # Errors
///
/// Returns [`serde_json::Error`] when the bytes are not valid JSON or do
/// not match the [`OutboundRevokePayload`] schema.
pub fn parse_outbound_revoke(bytes: &[u8]) -> Result<OutboundRevokePayload, serde_json::Error> {
    serde_json::from_slice(bytes)
}

// ── Convenience serializers ───────────────────────────────────────────────────

/// Serialize an [`OutboundSharePayload`] to `JobPayload` bytes.
///
/// # Errors
///
/// Returns [`serde_json::Error`] on encoding failure (effectively unreachable
/// for this struct).
pub fn outbound_share_to_bytes(
    payload: &OutboundSharePayload,
) -> Result<JobPayload, serde_json::Error> {
    serde_json::to_vec(payload)
}

/// Serialize an [`OutboundRevokePayload`] to `JobPayload` bytes.
///
/// # Errors
///
/// Returns [`serde_json::Error`] on encoding failure (effectively unreachable
/// for this struct).
pub fn outbound_revoke_to_bytes(
    payload: &OutboundRevokePayload,
) -> Result<JobPayload, serde_json::Error> {
    serde_json::to_vec(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_strings_are_correct() {
        assert_eq!(OUTBOUND_SHARE_KIND, "federation.propagate.outbound_share");
        assert_eq!(OUTBOUND_REVOKE_KIND, "federation.propagate.outbound_revoke");
    }

    #[test]
    fn kind_strings_match_verb_constants() {
        assert_eq!(
            OUTBOUND_SHARE_KIND,
            cairn_core::verbs::propose_share::PROPAGATION_OUTBOUND_KIND,
        );
        assert_eq!(
            OUTBOUND_REVOKE_KIND,
            cairn_core::verbs::revoke_share::REVOKE_OUTBOUND_KIND,
        );
    }
}
