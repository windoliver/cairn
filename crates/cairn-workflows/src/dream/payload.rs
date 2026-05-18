//! `DreamPayload` — what an enqueued dream-distillation job carries.
//!
//! JSON-encoded for the same audit-trail reason as
//! [`crate::consolidation::ConsolidationPayload`].

use cairn_core::contract::job_store::JobPayload;
use cairn_core::{config::DreamTier, domain::ScopeTuple};
use serde::{Deserialize, Serialize};

/// One enqueued dream-distillation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DreamPayload {
    /// Dream tier being run. Defaults to Light Sleep so old queued
    /// v0.1 payloads can still drain after an upgrade.
    #[serde(default)]
    pub tier: DreamTier,
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
    /// Recommended `EnqueueRequest::queue_key` for this payload.
    ///
    /// Enqueuers SHOULD pass this string as `queue_key` so the
    /// scheduler serializes every dream job for a given
    /// `(scope, key)` pair. Without scheduler-level serialization,
    /// two workers handling concurrent same-target jobs race the
    /// pre- and post-LLM existence checks in
    /// [`crate::dream::handler::DreamHandler`] and can publish
    /// conflicting version chains for the same deterministic
    /// `target_id`. The pre/post checks narrow the window but
    /// cannot close it without serialization at enqueue
    /// (round-5 and round-7 adversarial reviews #1 / #2).
    ///
    /// The key intentionally omits `sources_hash`: the active set
    /// of sources is computed inside the handler, so the enqueuer
    /// cannot know the eventual `target_id`. Serializing on
    /// `(scope, key)` is a sufficient guard — two windows over the
    /// same logical key drain in order rather than racing each
    /// other.
    #[must_use]
    pub fn recommended_queue_key(&self) -> String {
        let scope_wire = self
            .bound_scope
            .as_ref()
            .map(cairn_core::domain::ScopeTuple::canonical_wire)
            .unwrap_or_default();
        format!(
            "dream:{tier}:{scope_wire}:{key}",
            tier = self.tier.as_str(),
            key = self.key
        )
    }

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
            tier: DreamTier::LightSleep,
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
            tier: DreamTier::RemSleep,
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

    #[test]
    fn queue_key_distinguishes_by_scope() {
        let a = DreamPayload {
            tier: DreamTier::LightSleep,
            key: "k".into(),
            bound_scope: None,
        }
        .recommended_queue_key();
        let b = DreamPayload {
            tier: DreamTier::LightSleep,
            key: "k".into(),
            bound_scope: Some(ScopeTuple {
                tenant: Some("acme".into()),
                ..ScopeTuple::default()
            }),
        }
        .recommended_queue_key();
        assert_ne!(a, b, "scope must split the queue key");
        // Same scope + key → same queue key.
        let c = DreamPayload {
            tier: DreamTier::LightSleep,
            key: "k".into(),
            bound_scope: None,
        }
        .recommended_queue_key();
        assert_eq!(a, c);

        let d = DreamPayload {
            tier: DreamTier::RemSleep,
            key: "k".into(),
            bound_scope: None,
        }
        .recommended_queue_key();
        assert_ne!(a, d, "tier must split the queue key");
    }

    #[test]
    fn legacy_payload_defaults_to_light_sleep() {
        let back = DreamPayload::from_bytes(br#"{"key":"sess-legacy"}"#).expect("decode");
        assert_eq!(back.tier, DreamTier::LightSleep);
    }
}
