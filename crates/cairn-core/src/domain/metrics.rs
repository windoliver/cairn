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
    /// Emitted once per observed verb invocation. The payload is
    /// intentionally body-free: it records dimensions needed for SRE
    /// dashboards without carrying query text, record bodies, snippets,
    /// source paths, or raw error messages.
    #[serde(rename = "verb_invocation")]
    VerbInvocation {
        /// Wall-clock millis since UNIX epoch.
        ts_ms: i64,
        /// Verb name (`ingest`, `search`, `assemble_hot`, ...).
        verb: String,
        /// Surface that handled the verb (`cli`, `mcp`, `sdk`).
        surface: String,
        /// Body-free mode/classification such as `keyword`, `hybrid`,
        /// or `record`.
        mode: Option<String>,
        /// Response status (`committed`, `aborted`, `rejected`).
        status: String,
        /// Wall-clock latency observed by the surface.
        latency_ms: u64,
        /// Body-free error class when status is not committed.
        error: Option<String>,
        /// Budget usage ratio when the verb has an explicit budget.
        budget_used_ratio: Option<f64>,
        /// Degradation state such as `none` or `partial`.
        degradation_state: Option<String>,
    },
    /// Emitted for local search modes after a response is produced.
    /// Query text and snippets are deliberately omitted.
    #[serde(rename = "search_completed")]
    SearchCompleted {
        /// Wall-clock millis since UNIX epoch.
        ts_ms: i64,
        /// Search mode (`keyword`, `semantic`, `hybrid`).
        mode: String,
        /// Number of hits returned to the caller.
        hit_count: u32,
        /// Response latency in milliseconds.
        latency_ms: u64,
        /// Degradation state such as `none` or `partial`.
        degradation_state: String,
        /// Body-free error class when the search did not commit.
        error: Option<String>,
    },
    /// Emitted by local sensors after an observation is accepted or
    /// dropped. Payload bodies, OCR text, transcripts, command output,
    /// file paths, and raw policy messages are deliberately omitted.
    #[serde(rename = "sensor_emission")]
    SensorEmission {
        /// Wall-clock millis since UNIX epoch.
        ts_ms: i64,
        /// Sensor family (`hook`, `terminal`, `clipboard`, ...).
        sensor: String,
        /// Response status (`emitted` or `dropped`).
        status: String,
        /// Wall-clock latency observed by the sensor surface.
        latency_ms: u64,
        /// Sanitized or observed payload size used for budget accounting.
        bytes: u64,
        /// Configured sensor byte budget when known.
        budget_bytes: Option<u64>,
        /// `bytes / budget_bytes` when a non-zero budget is known.
        budget_used_ratio: Option<f64>,
        /// Body-free error class when the observation was dropped.
        error: Option<String>,
        /// Degradation state such as `none` or `partial`.
        degradation_state: Option<String>,
    },
    /// Emitted for record WAL operations after finalization.
    #[serde(rename = "wal_apply")]
    WalApply {
        /// Wall-clock millis since UNIX epoch.
        ts_ms: i64,
        /// WAL kind (`upsert`, `forget_record`, `expire`).
        kind: String,
        /// Operation id.
        operation_id: String,
        /// Final WAL state (`committed`, `aborted`, ...).
        state: String,
        /// Apply latency in milliseconds.
        latency_ms: u64,
        /// Retry count observed by the caller. Record WAL applies are
        /// single-pass today, so this is currently zero.
        retry_count: u32,
    },
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
    /// Emitted after an administrative projection rebuild, such as
    /// `cairn admin reindex --from-db`. Record bodies and projection
    /// payloads are omitted; only aggregate SRE dimensions are exported.
    #[serde(rename = "projection_rebuild")]
    ProjectionRebuild {
        /// Wall-clock millis since UNIX epoch.
        ts_ms: i64,
        /// Projection or rebuild path (`sqlite.from_db`, ...).
        projection: String,
        /// Final rebuild status (`committed` or `aborted`).
        status: String,
        /// Wall-clock latency observed by the caller.
        latency_ms: u64,
        /// Number of projection rows rebuilt when known.
        records_rebuilt: u64,
        /// Queue lag in milliseconds when known; zero is the local
        /// unknown/no-lag sentinel for immediate admin rebuilds.
        queue_lag_ms: i64,
        /// Retry count observed by the caller.
        retry_count: u32,
        /// Body-free error class when status is not committed.
        error: Option<String>,
        /// Degradation state such as `none` or `partial`.
        degradation_state: Option<String>,
    },
    /// Emitted when an active task-trace canvas is rendered into the
    /// hot-memory current-task section. The payload is intentionally
    /// body-free: IDs are hashed and canvas text / node summaries are
    /// omitted so metrics stay safe for append-only observability.
    #[serde(rename = "trace_canvas_rendered")]
    TraceCanvasRendered {
        /// Wall-clock millis since UNIX epoch.
        ts_ms: i64,
        /// SHA-256 of the canvas session id.
        session_id_hash: String,
        /// SHA-256 of the rendered canvas id.
        canvas_id_hash: String,
        /// Monotonic canvas version rendered by the caller.
        version: i64,
        /// Number of canvas nodes available to the renderer.
        node_count: u32,
        /// Number of canvas edges available to the renderer.
        edge_count: u32,
        /// Bytes rendered into the hot prefix for this section.
        bytes: u64,
        /// Effective canvas-local render budget.
        budget_bytes: u64,
        /// True when the canvas identified an active node.
        active_node: bool,
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
        ///
        /// **`not_before_ms == 0` is the "lag unknown" sentinel** and
        /// MUST be reported as `queue_lag_ms = 0` rather than the raw
        /// subtraction (`now_ms - 0` is the entire Unix epoch, ~1.7e12
        /// ms, and would corrupt dashboards). Production enqueuers
        /// should stamp `not_before_ms = now_ms` (or a future
        /// scheduled time) so this field becomes a real measurement;
        /// the clamp protects against drift from new call sites that
        /// forget. See `cairn-workflows::scheduler::worker::execute_one`.
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
    fn trace_canvas_rendered_round_trips_without_body_fields() {
        let event = MetricEvent::TraceCanvasRendered {
            ts_ms: 1_700_000_000_000,
            session_id_hash: "sha256:session".into(),
            canvas_id_hash: "sha256:canvas".into(),
            version: 3,
            node_count: 2,
            edge_count: 1,
            bytes: 512,
            budget_bytes: 1024,
            active_node: true,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"trace_canvas_rendered\""));
        assert!(!json.contains("Issue 134"));
        assert!(!json.contains("finish trace canvas hot memory"));
        let back: MetricEvent = serde_json::from_str(&json).expect("deserialize");
        match back {
            MetricEvent::TraceCanvasRendered {
                version,
                node_count,
                edge_count,
                bytes,
                budget_bytes,
                active_node,
                ..
            } => {
                assert_eq!(version, 3);
                assert_eq!(node_count, 2);
                assert_eq!(edge_count, 1);
                assert_eq!(bytes, 512);
                assert_eq!(budget_bytes, 1024);
                assert!(active_node);
            }
            _ => panic!("expected TraceCanvasRendered"),
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

    #[test]
    fn verb_invocation_metric_is_body_free() {
        let event = MetricEvent::VerbInvocation {
            ts_ms: 1,
            verb: "ingest".into(),
            surface: "cli".into(),
            mode: Some("body".into()),
            status: "committed".into(),
            latency_ms: 12,
            error: None,
            budget_used_ratio: Some(0.25),
            degradation_state: Some("none".into()),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"verb_invocation\""));
        assert!(json.contains("\"verb\":\"ingest\""));
        assert!(json.contains("\"budget_used_ratio\":0.25"));
        assert!(!json.contains("secret"));
        assert!(!json.contains("body_text"));
    }

    #[test]
    fn sensor_emission_metric_is_body_free_and_exposes_budget() {
        let event = MetricEvent::SensorEmission {
            ts_ms: 1,
            sensor: "clipboard".into(),
            status: "dropped".into(),
            latency_ms: 7,
            bytes: 64,
            budget_bytes: Some(1024),
            budget_used_ratio: Some(0.0625),
            error: Some("privacy_denied".into()),
            degradation_state: Some("none".into()),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"sensor_emission\""));
        assert!(json.contains("\"latency_ms\":7"));
        assert!(json.contains("\"budget_used_ratio\":0.0625"));
        assert!(!json.contains("copied secret text"));
        assert!(!json.contains("body"));
    }

    #[test]
    fn projection_rebuild_metric_exposes_queue_and_retry_fields() {
        let event = MetricEvent::ProjectionRebuild {
            ts_ms: 1,
            projection: "sqlite.from_db".into(),
            status: "committed".into(),
            latency_ms: 123,
            records_rebuilt: 3,
            queue_lag_ms: 42,
            retry_count: 0,
            error: None,
            degradation_state: Some("none".into()),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"projection_rebuild\""));
        assert!(json.contains("\"queue_lag_ms\":42"));
        assert!(json.contains("\"retry_count\":0"));
        let back: MetricEvent = serde_json::from_str(&json).expect("deserialize");
        match back {
            MetricEvent::ProjectionRebuild {
                records_rebuilt,
                queue_lag_ms,
                ..
            } => {
                assert_eq!(records_rebuilt, 3);
                assert_eq!(queue_lag_ms, 42);
            }
            _ => panic!("expected ProjectionRebuild"),
        }
    }
}
