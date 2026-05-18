//! Cairn background workflows host.
//!
//! Brief §10 (v0.1 row) + §19.a item 5: durable `tokio` orchestrator
//! backed by a `SQLite` job table. Persistence lives in
//! [`SqliteJobStore`] which satisfies
//! [`cairn_core::contract::JobStore`]; the scheduler that consumes it
//! lands alongside the first concrete workflow types.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod consent_mirror;
pub mod consolidation;
pub mod drainer;
pub mod dream;
pub mod evaluation;
pub mod expiration;
pub mod planners;
pub mod salience_decay;
pub mod scheduler;
pub mod sqlite_apply;
pub mod sqlite_store;
pub mod synthetic;
pub mod trace_canvas;
pub mod workflows;

#[cfg(test)]
mod test_support;

pub use consent_mirror::{ConsentLogMaterializer, MirrorError};
pub use consolidation::{
    CONSOLIDATION_KIND, ConsolidationForgetCleanupHandler, ConsolidationHandler,
    ConsolidationPayload, FORGET_CLEANUP_KIND, ForgetCleanupPayload,
};
pub use drainer::{
    DrainStats, FileWorkflowCheckpointStore, FlushPlanApply, FlushPlanApplyOutcome, Workflow,
    WorkflowCheckpointStore, WorkflowContext, WorkflowDrainer, WorkflowError,
    WorkflowStatusSnapshot,
};
pub use dream::{
    DREAM_KIND, DreamEnqueueDecision, DreamHandler, DreamPayload, enqueue_tier,
    enqueue_tier_with_dedupe_token,
};
pub use evaluation::{
    CheckOutcome, EVALUATION_KIND, EvaluationHandler, EvaluationPayload, EvaluationReport,
    GoldenCheck, OrphanCheck, TombstoneConsistencyCheck, default_checks as default_golden_checks,
};
pub use expiration::{
    EXPIRATION_KIND, ExpirationHandler, ExpirationPayload, ExpirationSweepReport,
};
pub use planners::{
    ConsolidatePlanSource, ExpirePlanSource, PromotePlanSource, ReflectionPlanSource,
};
pub use scheduler::{Clock, MockClock, Scheduler, SchedulerConfig, SystemClock};
pub use sqlite_apply::SqliteFlushPlanApply;
pub use sqlite_store::{SqliteJobStore, SqliteJobStoreInitError};
pub use trace_canvas::{
    TRACE_CANVAS_KIND, TraceCanvasEnqueueDecision, TraceCanvasHandler, TraceCanvasMaterializer,
    TraceCanvasPayload, TraceCanvasProjection, enqueue_trace_canvas_step,
};
pub use workflows::{
    ConsolidateWorkflow, ExpireWorkflow, PromoteWorkflow, ReflectionWorkflow, WorkflowPlanSource,
};

use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::contract::workflow_orchestrator::{
    CONTRACT_VERSION, WorkflowOrchestrator, WorkflowOrchestratorCapabilities,
};
use cairn_core::register_plugin;

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-workflows";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Accepted host contract version range. Single source of truth for both the
/// trait impl's `supported_contract_versions()` and the const-eval guard.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

/// In-process `WorkflowOrchestrator`. The scheduler loop (worker pool,
/// reaper, heartbeat) and `SqliteJobStore` persistence are now fully
/// wired into `cairn mcp serve` (Task 17). Capability bits reflect the
/// live runtime: jobs are durably persisted in `SQLite` WAL mode and
/// crash-recovered on the next `cairn mcp serve` start via the lease
/// reaper.
#[derive(Default)]
pub struct InProcessOrchestrator;

#[async_trait::async_trait]
impl WorkflowOrchestrator for InProcessOrchestrator {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &WorkflowOrchestratorCapabilities {
        static CAPS: WorkflowOrchestratorCapabilities = WorkflowOrchestratorCapabilities {
            durable: true,
            crash_safe: true,
            cron_schedules: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        ACCEPTED_RANGE
    }
}

const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

register_plugin!(
    WorkflowOrchestrator,
    InProcessOrchestrator,
    "cairn-workflows",
    MANIFEST_TOML
);
