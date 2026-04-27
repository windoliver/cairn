//! Integration: a fake plugin registers via `register_plugin!`, the host
//! looks it up, and version mismatch fails closed.

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities, MemoryStorePlugin,
};
use cairn_core::contract::registry::{PluginError, PluginName, PluginRegistry};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;
use cairn_core::register_plugin_with;

mod compatible_plugin {
    use super::*;

    #[derive(Default)]
    pub struct FakeStore;

    #[async_trait::async_trait]
    impl MemoryStore for FakeStore {
        fn name(&self) -> &'static str {
            "fake-compat"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: true,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }
    }

    impl MemoryStorePlugin for FakeStore {
        const NAME: &'static str = "fake-compat";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    register_plugin!(MemoryStore, FakeStore, "fake-compat");
}

mod future_plugin {
    use super::*;

    #[derive(Default)]
    pub struct FutureStore;

    #[async_trait::async_trait]
    impl MemoryStore for FutureStore {
        fn name(&self) -> &'static str {
            "fake-future"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            // Demands a future host version — must be rejected today.
            VersionRange::new(
                ContractVersion::new(9, 9, 0),
                ContractVersion::new(10, 0, 0),
            )
        }
    }

    impl MemoryStorePlugin for FutureStore {
        const NAME: &'static str = "fake-future";
        const SUPPORTED_VERSIONS: VersionRange = VersionRange::new(
            ContractVersion::new(9, 9, 0),
            ContractVersion::new(10, 0, 0),
        );
    }

    register_plugin!(MemoryStore, FutureStore, "fake-future");
}

#[test]
fn compatible_plugin_registers_via_macro() {
    let mut reg = PluginRegistry::new();
    compatible_plugin::register(&mut reg).expect("compatible plugin registers");

    let name = PluginName::new("fake-compat").expect("valid name");
    let resolved = reg.memory_store(&name).expect("registered");
    assert_eq!(resolved.name(), "fake-compat");
    assert!(
        resolved
            .supported_contract_versions()
            .accepts(CONTRACT_VERSION)
    );
}

#[test]
fn incompatible_plugin_fails_closed() {
    let mut reg = PluginRegistry::new();
    let err =
        future_plugin::register(&mut reg).expect_err("plugin demanding future host must fail");
    match err {
        PluginError::UnsupportedContractVersion { contract, host, .. } => {
            assert_eq!(contract, "MemoryStore");
            assert_eq!(host, CONTRACT_VERSION);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn keeps_arc_pointer_stable() {
    // Two lookups return the same underlying Arc.
    let mut reg = PluginRegistry::new();
    compatible_plugin::register(&mut reg).expect("registers");
    let name = PluginName::new("fake-compat").expect("valid");
    let a = reg.memory_store(&name).expect("registered");
    let b = reg.memory_store(&name).expect("registered");
    assert!(Arc::ptr_eq(&a, &b));
}

mod manifest_aware_plugin {
    use super::*;

    pub const MANIFEST_TOML: &str = r#"
name = "fake-with-manifest"
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

    #[derive(Default)]
    pub struct FakeStore;

    #[async_trait::async_trait]
    impl MemoryStore for FakeStore {
        fn name(&self) -> &'static str {
            "fake-with-manifest"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }
    }

    impl MemoryStorePlugin for FakeStore {
        const NAME: &'static str = "fake-with-manifest";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    register_plugin!(MemoryStore, FakeStore, "fake-with-manifest", MANIFEST_TOML);
}

#[test]
fn manifest_aware_macro_registers_with_manifest() {
    let mut reg = PluginRegistry::new();
    manifest_aware_plugin::register(&mut reg).expect("manifest-aware register succeeds");

    let name = PluginName::new("fake-with-manifest").expect("valid");
    assert!(reg.memory_store(&name).is_some(), "trait registered");
    assert!(reg.parsed_manifest(&name).is_some(), "manifest registered");
    assert_eq!(
        reg.parsed_manifest(&name).unwrap().contract(),
        cairn_core::contract::manifest::ContractKind::MemoryStore
    );
}

mod incompatible_factory_plugin {
    use super::*;

    pub struct NeverBuilt;

    #[async_trait::async_trait]
    impl MemoryStore for NeverBuilt {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
    }

    impl MemoryStorePlugin for NeverBuilt {
        const NAME: &'static str = "never-built";
        const SUPPORTED_VERSIONS: VersionRange = VersionRange::new(
            ContractVersion::new(9, 9, 0),
            ContractVersion::new(10, 0, 0),
        );
    }

    register_plugin_with!(
        MemoryStore,
        NeverBuilt,
        "never-built",
        |_cfg: &cairn_core::config::CairnConfig| -> Result<NeverBuilt, std::convert::Infallible> {
            panic!("factory must NOT be called for incompatible plugin versions")
        }
    );
}

mod config_driven_plugin {
    use super::*;

    pub struct PathStore;

    #[async_trait::async_trait]
    impl MemoryStore for PathStore {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
    }

    impl MemoryStorePlugin for PathStore {
        const NAME: &'static str = "path-store";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    register_plugin_with!(
        MemoryStore,
        PathStore,
        "path-store",
        |_cfg: &cairn_core::config::CairnConfig| { Ok::<_, std::convert::Infallible>(PathStore) }
    );
}

mod name_mismatch_plugin {
    use super::*;

    #[derive(Default)]
    pub struct BadNameStore;

    #[async_trait::async_trait]
    impl MemoryStore for BadNameStore {
        fn name(&self) -> &str {
            Self::NAME
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            Self::SUPPORTED_VERSIONS
        }
    }

    impl MemoryStorePlugin for BadNameStore {
        const NAME: &'static str = "actual-name";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    // NAME const = "actual-name" but macro literal = "wrong-name"
    register_plugin!(MemoryStore, BadNameStore, "wrong-name");
}

#[test]
fn factory_not_called_on_version_mismatch() {
    let mut reg = PluginRegistry::new();
    let cfg = CairnConfig::default();
    let err = incompatible_factory_plugin::register(&mut reg, &cfg)
        .expect_err("incompatible plugin must fail closed");
    match err {
        PluginError::UnsupportedContractVersion { contract, host, .. } => {
            assert_eq!(contract, "MemoryStore");
            assert_eq!(host, CONTRACT_VERSION);
        }
        other => panic!("expected UnsupportedContractVersion, got {other:?}"),
    }
    // If we reach here the panicking factory was never called — test passes.
}

#[test]
fn config_driven_plugin_registers_and_resolves() {
    let mut reg = PluginRegistry::new();
    let cfg = CairnConfig::default();
    config_driven_plugin::register(&mut reg, &cfg).expect("config-driven plugin registers");
    let name = PluginName::new("path-store").expect("valid");
    let plugin = reg.memory_store(&name).expect("registered");
    assert_eq!(plugin.name(), "path-store");
}

#[test]
fn register_plugin_macro_rejects_name_const_mismatch() {
    // Verifies the new static identity pre-check in register_plugin! fires
    // before Default::default() is called.
    let mut reg = PluginRegistry::new();
    let err =
        name_mismatch_plugin::register(&mut reg).expect_err("NAME/literal mismatch must fail");
    assert!(
        matches!(err, PluginError::IdentityMismatch { .. }),
        "expected IdentityMismatch, got {err:?}"
    );
}
