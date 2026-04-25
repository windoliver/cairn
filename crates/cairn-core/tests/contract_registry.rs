//! Integration: a fake plugin registers via `register_plugin!`, the host
//! looks it up, and version mismatch fails closed.

use std::sync::Arc;

use cairn_core::contract::memory_store::{
    MemoryStore, MemoryStoreCapabilities, CONTRACT_VERSION,
};
use cairn_core::contract::registry::{PluginError, PluginName, PluginRegistry};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

mod compatible_plugin {
    use super::*;

    #[derive(Default)]
    pub struct FakeStore;

    #[async_trait::async_trait]
    impl MemoryStore for FakeStore {
        fn name(&self) -> &'static str {
            "fake-compatible"
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
            VersionRange::new(
                ContractVersion::new(0, 1, 0),
                ContractVersion::new(0, 2, 0),
            )
        }
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

    register_plugin!(MemoryStore, FutureStore, "fake-future");
}

#[test]
fn compatible_plugin_registers_via_macro() {
    let mut reg = PluginRegistry::new();
    compatible_plugin::register(&mut reg).expect("compatible plugin registers");

    let name = PluginName::new("fake-compat").expect("valid name");
    let resolved = reg.memory_store(&name).expect("registered");
    assert_eq!(resolved.name(), "fake-compatible");
    assert!(resolved.supported_contract_versions().accepts(CONTRACT_VERSION));
}

#[test]
fn incompatible_plugin_fails_closed() {
    let mut reg = PluginRegistry::new();
    let err = future_plugin::register(&mut reg)
        .expect_err("plugin demanding future host must fail");
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
