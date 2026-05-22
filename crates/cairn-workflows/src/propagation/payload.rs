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

/// Target deserialization type for `OUTBOUND_SHARE_KIND` job payloads.
///
/// T7's `propose_share` verb serializes a bare
/// [`cairn_core::domain::sharing::SignedShareLink`] with
/// `canonical_bytes()` (serde-json canonical encoding). The handler
/// deserializes the same type back via [`parse_outbound_share`].
pub type OutboundSharePayload = SignedShareLink;

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
/// Enqueuers should prefer using `cairn_core::domain::canonical::canonical_bytes`
/// directly (matching T7 exactly), but this helper is provided for
/// completeness and symmetric testing.
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
