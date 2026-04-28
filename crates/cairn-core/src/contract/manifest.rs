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
/// names from §4.1 exactly (`LLMProvider`, `MCPServer`, etc.).
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
    /// Implements `MCPServer` contract.
    MCPServer,
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
            ContractKind::MCPServer => "MCPServer",
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
///
/// Fields are private to ensure that every `PluginManifest` value has passed
/// `parse_toml`'s semantic validation (range ordering, feature-key grammar,
/// name pattern). Use the accessor methods to read individual fields.
#[derive(Debug, Clone)]
pub struct PluginManifest {
    name: PluginName,
    contract: ContractKind,
    contract_version_range: VersionRange,
    features: BTreeMap<String, bool>,
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
    /// Stable plugin identifier as parsed from the manifest.
    #[must_use]
    pub fn name(&self) -> &PluginName {
        &self.name
    }

    /// Contract kind this plugin implements.
    #[must_use]
    pub fn contract(&self) -> ContractKind {
        self.contract
    }

    /// Half-open version range `[min, max_exclusive)` of host `CONTRACT_VERSION`
    /// values this plugin accepts.
    #[must_use]
    pub fn contract_version_range(&self) -> VersionRange {
        self.contract_version_range
    }

    /// Free-form boolean feature flags this plugin advertises (may be empty).
    #[must_use]
    pub fn features(&self) -> &BTreeMap<String, bool> {
        &self.features
    }

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

    /// Verify the manifest declares `expected_name`, the `expected_kind` contract,
    /// and that `contract_version_range` accepts the supplied host `host_version`.
    ///
    /// Hosts call this before invoking a plugin's `register(&mut PluginRegistry)`
    /// to fail closed on name, contract-kind, and version mismatches — independent
    /// of the runtime check the registry itself performs.
    ///
    /// # Errors
    /// - [`PluginError::ManifestNameMismatch`] when `self.name != expected_name`.
    /// - [`PluginError::ContractMismatch`] when `self.contract != expected_kind`.
    /// - [`PluginError::UnsupportedContractVersion`] when `host_version` is
    ///   outside `self.contract_version_range`.
    pub fn verify_compatible_with(
        &self,
        expected_name: &PluginName,
        expected_kind: ContractKind,
        host_version: ContractVersion,
    ) -> Result<(), PluginError> {
        if &self.name != expected_name {
            return Err(PluginError::ManifestNameMismatch {
                expected: expected_name.clone(),
                manifest: self.name.clone(),
            });
        }
        if self.contract != expected_kind {
            return Err(PluginError::ContractMismatch {
                expected: expected_kind,
                actual: self.contract,
            });
        }
        if !self.contract_version_range.accepts(host_version) {
            return Err(PluginError::UnsupportedContractVersion {
                contract: expected_kind.as_static_str(),
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

    // Inlined from `crates/cairn-idl/schema/plugin/example.toml` so that
    // `cargo package -p cairn-core` does not pull in a sibling crate path that
    // is absent from the published package.  The cairn-idl-side file is kept
    // intact for the JSON-Schema validation test in
    // `crates/cairn-idl/tests/plugin_manifest_validation.rs`.
    const FIXTURE: &str = r#"
name = "cairn-store-sqlite"
contract = "MemoryStore"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 3
patch = 0

[features]
fts = true
vector = false
graph_edges = false
"#;

    #[test]
    fn parses_example_fixture() {
        let m = PluginManifest::parse_toml(FIXTURE).expect("fixture is valid");
        assert_eq!(m.name.as_str(), "cairn-store-sqlite");
        assert_eq!(m.contract, ContractKind::MemoryStore);
        assert_eq!(m.contract_version_range.min, ContractVersion::new(0, 1, 0));
        assert_eq!(
            m.contract_version_range.max_exclusive,
            ContractVersion::new(0, 3, 0)
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
        let expected = PluginName::new("cairn-store-sqlite").expect("valid");
        assert!(
            m.verify_compatible_with(
                &expected,
                ContractKind::MemoryStore,
                ContractVersion::new(0, 1, 0)
            )
            .is_ok()
        );
        assert!(
            m.verify_compatible_with(
                &expected,
                ContractKind::MemoryStore,
                ContractVersion::new(0, 1, 9)
            )
            .is_ok()
        );
    }

    #[test]
    fn verify_compatible_with_rejects_host_outside_range() {
        let m = PluginManifest::parse_toml(FIXTURE).expect("fixture is valid");
        let expected = PluginName::new("cairn-store-sqlite").expect("valid");
        let err = m
            .verify_compatible_with(
                &expected,
                ContractKind::MemoryStore,
                ContractVersion::new(0, 3, 0),
            )
            .expect_err("host outside range must fail");
        match err {
            PluginError::UnsupportedContractVersion { contract, host, .. } => {
                assert_eq!(contract, "MemoryStore");
                assert_eq!(host, ContractVersion::new(0, 3, 0));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn verify_compatible_with_rejects_contract_mismatch() {
        let m = PluginManifest::parse_toml(FIXTURE).expect("fixture is valid");
        let expected = PluginName::new("cairn-store-sqlite").expect("valid");
        // Fixture is a MemoryStore manifest; verify against LLMProvider must fail.
        let err = m
            .verify_compatible_with(
                &expected,
                ContractKind::LLMProvider,
                ContractVersion::new(0, 1, 0),
            )
            .expect_err("contract kind mismatch must fail");
        match err {
            PluginError::ContractMismatch { expected, actual } => {
                assert_eq!(expected, ContractKind::LLMProvider);
                assert_eq!(actual, ContractKind::MemoryStore);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn verify_compatible_with_rejects_name_mismatch() {
        let m = PluginManifest::parse_toml(FIXTURE).expect("fixture is valid");
        let wrong = PluginName::new("acme-other-name").expect("valid");
        let err = m
            .verify_compatible_with(
                &wrong,
                ContractKind::MemoryStore,
                ContractVersion::new(0, 1, 0),
            )
            .expect_err("manifest name mismatch must fail");
        match err {
            PluginError::ManifestNameMismatch { expected, manifest } => {
                assert_eq!(expected.as_str(), "acme-other-name");
                assert_eq!(manifest.as_str(), "cairn-store-sqlite");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn accessors_expose_parsed_values() {
        let m = PluginManifest::parse_toml(FIXTURE).expect("fixture is valid");
        assert_eq!(m.name().as_str(), "cairn-store-sqlite");
        assert_eq!(m.contract(), ContractKind::MemoryStore);
        assert_eq!(
            m.contract_version_range(),
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 3, 0),)
        );
        assert_eq!(m.features().get("fts"), Some(&true));
        assert_eq!(m.features().len(), 3);
    }
}
