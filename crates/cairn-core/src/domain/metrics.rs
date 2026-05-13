//! `MetricEvent` — wire-shape for events the runtime emits to a
//! `MetricsSink`. One JSON line per event when written to
//! `.cairn/metrics.jsonl`.

use serde::{Deserialize, Serialize};

use crate::domain::hot_prefix::SourceWatermarks;

/// One line of `.cairn/metrics.jsonl` (or one tracing event field set).
///
/// `event` is the discriminator. New variants are additive — readers
/// MUST tolerate unknown variants (`#[non_exhaustive]` on the enum).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
#[non_exhaustive]
pub enum MetricEvent {
    /// Emitted exactly once per `assemble_hot` call.
    #[serde(rename = "hot_prefix_assembled")]
    HotPrefixAssembled {
        /// Wall-clock millis since UNIX epoch.
        ts_ms: i64,
        /// Vault id (from `.cairn/vault.id`).
        vault_id: String,
        /// Issuing agent identity in canonical text form.
        agent_id: String,
        /// SHA-256 of the canonical-JSON recipe.
        recipe_hash: String,
        /// Wall-clock latency observed by the verb (cache hit or miss).
        latency_ms: u64,
        /// Bytes of assembled prefix (after trimming to budget).
        bytes: u64,
        /// Configured `vault.hot_memory.max_bytes`.
        budget_bytes: u64,
        /// `bytes / budget_bytes` (0.0 when `budget_bytes == 0`).
        budget_used_ratio: f64,
        /// True iff the prefix was served from `hot_prefix_cache`.
        cache_hit: bool,
        /// Snapshot of every source-class watermark at assembly time.
        watermarks: SourceWatermarks,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_prefix_assembled_round_trips_through_json() {
        let event = MetricEvent::HotPrefixAssembled {
            ts_ms: 1_700_000_000_000,
            vault_id: "v1".into(),
            agent_id: "agt:cairn-cli:default:writer:v1".into(),
            recipe_hash: "deadbeef".into(),
            latency_ms: 12,
            bytes: 1024,
            budget_bytes: 25_600,
            budget_used_ratio: 0.04,
            cache_hit: false,
            watermarks: SourceWatermarks::default(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"hot_prefix_assembled\""));
        let back: MetricEvent = serde_json::from_str(&json).expect("deserialize");
        let MetricEvent::HotPrefixAssembled { latency_ms, .. } = back;
        assert_eq!(latency_ms, 12);
    }
}
