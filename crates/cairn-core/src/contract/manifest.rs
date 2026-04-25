//! Plugin capability manifest (TOML), brief §4.1.
//!
//! The schema lives in `crates/cairn-idl/schema/plugin/manifest.json`; this
//! module is the in-process parser that hosts use to validate a manifest
//! before invoking the plugin's `register(&mut PluginRegistry)`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contract::registry::{PluginError, PluginName};
use crate::contract::version::VersionRange;

/// Contract enum mirroring `cairn-idl/schema/plugin/manifest.json#contract`.
///
/// `#[allow(clippy::upper_case_acronyms)]` — variant names match the trait
/// names from §4.1 exactly (`LLMProvider`, `McpServer`, etc.).
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ContractKind {
    /// Implements `MemoryStore` contract.
    MemoryStore,
    /// Implements `LLMProvider` contract.
    LLMProvider,
    /// Implements `WorkflowOrchestrator` contract.
    WorkflowOrchestrator,
    /// Implements `SensorIngress` contract.
    SensorIngress,
    /// Implements `McpServer` contract.
    McpServer,
    /// Implements `FrontendAdapter` contract.
    FrontendAdapter,
    /// Implements `AgentProvider` contract.
    AgentProvider,
}

/// Wire form: matches the TOML manifest exactly. `name` is parsed as a
/// `String` here and validated via `PluginName::new` on the way out.
#[derive(Debug, Clone, Deserialize)]
struct PluginManifestWire {
    name: String,
    contract: ContractKind,
    contract_version_range: VersionRange,
    #[serde(default)]
    features: BTreeMap<String, bool>,
}

/// Validated, in-memory plugin capability manifest.
///
/// Produced by [`PluginManifest::parse_toml`] from a TOML source string.
/// Hosts use this to gate activation before calling `register`.
#[derive(Debug, Clone)]
pub struct PluginManifest {
    /// Stable plugin identifier, validated by [`PluginName`] rules.
    pub name: PluginName,
    /// Which contract this plugin implements.
    pub contract: ContractKind,
    /// Half-open version range `[min, max_exclusive)` this plugin supports.
    pub contract_version_range: VersionRange,
    /// Boolean feature flags this plugin advertises (may be empty).
    pub features: BTreeMap<String, bool>,
}

impl PluginManifest {
    /// Parse a manifest from its TOML source.
    ///
    /// # Errors
    /// [`PluginError::InvalidManifest`] for syntactic errors;
    /// [`PluginError::InvalidName`] when `name` violates `PluginName` rules.
    pub fn parse_toml(source: &str) -> Result<Self, PluginError> {
        let wire: PluginManifestWire =
            toml::from_str(source).map_err(|e| PluginError::InvalidManifest(e.to_string()))?;
        Ok(Self {
            name: PluginName::new(wire.name)?,
            contract: wire.contract,
            contract_version_range: wire.contract_version_range,
            features: wire.features,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::version::ContractVersion;

    const FIXTURE: &str = include_str!("../../../cairn-idl/schema/plugin/example.toml");

    #[test]
    fn parses_example_fixture() {
        let m = PluginManifest::parse_toml(FIXTURE).expect("fixture is valid");
        assert_eq!(m.name.as_str(), "cairn-store-sqlite");
        assert_eq!(m.contract, ContractKind::MemoryStore);
        assert_eq!(m.contract_version_range.min, ContractVersion::new(0, 1, 0));
        assert_eq!(
            m.contract_version_range.max_exclusive,
            ContractVersion::new(0, 2, 0)
        );
        assert_eq!(m.features.get("fts"), Some(&true));
    }

    #[test]
    fn rejects_invalid_name() {
        let bad = r#"
            name = "BAD_NAME"
            contract = "MemoryStore"
            [contract_version_range.min]
            major = 0
            minor = 1
            patch = 0
            [contract_version_range.max_exclusive]
            major = 0
            minor = 2
            patch = 0
        "#;
        assert!(matches!(
            PluginManifest::parse_toml(bad),
            Err(PluginError::InvalidName(_))
        ));
    }

    #[test]
    fn rejects_unknown_contract() {
        let bad = r#"
            name = "good-name"
            contract = "Unknown"
            [contract_version_range.min]
            major = 0
            minor = 1
            patch = 0
            [contract_version_range.max_exclusive]
            major = 0
            minor = 2
            patch = 0
        "#;
        assert!(matches!(
            PluginManifest::parse_toml(bad),
            Err(PluginError::InvalidManifest(_))
        ));
    }

    #[test]
    fn defaults_features_to_empty() {
        let minimal = r#"
            name = "good-name"
            contract = "LLMProvider"
            [contract_version_range.min]
            major = 0
            minor = 1
            patch = 0
            [contract_version_range.max_exclusive]
            major = 0
            minor = 2
            patch = 0
        "#;
        let m = PluginManifest::parse_toml(minimal).expect("valid");
        assert!(m.features.is_empty());
    }
}
