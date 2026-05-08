//! Cairn background workflows host.
//!
//! Brief §10 (v0.1 row) + §19.a item 5: durable `tokio` orchestrator
//! backed by a `SQLite` job table. Persistence lives in
//! [`SqliteJobStore`] which satisfies
//! [`cairn_core::contract::JobStore`]; the scheduler that consumes it
//! lands alongside the first concrete workflow types.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod consent_mirror;
pub mod sqlite_store;

pub use consent_mirror::{ConsentLogMaterializer, MirrorError};
pub use sqlite_store::{SqliteJobStore, SqliteJobStoreInitError};

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

/// In-process `WorkflowOrchestrator`. The persistence half (durable
/// `SQLite` job table + lease state machine) lands in this PR via
/// [`SqliteJobStore`]; the scheduler loop (worker pool, reaper,
/// heartbeat) and startup wiring land in the follow-up. Capability bits
/// stay `false` until that follow-up because nothing here actually
/// executes leased jobs yet — flipping them earlier would let callers
/// route work into a runner that never runs.
#[derive(Default)]
pub struct InProcessOrchestrator;

#[async_trait::async_trait]
impl WorkflowOrchestrator for InProcessOrchestrator {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &WorkflowOrchestratorCapabilities {
        static CAPS: WorkflowOrchestratorCapabilities = WorkflowOrchestratorCapabilities {
            durable: false,
            crash_safe: false,
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
