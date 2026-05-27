//! Progress event emitted by long-running admin workflows (backfill, etc.).
//!
//! Brief §5 (pipeline) / §10 (workflows): long-running admin operations
//! (backfill, consolidate, promote, expire, evaluate) emit a stream of
//! `ProgressEvent`s so the CLI `--watch` flag and SDK subscribers can
//! observe progress without polling. Scheduler-side wiring is deferred to
//! a follow-up issue; this module only defines the domain type.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Opaque identifier for a long-running workflow instance.
///
/// Format: `"wf-<ULID>"`. Stable across restarts; persisted in the job
/// store alongside the backfill spec.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkflowId(pub String);

impl WorkflowId {
    /// Mint a fresh, globally-unique workflow id.
    #[must_use]
    pub fn new() -> Self {
        Self(format!("wf-{}", Ulid::new()))
    }
}

impl Default for WorkflowId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for WorkflowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The logical phase of a workflow, carried in every [`ProgressEvent`].
///
/// `#[non_exhaustive]` — future phases (e.g. `Paused`, `Cancelled`) will
/// be added without breaking existing match arms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ProgressKind {
    /// Workflow has been accepted and the first unit of work is about to begin.
    Started,
    /// Incremental progress checkpoint (processed N of total items).
    Tick,
    /// All work finished successfully.
    Completed,
    /// Workflow terminated abnormally.
    Failed {
        /// Machine-readable error code (e.g. `"rate_limit_exceeded"`).
        code: String,
        /// Human-readable explanation.
        msg: String,
    },
}

/// A single progress snapshot emitted during a long-running workflow.
///
/// Adapters that implement [`BackfillSpawner`] publish these events on a
/// channel; the CLI `--watch` flag drains that channel and renders them.
///
/// [`BackfillSpawner`]: crate::contract::backfill_spawner::BackfillSpawner
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProgressEvent {
    /// Identifies the workflow run this event belongs to.
    pub workflow_id: WorkflowId,
    /// Wall-clock timestamp when the event was emitted.
    pub at: DateTime<Utc>,
    /// Logical phase.
    pub kind: ProgressKind,
    /// Number of records/units processed so far.
    pub processed: u64,
    /// Total expected units, if known at emission time.
    pub total: Option<u64>,
    /// Adapter-specific detail bag (schema is adapter-defined).
    pub detail: serde_json::Value,
}

impl ProgressEvent {
    /// Construct a minimal event with `total` and `detail` unset.
    #[must_use]
    pub fn new(workflow_id: WorkflowId, kind: ProgressKind, processed: u64) -> Self {
        Self {
            workflow_id,
            at: Utc::now(),
            kind,
            processed,
            total: None,
            detail: serde_json::Value::Null,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn progress_kind_started_serializes_round_trip() {
        let ev = ProgressEvent::new(WorkflowId::new(), ProgressKind::Started, 0);
        let s = serde_json::to_string(&ev).expect("serialize");
        let back: ProgressEvent = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(ev, back);
    }

    #[test]
    fn progress_kind_failed_carries_code_and_msg() {
        let ev = ProgressEvent::new(
            WorkflowId::new(),
            ProgressKind::Failed {
                code: "X".into(),
                msg: "boom".into(),
            },
            42,
        );
        let s = serde_json::to_string(&ev).expect("serialize");
        let back: ProgressEvent = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(ev, back);
    }

    #[test]
    fn workflow_id_display_has_wf_prefix() {
        let id = WorkflowId::new();
        assert!(id.to_string().starts_with("wf-"), "{id}");
    }

    #[test]
    fn workflow_id_default_is_unique() {
        let a = WorkflowId::default();
        let b = WorkflowId::default();
        assert_ne!(a, b, "each default WorkflowId must be unique");
    }
}
