//! `DreamPayload` — what an enqueued dream-distillation job carries.
//!
//! JSON-encoded for the same audit-trail reason as
//! [`crate::consolidation::ConsolidationPayload`].

use cairn_core::contract::job_store::JobPayload;
use cairn_core::domain::ScopeTuple;
use serde::{Deserialize, Serialize};

/// One enqueued dream-distillation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DreamPayload {
    /// Logical key the handler uses when building the synthesized
    /// dream record's stable `target_id`. For session-scoped Light
    /// dreams this is the session id; future Deep dreams may pass a
    /// folder or tenant key.
    pub key: String,
    /// Bound scope (tenant / workspace / user / agent) the enqueuing
    /// caller verified — used to filter store reads so a job
    /// dispatched on behalf of one tenant cannot read another. `None`
    /// is single-tenant P0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_scope: Option<ScopeTuple>,
}

impl DreamPayload {
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
        let p = DreamPayload {
            key: "sess-1".into(),
            bound_scope: None,
        };
        let bytes = p.to_bytes().expect("encode");
        let back = DreamPayload::from_bytes(&bytes).expect("decode");
        assert_eq!(p, back);
    }

    #[test]
    fn roundtrip_with_scope() {
        let p = DreamPayload {
            key: "sess-1".into(),
            bound_scope: Some(ScopeTuple {
                tenant: Some("acme".into()),
                ..ScopeTuple::default()
            }),
        };
        let bytes = p.to_bytes().expect("encode");
        let back = DreamPayload::from_bytes(&bytes).expect("decode");
        assert_eq!(p, back);
    }

    #[test]
    fn unknown_fields_rejected() {
        let bytes = br#"{"key":"sess-1","x":1}"#;
        assert!(DreamPayload::from_bytes(bytes).is_err());
    }
}
