//! `SensorIngress` contract (brief §4 row 4).
//!
//! P0: hook sensors only (#84). IDE/clipboard/screen/web are P1; Slack/
//! email/GitHub are P2.

use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `SensorIngress`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

/// Static capability declaration for a `SensorIngress` impl.
// Three flags cover distinct sensor delivery dimensions; a state machine
// adds indirection with no clarity gain here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SensorIngressCapabilities {
    /// Whether the sensor supports batched event delivery.
    pub batches: bool,
    /// Whether the sensor supports continuous streaming delivery.
    pub streaming: bool,
    /// Whether the sensor checks consent before forwarding events.
    pub consent_aware: bool,
}

/// Sensor ingress contract — event delivery into the Cairn pipeline.
///
/// Brief §4 row 4: P0 is hook sensors only (#84). IDE/clipboard/screen/web
/// are P1; Slack/email/GitHub are P2. All share this trait.
#[async_trait::async_trait]
pub trait SensorIngress: Send + Sync {
    /// Stable identifier of the registered plugin instance.
    fn name(&self) -> &str;

    /// Static capability advertisement (brief §4.1).
    fn capabilities(&self) -> &SensorIngressCapabilities;

    /// Range of `SensorIngress::CONTRACT_VERSION` values this impl accepts.
    fn supported_contract_versions(&self) -> VersionRange;
}

/// Static identity descriptor for a [`SensorIngress`] plugin (§4.1).
///
/// Carries the two associated consts the `register_plugin_with!` macro checks
/// before construction. See [`MemoryStorePlugin`](crate::contract::MemoryStorePlugin)
/// for the design rationale.
pub trait SensorIngressPlugin: SensorIngress + Sized {
    /// Stable plugin name, checked statically before construction (§4.1).
    const NAME: &'static str;
    /// Version range checked statically before construction (§4.1).
    const SUPPORTED_VERSIONS: VersionRange;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubSensor;

    #[async_trait::async_trait]
    impl SensorIngress for StubSensor {
        fn name(&self) -> &'static str {
            Self::NAME
        }
        fn capabilities(&self) -> &SensorIngressCapabilities {
            static CAPS: SensorIngressCapabilities = SensorIngressCapabilities {
                batches: true,
                streaming: false,
                consent_aware: true,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
    }

    impl SensorIngressPlugin for StubSensor {
        const NAME: &'static str = "stub-sensor";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    #[test]
    fn dyn_compatible() {
        let s: Box<dyn SensorIngress> = Box::new(StubSensor);
        assert_eq!(s.name(), "stub-sensor");
        assert!(s.supported_contract_versions().accepts(CONTRACT_VERSION));
    }

    #[test]
    fn static_consts_accessible() {
        assert_eq!(StubSensor::NAME, "stub-sensor");
        assert!(StubSensor::SUPPORTED_VERSIONS.accepts(CONTRACT_VERSION));
    }
}
