//! Serde-encoded payload carried in `workflow_jobs.payload`.
//! `Bincode` would be smaller but JSON gives us auditability for free
//! and keeps replay logs human-readable.

use cairn_core::contract::job_store::JobPayload;
use cairn_core::domain::ScopeTuple;
use serde::{Deserialize, Serialize};

/// One enqueued rolling-summary request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationPayload {
    /// Session whose turns are being summarized.
    pub session_id: String,
    /// Watermark — the highest sequence already covered by a prior
    /// summary for this session. `0` for the first run.
    pub since_sequence: u32,
    /// Bound scope (tenant / workspace / user / agent) the enqueuing
    /// verb verified the issuer against. The handler MUST filter
    /// `list_trace_turns` and the source-liveness re-check by this
    /// scope so a job dispatched on behalf of tenant A cannot read or
    /// summarize tenant B's `turn_summary` records (round-4 adversarial
    /// review #1). `None` is single-tenant P0 — no tenant binding to
    /// enforce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_scope: Option<ScopeTuple>,
}

impl ConsolidationPayload {
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
        let p = ConsolidationPayload {
            session_id: "s1".into(),
            since_sequence: 12,
            bound_scope: None,
        };
        let bytes = p.to_bytes().expect("encode");
        let back = ConsolidationPayload::from_bytes(&bytes).expect("decode");
        assert_eq!(p, back);
    }

    #[test]
    fn roundtrip_with_scope() {
        let p = ConsolidationPayload {
            session_id: "s1".into(),
            since_sequence: 4,
            bound_scope: Some(ScopeTuple {
                tenant: Some("acme".into()),
                ..ScopeTuple::default()
            }),
        };
        let bytes = p.to_bytes().expect("encode");
        let back = ConsolidationPayload::from_bytes(&bytes).expect("decode");
        assert_eq!(p, back);
    }

    #[test]
    fn unknown_fields_rejected() {
        let bytes = br#"{"session_id":"s1","since_sequence":0,"x":1}"#;
        assert!(ConsolidationPayload::from_bytes(bytes).is_err());
    }
}
