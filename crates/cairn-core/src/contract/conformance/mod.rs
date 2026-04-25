//! Two-tier conformance suite for plugins (brief §4.1).
//!
//! Tier-1 cases are pure trait-surface checks: they instantiate the plugin
//! against a fresh registry, verify name / version / manifest agreement,
//! and assert capability self-consistency. Every plugin must pass tier-1
//! to be considered conformant.
//!
//! Tier-2 cases exercise verb behaviour. They are stubbed `Pending` until
//! per-impl PRs land — at which point each contract module's tier-2 case
//! body is replaced with a real check.
//!
//! The suite lives in `cairn-core` (rather than `cairn-test-fixtures`)
//! because `cairn plugins verify` is a production code path. All cases
//! are pure functions: zero I/O, no adapter deps.
//!
//! Entry point: [`run_conformance_for_plugin`].
//!
//! See `docs/superpowers/specs/2026-04-25-plugin-host-list-verify-design.md`
//! §4 for the full design.

pub mod mcp_server;
pub mod memory_store;
pub mod sensor_ingress;
pub mod workflow_orchestrator;

use crate::contract::manifest::ContractKind;
use crate::contract::registry::{PluginError, PluginName, PluginRegistry};

/// Outcome of running a single conformance case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseOutcome {
    /// Stable case identifier (`snake_case`, brief-aligned).
    pub id: &'static str,
    /// Tier this case belongs to.
    pub tier: Tier,
    /// Case result.
    pub status: CaseStatus,
}

/// Conformance tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Always run; failure makes the plugin non-conformant.
    One,
    /// Verb-behaviour cases; stubbed `Pending` until per-impl PRs land.
    Two,
}

impl Tier {
    /// Numeric encoding used in JSON output (`1` or `2`).
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            Tier::One => 1,
            Tier::Two => 2,
        }
    }
}

/// Result of a single conformance case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaseStatus {
    /// Case ran to completion and the invariant held.
    Ok,
    /// Case body is a stub awaiting a real implementation.
    Pending {
        /// Static reason string explaining what's pending.
        reason: &'static str,
    },
    /// Case ran and the invariant did not hold.
    Failed {
        /// Human-readable failure message.
        message: String,
    },
}

impl CaseStatus {
    /// `true` iff this status counts as a tier-1 failure when running with
    /// `--strict` interpreting `Pending` as failure.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(self, CaseStatus::Failed { .. })
    }

    /// Stable string discriminator used in JSON output.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            CaseStatus::Ok => "ok",
            CaseStatus::Pending { .. } => "pending",
            CaseStatus::Failed { .. } => "failed",
        }
    }
}

/// Run the full conformance suite (tier-1 + tier-2) against the plugin
/// identified by `name` in `registry`.
///
/// Returns an empty vec if the plugin is not registered. Otherwise the
/// returned vec contains tier-1 cases first, then tier-2 cases, in
/// declaration order.
///
/// # Errors
/// This function does not return `Result`; case-level errors are encoded
/// in `CaseStatus::Failed`. The only way this function "fails" is by
/// returning an empty vec when `name` is not in the registry.
#[must_use]
pub fn run_conformance_for_plugin(
    registry: &PluginRegistry,
    name: &PluginName,
) -> Vec<CaseOutcome> {
    let Some(manifest) = registry.parsed_manifest(name) else {
        return Vec::new();
    };
    match manifest.contract() {
        ContractKind::MemoryStore => memory_store::run(registry, name),
        ContractKind::WorkflowOrchestrator => workflow_orchestrator::run(registry, name),
        ContractKind::SensorIngress => sensor_ingress::run(registry, name),
        ContractKind::MCPServer => mcp_server::run(registry, name),
        // P0 ships no bundled plugins for these — verify returns empty.
        ContractKind::LLMProvider | ContractKind::FrontendAdapter | ContractKind::AgentProvider => {
            Vec::new()
        }
    }
}

/// Internal helper: tier-1 `manifest_matches_host` case.
///
/// Calls [`crate::contract::manifest::PluginManifest::verify_compatible_with`]
/// with the plugin's runtime name, the manifest's declared contract kind,
/// and the host's `CONTRACT_VERSION` for that contract. The host version
/// is supplied by callers (per-contract `run` functions) so this helper
/// stays generic over contract kind.
pub(super) fn tier1_manifest_matches_host(
    registry: &PluginRegistry,
    name: &PluginName,
    host_version: crate::contract::version::ContractVersion,
) -> CaseOutcome {
    let Some(manifest) = registry.parsed_manifest(name) else {
        return CaseOutcome {
            id: "manifest_matches_host",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!("no manifest registered for plugin {name}"),
            },
        };
    };
    let result = manifest.verify_compatible_with(name, manifest.contract(), host_version);
    let status = match result {
        Ok(()) => CaseStatus::Ok,
        Err(PluginError::ContractMismatch { expected, actual }) => CaseStatus::Failed {
            message: format!("manifest contract {actual:?} does not match host {expected:?}"),
        },
        Err(PluginError::ManifestNameMismatch { expected, manifest }) => CaseStatus::Failed {
            message: format!("manifest name {manifest} does not match registered name {expected}"),
        },
        Err(PluginError::UnsupportedContractVersion {
            host, plugin_range, ..
        }) => CaseStatus::Failed {
            message: format!(
                "host CONTRACT_VERSION {host} is outside manifest range {plugin_range:?}"
            ),
        },
        Err(other) => CaseStatus::Failed {
            message: format!("verify_compatible_with failed: {other}"),
        },
    };
    CaseOutcome {
        id: "manifest_matches_host",
        tier: Tier::One,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_as_u8() {
        assert_eq!(Tier::One.as_u8(), 1);
        assert_eq!(Tier::Two.as_u8(), 2);
    }

    #[test]
    fn case_status_str() {
        assert_eq!(CaseStatus::Ok.as_str(), "ok");
        assert_eq!(CaseStatus::Pending { reason: "x" }.as_str(), "pending");
        assert_eq!(
            CaseStatus::Failed {
                message: "x".to_string()
            }
            .as_str(),
            "failed"
        );
    }

    #[test]
    fn run_for_unregistered_plugin_is_empty() {
        let reg = PluginRegistry::new();
        let name = PluginName::new("does-not-exist").expect("valid");
        assert!(run_conformance_for_plugin(&reg, &name).is_empty());
    }

    #[test]
    fn tier1_manifest_matches_host_returns_failed_when_no_manifest() {
        use crate::contract::version::ContractVersion;

        let reg = PluginRegistry::new();
        let name = PluginName::new("missing-plugin").expect("valid");
        let outcome = tier1_manifest_matches_host(&reg, &name, ContractVersion::new(0, 1, 0));
        assert_eq!(outcome.id, "manifest_matches_host");
        assert_eq!(outcome.tier, Tier::One);
        let CaseStatus::Failed { message } = &outcome.status else {
            panic!("expected Failed status, got {:?}", outcome.status);
        };
        assert!(
            message.contains("missing-plugin"),
            "message should mention plugin name: {message}"
        );
    }
}
