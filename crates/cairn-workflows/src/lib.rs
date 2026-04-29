//! Cairn background workflows host.
//!
//! Brief §10 (v0.1 row) + §19.a item 5: durable `tokio` orchestrator
//! backed by a `SQLite` job table. Persistence lives in
//! [`SqliteJobStore`] which satisfies
//! [`cairn_core::contract::JobStore`]; the scheduler that consumes it
//! lands alongside the first concrete workflow types.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod sqlite_store;

pub use sqlite_store::SqliteJobStore;

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

/// In-process `WorkflowOrchestrator` advertising the durable +
/// crash-safe capabilities backed by [`SqliteJobStore`]. The scheduler
/// loop (worker pool, reaper, heartbeat) lands in the follow-up that
/// wires it into `cairn-cli` startup.
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
