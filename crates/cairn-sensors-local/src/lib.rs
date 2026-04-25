//! Local sensors for Cairn — IDE hook, terminal, clipboard, voice, screen.
//!
//! P0 scaffold: stub `SensorIngress` impl with all capability flags
//! `false`. Real capture lands per-sensor in #84 and follow-ups.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use cairn_core::contract::sensor_ingress::{
    CONTRACT_VERSION, SensorIngress, SensorIngressCapabilities,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-sensors-local";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Accepted host contract version range. Single source of truth for both the
/// trait impl's `supported_contract_versions()` and the const-eval guard.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

/// P0 stub `SensorIngress`. All capability flags are `false`.
#[derive(Default)]
pub struct LocalSensorIngress;

#[async_trait::async_trait]
impl SensorIngress for LocalSensorIngress {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &SensorIngressCapabilities {
        static CAPS: SensorIngressCapabilities = SensorIngressCapabilities {
            batches: false,
            streaming: false,
            consent_aware: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        ACCEPTED_RANGE
    }
}

// Compile-time guard: this crate's accepted range must include the host
// CONTRACT_VERSION. If we ever bump CONTRACT_VERSION without bumping
// ACCEPTED_RANGE, this assertion fires at build time.
const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

register_plugin!(
    SensorIngress,
    LocalSensorIngress,
    "cairn-sensors-local",
    MANIFEST_TOML
);
