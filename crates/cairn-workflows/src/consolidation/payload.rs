//! Serde-encoded payload carried in `workflow_jobs.payload`.
//! `Bincode` would be smaller but JSON gives us auditability for free
//! and keeps replay logs human-readable.

use cairn_core::contract::job_store::JobPayload;
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
