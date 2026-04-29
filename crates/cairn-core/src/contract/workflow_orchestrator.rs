//! `WorkflowOrchestrator` contract (brief §4 row 3).
//!
//! P0 default: tokio + `SQLite`-backed job table (#89). Optional Temporal
//! adapter is a P1+ swap — same trait.

use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `WorkflowOrchestrator`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

/// Static capability declaration for a `WorkflowOrchestrator` impl.
// Three flags cover distinct orchestration dimensions; a state machine adds
// indirection with no clarity gain here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkflowOrchestratorCapabilities {
    /// Whether the orchestrator persists job state across restarts.
    pub durable: bool,
    /// Whether the orchestrator can recover from a crash mid-workflow.
    pub crash_safe: bool,
    /// Whether the orchestrator supports cron-style recurring schedules.
    pub cron_schedules: bool,
}

/// Workflow orchestration contract.
///
/// Brief §4 row 3: P0 default is tokio + `SQLite`-backed job table (#89).
/// Optional Temporal adapter is a P1+ swap — same trait surface.
#[async_trait::async_trait]
pub trait WorkflowOrchestrator: Send + Sync {
    /// Stable identifier of the registered plugin instance.
    fn name(&self) -> &str;

    /// Static capability advertisement (brief §4.1).
    fn capabilities(&self) -> &WorkflowOrchestratorCapabilities;

    /// Range of `WorkflowOrchestrator::CONTRACT_VERSION` values this impl accepts.
    fn supported_contract_versions(&self) -> VersionRange;
}

/// Static identity descriptor for a [`WorkflowOrchestrator`] plugin (§4.1).
///
/// Carries the two associated consts the `register_plugin_with!` macro checks
/// before construction. See [`MemoryStorePlugin`](crate::contract::MemoryStorePlugin)
/// for the design rationale.
pub trait WorkflowOrchestratorPlugin: WorkflowOrchestrator + Sized {
    /// Stable plugin name, checked statically before construction (§4.1).
    const NAME: &'static str;
    /// Version range checked statically before construction (§4.1).
    const SUPPORTED_VERSIONS: VersionRange;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubOrch;

    #[async_trait::async_trait]
    impl WorkflowOrchestrator for StubOrch {
        fn name(&self) -> &'static str {
            Self::NAME
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
            Self::SUPPORTED_VERSIONS
        }
    }

    impl WorkflowOrchestratorPlugin for StubOrch {
        const NAME: &'static str = "stub-orch";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    #[test]
    fn dyn_compatible() {
        let o: Box<dyn WorkflowOrchestrator> = Box::new(StubOrch);
        assert_eq!(o.name(), "stub-orch");
        assert!(o.supported_contract_versions().accepts(CONTRACT_VERSION));
    }

    #[test]
    fn static_consts_accessible() {
        assert_eq!(StubOrch::NAME, "stub-orch");
        assert!(StubOrch::SUPPORTED_VERSIONS.accepts(CONTRACT_VERSION));
    }
}
