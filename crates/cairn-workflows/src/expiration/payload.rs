//! `ExpirationPayload` — what an enqueued expiration sweep carries.
//!
//! JSON-encoded so replay logs stay human-readable, matching the
//! `consolidation` payload's contract.

use cairn_core::contract::job_store::JobPayload;
use cairn_core::domain::ScopeTuple;
use serde::{Deserialize, Serialize};

/// Resume cursor folded into a continuation payload. Mirrors the
/// fields of `cairn_core::contract::memory_store::ListCursor`
/// (`updated_at` epoch-seconds + tie-breaker `record_id`) so the
/// handler can rebuild a `ListCursor` without depending on Serde
/// support on that public type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpirationCursor {
    /// `updated_at` epoch-seconds boundary of the previous page's
    /// tail.
    pub updated_at: i64,
    /// Tie-breaker record id, parseable by
    /// `cairn_core::domain::RecordId::parse`.
    pub record_id: String,
}

/// One enqueued expiration-sweep request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpirationPayload {
    /// Wall-clock `now_ms` to compare every record's `updated_at`
    /// against. The enqueuer captures this once so a job that retries
    /// later still produces a deterministic result against the same
    /// snapshot (round-3-compatible with brief §15 release gating).
    pub now_ms: i64,
    /// Optional bound scope. Mirrors `ConsolidationPayload.bound_scope`
    /// — when present the handler filters `list` reads so a job
    /// dispatched on behalf of one tenant cannot tombstone another's
    /// records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_scope: Option<ScopeTuple>,
    /// Optional resume cursor — set on continuation jobs the
    /// handler enqueues when the inspect-cap fired before the
    /// store-side cursor exhausted. Without this, expiration in a
    /// fresh-prefix vault could starve old records permanently
    /// (round-6 adversarial review #1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ExpirationCursor>,
}

impl ExpirationPayload {
    /// Serialize to `JobPayload`.
    ///
    /// # Errors
    /// JSON encoding failure (effectively unreachable for this struct).
    pub fn to_bytes(&self) -> Result<JobPayload, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Deserialize from `JobPayload`.
    ///
    /// # Errors
    /// JSON decoding failure.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let p = ExpirationPayload {
            now_ms: 123_456_789,
            bound_scope: None,
            cursor: None,
        };
        let bytes = p.to_bytes().expect("encode");
        let back = ExpirationPayload::from_bytes(&bytes).expect("decode");
        assert_eq!(p, back);
    }

    #[test]
    fn roundtrip_with_cursor() {
        let p = ExpirationPayload {
            now_ms: 1,
            bound_scope: None,
            cursor: Some(ExpirationCursor {
                updated_at: 1_700_000_000,
                record_id: "01HQZX9F5N0000000000000001".into(),
            }),
        };
        let bytes = p.to_bytes().expect("encode");
        let back = ExpirationPayload::from_bytes(&bytes).expect("decode");
        assert_eq!(p, back);
    }

    #[test]
    fn unknown_fields_rejected() {
        let bytes = br#"{"now_ms":0,"y":2}"#;
        assert!(ExpirationPayload::from_bytes(bytes).is_err());
    }
}
