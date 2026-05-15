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
    /// Emitted on every successful `JobStore::lease` return (issue
    /// #92, spec §4.6). Marks the moment a worker has taken
    /// ownership of a queued row. Emitted **after** the lease commit
    /// lands — sink failures never abort the worker (spec §4.13).
    #[serde(rename = "workflow_job_started")]
    WorkflowJobStarted {
        /// Wall-clock millis since UNIX epoch at lease time.
        ts_ms: i64,
        /// `JobId` rendered as a string.
        job_id: String,
        /// `JobKind` rendered as a string (e.g. `"dream.light"`).
        kind: String,
        /// Worker-visible attempt count for this delivery (1-based).
        attempts: u32,
        /// `now_ms − not_before_ms` at lease time — surfaces queue
        /// latency for dashboards.
        queue_lag_ms: i64,
        /// Step-level idempotency key, if the enqueuer set one.
        dedupe_key: Option<String>,
    },
    /// Emitted on every successful `JobStore::complete` return
    /// (issue #92, spec §4.6). Pairs with [`Self::WorkflowJobStarted`]
    /// via `job_id` — `duration_ms` is worker-local so it stays
    /// monotonic across leases.
    #[serde(rename = "workflow_job_completed")]
    WorkflowJobCompleted {
        /// Wall-clock millis since UNIX epoch at complete time.
        ts_ms: i64,
        /// `JobId` rendered as a string.
        job_id: String,
        /// `JobKind` rendered as a string.
        kind: String,
        /// Attempt count that finished successfully.
        attempts: u32,
        /// `ts_ms − started_at_ms` measured by the worker.
        duration_ms: u64,
    },
    /// Emitted on every `JobStore::fail` (retry and permanent) and
    /// on each reaper-reclaimed lease (issue #92, spec §4.6).
    /// `failure_class` is the snake-case form of `FailureClass`;
    /// `will_retry_at_ms` is `Some(_)` for retries (next eligible
    /// time) and `None` for terminal failures.
    #[serde(rename = "workflow_job_failed")]
    WorkflowJobFailed {
        /// Wall-clock millis since UNIX epoch at fail time.
        ts_ms: i64,
        /// `JobId` rendered as a string.
        job_id: String,
        /// `JobKind` rendered as a string.
        kind: String,
        /// Attempt count that failed.
        attempts: u32,
        /// `"retry"` or `"permanent"`.
        disposition: String,
        /// `FailureClass::as_str()` — `snake_case` discriminator.
        failure_class: String,
        /// Worker-supplied failure message (truncated upstream as needed).
        last_error: String,
        /// Absent for terminal failures; present for retries (next
        /// eligible wall-clock millis).
        will_retry_at_ms: Option<i64>,
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

    #[test]
    fn workflow_job_started_round_trips() {
        let event = MetricEvent::WorkflowJobStarted {
            ts_ms: 1_700_000_000_000,
            job_id: "j-1".into(),
            kind: "dream.light".into(),
            attempts: 1,
            queue_lag_ms: 42,
            dedupe_key: Some("op-x".into()),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"workflow_job_started\""));
        let back: MetricEvent = serde_json::from_str(&json).expect("deserialize");
        match back {
            MetricEvent::WorkflowJobStarted {
                queue_lag_ms,
                dedupe_key,
                ..
            } => {
                assert_eq!(queue_lag_ms, 42);
                assert_eq!(dedupe_key.as_deref(), Some("op-x"));
            }
            _ => panic!("expected WorkflowJobStarted"),
        }
    }

    #[test]
    fn workflow_job_completed_round_trips() {
        let event = MetricEvent::WorkflowJobCompleted {
            ts_ms: 1,
            job_id: "j-1".into(),
            kind: "dream.light".into(),
            attempts: 1,
            duration_ms: 123,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"workflow_job_completed\""));
        let back: MetricEvent = serde_json::from_str(&json).expect("deserialize");
        match back {
            MetricEvent::WorkflowJobCompleted { duration_ms, .. } => {
                assert_eq!(duration_ms, 123);
            }
            _ => panic!("expected WorkflowJobCompleted"),
        }
    }

    #[test]
    fn workflow_job_failed_round_trips() {
        let event = MetricEvent::WorkflowJobFailed {
            ts_ms: 1,
            job_id: "j-1".into(),
            kind: "dream.light".into(),
            attempts: 3,
            disposition: "retry".into(),
            failure_class: "transient".into(),
            last_error: "boom".into(),
            will_retry_at_ms: Some(1_500),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"workflow_job_failed\""));
        assert!(json.contains("\"failure_class\":\"transient\""));
        let back: MetricEvent = serde_json::from_str(&json).expect("deserialize");
        match back {
            MetricEvent::WorkflowJobFailed {
                disposition,
                failure_class,
                will_retry_at_ms,
                ..
            } => {
                assert_eq!(disposition, "retry");
                assert_eq!(failure_class, "transient");
                assert_eq!(will_retry_at_ms, Some(1_500));
            }
            _ => panic!("expected WorkflowJobFailed"),
        }
    }
}
