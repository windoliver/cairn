//! `EvaluationPayload` — what an enqueued evaluation sweep carries.

use cairn_core::contract::job_store::JobPayload;
use cairn_core::domain::ScopeTuple;
use serde::{Deserialize, Serialize};

/// One enqueued evaluation-sweep request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationPayload {
    /// Wall-clock millis since UNIX epoch — captured at enqueue time
    /// so retries against the same payload produce identical
    /// metrics (issue #91 AC: "Evaluation outputs are deterministic
    /// enough for release gating").
    pub ts_ms: i64,
    /// IDs of the `GoldenCheck`s to run. Empty list means "use
    /// `EvaluationConfig::checks`", which itself empty means "run
    /// every registered check".
    #[serde(default)]
    pub check_ids: Vec<String>,
    /// Bound scope the enqueuing caller verified. Every golden check
    /// filters its store reads by this scope; the report record is
    /// stamped with it. Brief §6 multi-tenant isolation: omitting
    /// the scope on a multi-tenant deployment lets one sweep
    /// aggregate records across tenants. `None` is single-tenant
    /// P0 — no scope binding to enforce (round-1 adversarial review
    /// #4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_scope: Option<ScopeTuple>,
}

impl EvaluationPayload {
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
        let p = EvaluationPayload {
            ts_ms: 1_700_000_000_000,
            check_ids: vec!["orphan".into()],
            bound_scope: None,
        };
        let bytes = p.to_bytes().expect("encode");
        let back = EvaluationPayload::from_bytes(&bytes).expect("decode");
        assert_eq!(p, back);
    }

    #[test]
    fn roundtrip_with_scope() {
        let p = EvaluationPayload {
            ts_ms: 0,
            check_ids: vec![],
            bound_scope: Some(ScopeTuple {
                tenant: Some("acme".into()),
                ..ScopeTuple::default()
            }),
        };
        let bytes = p.to_bytes().expect("encode");
        let back = EvaluationPayload::from_bytes(&bytes).expect("decode");
        assert_eq!(p, back);
    }

    #[test]
    fn unknown_fields_rejected() {
        let bytes = br#"{"ts_ms":0,"z":3}"#;
        assert!(EvaluationPayload::from_bytes(bytes).is_err());
    }
}
