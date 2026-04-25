//! Plugin capability manifest (TOML), brief §4.1.
//!
//! The schema lives in `crates/cairn-idl/schema/plugin/manifest.json`; this
//! module is the in-process parser that hosts use to validate a manifest
//! before invoking the plugin's `register(&mut PluginRegistry)`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contract::registry::{PluginError, PluginName};
use crate::contract::version::{ContractVersion, VersionRange};

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

impl ContractKind {
    /// Stable `&'static str` identifier matching the literal used in
    /// [`PluginError::UnsupportedContractVersion::contract`] for the same kind.
    ///
    /// These strings must remain in sync with the `$contract` literals in the
    /// `register_method!` macro in `registry.rs` — they serve as the shared
    /// discriminator in error messages and downstream consumers.
    #[must_use]
    pub fn as_static_str(self) -> &'static str {
        match self {
            ContractKind::MemoryStore => "MemoryStore",
            ContractKind::LLMProvider => "LLMProvider",
            ContractKind::WorkflowOrchestrator => "WorkflowOrchestrator",
            ContractKind::SensorIngress => "SensorIngress",
            ContractKind::McpServer => "McpServer",
            ContractKind::FrontendAdapter => "FrontendAdapter",
            ContractKind::AgentProvider => "AgentProvider",
        }
    }
}

/// Wire form: matches the TOML manifest exactly. `name` is parsed as a
/// `String` here and validated via `PluginName::new` on the way out.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// Returns `true` when `range.min < range.max_exclusive` (strictly ordered).
///
/// JSON Schema cannot express cross-property comparison, so this check lives
/// exclusively in the Rust parser. See the `contract_version_range` description
/// in `manifest.json` for the documented gap.
fn is_strictly_ordered(range: &VersionRange) -> bool {
    let lo = (range.min.major, range.min.minor, range.min.patch);
    let hi = (
        range.max_exclusive.major,
        range.max_exclusive.minor,
        range.max_exclusive.patch,
    );
    lo < hi
}

/// Returns `true` when `key` satisfies the feature-key grammar:
/// non-empty, at most 64 chars, all chars ASCII alphanumeric or `_`.
///
/// This mirrors the `^[A-Za-z0-9_]{1,64}$` pattern in `manifest.json`
/// `propertyNames`.
fn is_valid_feature_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 64 && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl PluginManifest {
    /// Parse a manifest from its TOML source.
    ///
    /// # Errors
    /// - [`PluginError::InvalidManifest`] for syntactic errors, an inverted/empty
    ///   `contract_version_range`, or an invalid feature key.
    /// - [`PluginError::InvalidName`] when `name` violates `PluginName` rules.
    pub fn parse_toml(source: &str) -> Result<Self, PluginError> {
        let wire: PluginManifestWire =
            toml::from_str(source).map_err(|e| PluginError::InvalidManifest(e.to_string()))?;

        // Semantic validation: range must be non-empty (min < max_exclusive).
        // JSON Schema cannot enforce this cross-property invariant; the Rust
        // parser is the gatekeeper.
        if !is_strictly_ordered(&wire.contract_version_range) {
            return Err(PluginError::InvalidManifest(format!(
                "contract_version_range is empty or inverted: \
                 min {} must be strictly less than max_exclusive {}",
                wire.contract_version_range.min, wire.contract_version_range.max_exclusive,
            )));
        }

        // Validate every feature key: ^[A-Za-z0-9_]{1,64}$
        for key in wire.features.keys() {
            if !is_valid_feature_key(key) {
                return Err(PluginError::InvalidManifest(format!(
                    "feature key {key:?} is invalid: \
                     must be 1..=64 ASCII alnum + `_`",
                )));
            }
        }

        Ok(Self {
            name: PluginName::new(wire.name)?,
            contract: wire.contract,
            contract_version_range: wire.contract_version_range,
            features: wire.features,
        })
    }

    /// Verify the manifest declares `expected` contract and that
    /// `contract_version_range` accepts the supplied host `CONTRACT_VERSION`.
    ///
    /// Hosts call this before invoking a plugin's `register(&mut PluginRegistry)`
    /// to fail closed on both contract-kind and version mismatches — independent
    /// of the runtime check the registry itself performs.
    ///
    /// # Errors
    /// - [`PluginError::ContractMismatch`] when `self.contract != expected`.
    /// - [`PluginError::UnsupportedContractVersion`] when `host_version` is
    ///   outside `self.contract_version_range`.
    pub fn verify_compatible_with(
        &self,
        expected: ContractKind,
        host_version: ContractVersion,
    ) -> Result<(), PluginError> {
        if self.contract != expected {
            return Err(PluginError::ContractMismatch {
                expected,
                actual: self.contract,
            });
        }
        if !self.contract_version_range.accepts(host_version) {
            return Err(PluginError::UnsupportedContractVersion {
                contract: expected.as_static_str(),
                plugin: self.name.clone(),
                plugin_range: self.contract_version_range,
                host: host_version,
            });
        }
        Ok(())
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

    #[test]
    fn rejects_unknown_top_level_field() {
        let bad = r#"
            name = "good-name"
            contract = "MemoryStore"
            rogue_field = "boom"
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
    fn rejects_unknown_range_field() {
        let bad = r#"
            name = "good-name"
            contract = "MemoryStore"
            [contract_version_range]
            rogue = "boom"
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
    fn rejects_unknown_version_field() {
        let bad = r#"
            name = "good-name"
            contract = "MemoryStore"
            [contract_version_range.min]
            major = 0
            minor = 1
            patch = 0
            rogue = 99
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

    // -- Finding 1: semantic range / feature-key validation ------------------

    #[test]
    fn rejects_empty_version_range() {
        let bad = r#"
            name = "good-name"
            contract = "MemoryStore"
            [contract_version_range.min]
            major = 0
            minor = 1
            patch = 0
            [contract_version_range.max_exclusive]
            major = 0
            minor = 1
            patch = 0
        "#;
        assert!(matches!(
            PluginManifest::parse_toml(bad),
            Err(PluginError::InvalidManifest(_))
        ));
    }

    #[test]
    fn rejects_inverted_version_range() {
        let bad = r#"
            name = "good-name"
            contract = "MemoryStore"
            [contract_version_range.min]
            major = 1
            minor = 0
            patch = 0
            [contract_version_range.max_exclusive]
            major = 0
            minor = 1
            patch = 0
        "#;
        assert!(matches!(
            PluginManifest::parse_toml(bad),
            Err(PluginError::InvalidManifest(_))
        ));
    }

    #[test]
    fn rejects_invalid_feature_key_with_dot() {
        let bad = r#"
            name = "good-name"
            contract = "MemoryStore"
            [contract_version_range.min]
            major = 0
            minor = 1
            patch = 0
            [contract_version_range.max_exclusive]
            major = 0
            minor = 2
            patch = 0
            [features]
            "bad.key" = true
        "#;
        assert!(matches!(
            PluginManifest::parse_toml(bad),
            Err(PluginError::InvalidManifest(_))
        ));
    }

    // Note: TOML does not permit empty string as a bare key, so the
    // `"" = true` case cannot be represented in TOML syntax. The
    // `is_valid_feature_key` function still guards against it for callers
    // that construct the BTreeMap directly. The dot-key test above covers
    // the parser-accessible invalid-key path.

    // -- Finding 2: verify_compatible_with -----------------------------------

    #[test]
    fn verify_compatible_with_accepts_host_in_range() {
        let m = PluginManifest::parse_toml(FIXTURE).expect("fixture is valid");
        assert!(
            m.verify_compatible_with(ContractKind::MemoryStore, ContractVersion::new(0, 1, 0))
                .is_ok()
        );
        assert!(
            m.verify_compatible_with(ContractKind::MemoryStore, ContractVersion::new(0, 1, 9))
                .is_ok()
        );
    }

    #[test]
    fn verify_compatible_with_rejects_host_outside_range() {
        let m = PluginManifest::parse_toml(FIXTURE).expect("fixture is valid");
        let err = m
            .verify_compatible_with(ContractKind::MemoryStore, ContractVersion::new(0, 2, 0))
            .expect_err("host outside range must fail");
        match err {
            PluginError::UnsupportedContractVersion { contract, host, .. } => {
                assert_eq!(contract, "MemoryStore");
                assert_eq!(host, ContractVersion::new(0, 2, 0));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn verify_compatible_with_rejects_contract_mismatch() {
        let m = PluginManifest::parse_toml(FIXTURE).expect("fixture is valid");
        // Fixture is a MemoryStore manifest; verify against LLMProvider must fail.
        let err = m
            .verify_compatible_with(ContractKind::LLMProvider, ContractVersion::new(0, 1, 0))
            .expect_err("contract kind mismatch must fail");
        match err {
            PluginError::ContractMismatch { expected, actual } => {
                assert_eq!(expected, ContractKind::LLMProvider);
                assert_eq!(actual, ContractKind::MemoryStore);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
