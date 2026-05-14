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
    /// Emitted at the end of each `EvaluationWorkflow` sweep (issue
    /// #91, brief §15). Drives release gating — CI rejects merges
    /// whose `failed` count grows beyond the previous baseline.
    #[serde(rename = "evaluation_completed")]
    EvaluationCompleted {
        /// Wall-clock millis since UNIX epoch — captured at the
        /// start of the sweep so retries against the same payload
        /// emit the same metric.
        ts_ms: i64,
        /// `report_target_id` of the synthesized `Reasoning` record
        /// the handler upserted (when `write_report_record = true`).
        /// Empty when the report-record arm was disabled.
        report_target_id: String,
        /// Total number of `GoldenCheck`s executed this sweep.
        checks_run: u32,
        /// Subset of `checks_run` that returned `CheckOutcome::Passed`.
        passed: u32,
        /// Subset of `checks_run` that returned `CheckOutcome::Failed`.
        failed: u32,
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
        match back {
            MetricEvent::HotPrefixAssembled { latency_ms, .. } => assert_eq!(latency_ms, 12),
            _ => panic!("expected HotPrefixAssembled"),
        }
    }

    #[test]
    fn evaluation_completed_round_trips_through_json() {
        let event = MetricEvent::EvaluationCompleted {
            ts_ms: 1_700_000_000_000,
            report_target_id: "01JTESTID0000000000000000".into(),
            checks_run: 2,
            passed: 2,
            failed: 0,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"evaluation_completed\""));
        let back: MetricEvent = serde_json::from_str(&json).expect("deserialize");
        match back {
            MetricEvent::EvaluationCompleted {
                checks_run,
                passed,
                failed,
                ..
            } => {
                assert_eq!(checks_run, 2);
                assert_eq!(passed, 2);
                assert_eq!(failed, 0);
            }
            _ => panic!("expected EvaluationCompleted"),
        }
    }
}
