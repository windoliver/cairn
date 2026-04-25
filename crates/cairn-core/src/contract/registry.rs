//! Plugin registry: typed, in-memory, host-assembled at startup.
//!
//! Brief §4.1: registration is explicit. Hosts call each plugin crate's
//! `register(&mut PluginRegistry)` (emitted by `register_plugin!`) in a
//! deterministic order, then assemble the active set from config.

use std::collections::HashMap;
use std::sync::Arc;

use crate::contract::version::{ContractVersion, VersionRange};

/// Stable identifier for a plugin instance. Lowercase ASCII alnum + `-`
/// (matches crates.io naming), 3..=64 chars. Examples: `cairn-store-sqlite`,
/// `cairn-llm-openai-compat`, `acme-store-qdrant`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginName(String);

impl PluginName {
    /// Construct a `PluginName`, validating shape.
    ///
    /// # Errors
    /// [`PluginError::InvalidName`] when `raw` violates the naming rule.
    pub fn new(raw: impl Into<String>) -> Result<Self, PluginError> {
        let raw = raw.into();
        let valid = raw.len() >= 3
            && raw.len() <= 64
            && raw
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !raw.starts_with('-')
            && !raw.ends_with('-');
        if valid {
            Ok(Self(raw))
        } else {
            Err(PluginError::InvalidName(raw))
        }
    }

    /// Returns the plugin name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PluginName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Errors produced by the plugin registry.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginError {
    /// The supplied string is not a valid plugin name.
    #[error("invalid plugin name: {0:?}")]
    InvalidName(String),

    /// A plugin with this name was already registered for this contract.
    #[error("duplicate plugin name {name} for contract {contract}")]
    DuplicateName {
        /// The contract slot that already holds this name.
        contract: &'static str,
        /// The conflicting plugin name.
        name: PluginName,
    },

    /// The plugin's accepted version range does not include the host's contract version.
    #[error(
        "plugin {plugin} for contract {contract} accepts {plugin_range:?} \
         but host is {host}"
    )]
    UnsupportedContractVersion {
        /// The contract whose version is mismatched.
        contract: &'static str,
        /// The plugin that declared incompatible version support.
        plugin: PluginName,
        /// The version range the plugin declared it supports.
        plugin_range: VersionRange,
        /// The host's current contract version.
        host: ContractVersion,
    },

    /// Plugin's runtime name does not match the registered identifier.
    #[error(
        "plugin runtime name {runtime:?} does not match registered key {registered} \
         for contract {contract}"
    )]
    IdentityMismatch {
        /// Contract under which registration was attempted.
        contract: &'static str,
        /// Name passed to the registry (the key).
        registered: PluginName,
        /// Name reported by the plugin at runtime via `name()`.
        runtime: String,
    },

    /// The plugin manifest contains invalid or missing fields.
    #[error("invalid plugin manifest: {0}")]
    InvalidManifest(String),

    /// Plugin manifest declares a different contract than the host expects.
    #[error("plugin manifest declares contract {actual:?} but host expected {expected:?}")]
    ContractMismatch {
        /// The contract kind the host was trying to verify against.
        expected: crate::contract::manifest::ContractKind,
        /// The contract kind the manifest actually declares.
        actual: crate::contract::manifest::ContractKind,
    },

    /// Plugin manifest declares a different name than the host expects.
    #[error("plugin manifest declares name {manifest:?} but host expected {expected:?}")]
    ManifestNameMismatch {
        /// The name the host was trying to verify against.
        expected: PluginName,
        /// The name the manifest actually declares.
        manifest: PluginName,
    },
}

use crate::contract::{
    agent_provider::AgentProvider, frontend_adapter::FrontendAdapter, llm_provider::LLMProvider,
    mcp_server::MCPServer, memory_store::MemoryStore, sensor_ingress::SensorIngress,
    workflow_orchestrator::WorkflowOrchestrator,
};

/// Active set of registered plugins, keyed per contract by `PluginName`.
///
/// Constructed empty by the host at startup, then populated by calling each
/// plugin crate's `register(&mut PluginRegistry)` (emitted by
/// `register_plugin!`). After all `register` calls, the host queries the
/// active impl per contract from `.cairn/config.yaml` (brief §4.1).
///
/// # Trust model
///
/// Plugins are **compile-time dependencies** of the host binary — every
/// `register_plugin!` call lives in a Cargo dependency the host author
/// explicitly added. The registry is therefore **not** a sandbox: a
/// plugin's generated `register` receives `&mut PluginRegistry` and can,
/// in principle, call any `register_*` method or register under any
/// `PluginName`. This is intentional. Rust crate trust is established at
/// the build, not at the registry mutation. A single plugin may also
/// register multiple contracts in one `register` call (e.g., a vendor
/// suite that ships both a `MemoryStore` and a `SensorIngress` impl).
///
/// For deployments that want a manifest-driven gate before invoking each
/// plugin's `register`, see [`super::manifest::PluginManifest::verify_compatible_with`].
/// That helper validates a manifest's name + contract kind + accepted
/// version range against the host before activation, but it is advisory:
/// the host still calls the plugin's `register` with a full `&mut
/// PluginRegistry`. Atomic / transactional registrars are out of P0 scope
/// and would conflict with multi-contract plugins; revisit if a future
/// threat model demands sandboxing (likely via WASM, not the registry).
#[derive(Default)]
pub struct PluginRegistry {
    memory_stores: HashMap<PluginName, Arc<dyn MemoryStore>>,
    llm_providers: HashMap<PluginName, Arc<dyn LLMProvider>>,
    workflow_orchestrators: HashMap<PluginName, Arc<dyn WorkflowOrchestrator>>,
    sensor_ingress: HashMap<PluginName, Arc<dyn SensorIngress>>,
    mcp_servers: HashMap<PluginName, Arc<dyn MCPServer>>,
    frontend_adapters: HashMap<PluginName, Arc<dyn FrontendAdapter>>,
    agent_providers: HashMap<PluginName, Arc<dyn AgentProvider>>,
}

impl std::fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRegistry")
            .field(
                "memory_stores",
                &self.memory_stores.keys().collect::<Vec<_>>(),
            )
            .field(
                "llm_providers",
                &self.llm_providers.keys().collect::<Vec<_>>(),
            )
            .field(
                "workflow_orchestrators",
                &self.workflow_orchestrators.keys().collect::<Vec<_>>(),
            )
            .field(
                "sensor_ingress",
                &self.sensor_ingress.keys().collect::<Vec<_>>(),
            )
            .field("mcp_servers", &self.mcp_servers.keys().collect::<Vec<_>>())
            .field(
                "frontend_adapters",
                &self.frontend_adapters.keys().collect::<Vec<_>>(),
            )
            .field(
                "agent_providers",
                &self.agent_providers.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

// Macro expansion needs absolute paths to avoid name collisions across contracts.
#[allow(unused_qualifications)]
macro_rules! register_method {
    ($method:ident, $field:ident, $trait_:path, $contract:literal, $host_version:expr) => {
        /// Register a plugin under the given contract.
        ///
        /// # Errors
        /// - [`PluginError::DuplicateName`] when `name` is already registered for this contract.
        /// - [`PluginError::UnsupportedContractVersion`] when the impl's
        ///   `supported_contract_versions()` does not accept the host's `CONTRACT_VERSION`.
        pub fn $method(
            &mut self,
            name: PluginName,
            plugin: Arc<dyn $trait_>,
        ) -> Result<(), PluginError> {
            if !plugin.supported_contract_versions().accepts($host_version) {
                return Err(PluginError::UnsupportedContractVersion {
                    contract: $contract,
                    plugin: name,
                    plugin_range: plugin.supported_contract_versions(),
                    host: $host_version,
                });
            }
            if plugin.name() != name.as_str() {
                return Err(PluginError::IdentityMismatch {
                    contract: $contract,
                    registered: name,
                    runtime: plugin.name().to_string(),
                });
            }
            if self.$field.contains_key(&name) {
                return Err(PluginError::DuplicateName {
                    contract: $contract,
                    name,
                });
            }
            self.$field.insert(name, plugin);
            Ok(())
        }
    };
}

impl PluginRegistry {
    /// Construct an empty `PluginRegistry`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    register_method!(
        register_memory_store,
        memory_stores,
        crate::contract::memory_store::MemoryStore,
        "MemoryStore",
        crate::contract::memory_store::CONTRACT_VERSION
    );
    register_method!(
        register_llm_provider,
        llm_providers,
        crate::contract::llm_provider::LLMProvider,
        "LLMProvider",
        crate::contract::llm_provider::CONTRACT_VERSION
    );
    register_method!(
        register_workflow_orchestrator,
        workflow_orchestrators,
        crate::contract::workflow_orchestrator::WorkflowOrchestrator,
        "WorkflowOrchestrator",
        crate::contract::workflow_orchestrator::CONTRACT_VERSION
    );
    register_method!(
        register_sensor_ingress,
        sensor_ingress,
        crate::contract::sensor_ingress::SensorIngress,
        "SensorIngress",
        crate::contract::sensor_ingress::CONTRACT_VERSION
    );
    register_method!(
        register_mcp_server,
        mcp_servers,
        crate::contract::mcp_server::MCPServer,
        "MCPServer",
        crate::contract::mcp_server::CONTRACT_VERSION
    );
    register_method!(
        register_frontend_adapter,
        frontend_adapters,
        crate::contract::frontend_adapter::FrontendAdapter,
        "FrontendAdapter",
        crate::contract::frontend_adapter::CONTRACT_VERSION
    );
    register_method!(
        register_agent_provider,
        agent_providers,
        crate::contract::agent_provider::AgentProvider,
        "AgentProvider",
        crate::contract::agent_provider::CONTRACT_VERSION
    );

    /// Look up a registered `MemoryStore` by plugin name.
    #[must_use]
    pub fn memory_store(&self, name: &PluginName) -> Option<Arc<dyn MemoryStore>> {
        self.memory_stores.get(name).cloned()
    }

    /// Look up a registered `LLMProvider` by plugin name.
    #[must_use]
    pub fn llm_provider(&self, name: &PluginName) -> Option<Arc<dyn LLMProvider>> {
        self.llm_providers.get(name).cloned()
    }

    /// Look up a registered `WorkflowOrchestrator` by plugin name.
    #[must_use]
    pub fn workflow_orchestrator(
        &self,
        name: &PluginName,
    ) -> Option<Arc<dyn WorkflowOrchestrator>> {
        self.workflow_orchestrators.get(name).cloned()
    }

    /// Look up a registered `SensorIngress` plugin by plugin name.
    #[must_use]
    pub fn sensor_ingress_plugin(&self, name: &PluginName) -> Option<Arc<dyn SensorIngress>> {
        self.sensor_ingress.get(name).cloned()
    }

    /// Look up a registered `MCPServer` by plugin name.
    #[must_use]
    pub fn mcp_server(&self, name: &PluginName) -> Option<Arc<dyn MCPServer>> {
        self.mcp_servers.get(name).cloned()
    }

    /// Look up a registered `FrontendAdapter` by plugin name.
    #[must_use]
    pub fn frontend_adapter(&self, name: &PluginName) -> Option<Arc<dyn FrontendAdapter>> {
        self.frontend_adapters.get(name).cloned()
    }

    /// Look up a registered `AgentProvider` by plugin name.
    #[must_use]
    pub fn agent_provider(&self, name: &PluginName) -> Option<Arc<dyn AgentProvider>> {
        self.agent_providers.get(name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::memory_store::{CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities};

    // -- PluginName tests -------------------------------------------------

    #[test]
    fn name_accepts_kebab_alnum() {
        assert!(PluginName::new("cairn-store-sqlite").is_ok());
        assert!(PluginName::new("acme-llm-2").is_ok());
        assert!(PluginName::new("a1b").is_ok());
    }

    #[test]
    fn name_rejects_uppercase() {
        assert!(matches!(
            PluginName::new("Cairn-Store"),
            Err(PluginError::InvalidName(_))
        ));
    }

    #[test]
    fn name_rejects_underscore() {
        assert!(matches!(
            PluginName::new("cairn_store"),
            Err(PluginError::InvalidName(_))
        ));
    }

    #[test]
    fn name_rejects_too_short() {
        assert!(matches!(
            PluginName::new("ab"),
            Err(PluginError::InvalidName(_))
        ));
    }

    #[test]
    fn name_rejects_leading_hyphen() {
        assert!(matches!(
            PluginName::new("-cairn"),
            Err(PluginError::InvalidName(_))
        ));
    }

    // -- Registry tests ---------------------------------------------------

    struct StubStore {
        name: &'static str,
        range: VersionRange,
    }

    #[async_trait::async_trait]
    impl MemoryStore for StubStore {
        fn name(&self) -> &str {
            self.name
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: true,
                vector: false,
                graph_edges: false,
                transactions: true,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            self.range
        }
    }

    fn compatible() -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    fn incompatible() -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 2, 0), ContractVersion::new(0, 3, 0))
    }

    #[test]
    fn registers_and_resolves_memory_store() {
        let mut reg = PluginRegistry::new();
        let name = PluginName::new("cairn-store-sqlite").expect("valid");
        reg.register_memory_store(
            name.clone(),
            Arc::new(StubStore {
                name: "cairn-store-sqlite",
                range: compatible(),
            }),
        )
        .expect("compatible plugin registers");
        let resolved = reg.memory_store(&name).expect("registered");
        assert_eq!(resolved.name(), "cairn-store-sqlite");
    }

    #[test]
    fn rejects_duplicate_name() {
        let mut reg = PluginRegistry::new();
        let name = PluginName::new("cairn-store-sqlite").expect("valid");
        reg.register_memory_store(
            name.clone(),
            Arc::new(StubStore {
                name: "cairn-store-sqlite",
                range: compatible(),
            }),
        )
        .expect("first registers");
        let err = reg
            .register_memory_store(
                name,
                Arc::new(StubStore {
                    name: "cairn-store-sqlite",
                    range: compatible(),
                }),
            )
            .expect_err("duplicate must fail");
        assert!(matches!(err, PluginError::DuplicateName { .. }));
    }

    #[test]
    fn rejects_incompatible_contract_version() {
        let mut reg = PluginRegistry::new();
        let name = PluginName::new("acme-store-future").expect("valid");
        let err = reg
            .register_memory_store(
                name,
                Arc::new(StubStore {
                    name: "acme-store-future",
                    range: incompatible(),
                }),
            )
            .expect_err("incompatible plugin must fail closed");
        match err {
            PluginError::UnsupportedContractVersion { host, contract, .. } => {
                assert_eq!(host, CONTRACT_VERSION);
                assert_eq!(contract, "MemoryStore");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn rejects_identity_mismatch() {
        let mut reg = PluginRegistry::new();
        let key = PluginName::new("acme-store").expect("valid");
        let err = reg
            .register_memory_store(
                key,
                Arc::new(StubStore {
                    name: "different-runtime-name",
                    range: compatible(),
                }),
            )
            .expect_err("identity mismatch must fail closed");
        assert!(matches!(err, PluginError::IdentityMismatch { .. }));
    }

    #[test]
    fn lookup_returns_none_for_unknown_name() {
        let reg = PluginRegistry::new();
        let name = PluginName::new("unknown").expect("valid");
        assert!(reg.memory_store(&name).is_none());
    }
}
