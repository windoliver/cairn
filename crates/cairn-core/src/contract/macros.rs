//! `register_plugin!` declarative macro.
//!
//! Brief §4.1: "Plugins call `cairn_core::register_plugin!(<trait>, <impl>,
//! <name>)` in their entry point. The host assembles the active set from
//! config at startup."
//!
//! The macro expands to a public `pub fn register(reg: &mut PluginRegistry)`
//! that constructs the impl via `Default::default()` and inserts it into
//! the registry under the given contract.

/// Emit a `register(&mut PluginRegistry) -> Result<(), PluginError>` function
/// in the calling crate that registers `$impl` under contract `$contract`
/// with the stable name `$name`.
///
/// # Construction discipline
///
/// The generated `register` function performs **static version and identity
/// checks** via the `XxxPlugin` associated consts (`SUPPORTED_VERSIONS`,
/// `NAME`) **before** calling `<$impl as Default>::default()`. Incompatible
/// or misidentified plugins are therefore rejected without ever constructing
/// an instance.
///
/// Implementations of [`Default::default`] must still be **side-effect
/// free**: no I/O, no resource allocation, no panics. Open files, network
/// connections, model handles, etc. belong in a separate `init`/`open` step
/// the host invokes after registration — typically driven by
/// `.cairn/config.yaml` (brief §4.1).
///
/// Config-driven construction (e.g., a `MemoryStore` impl that needs a
/// database path at build time) is supported via `register_plugin_with!`
/// which accepts a factory closure and performs the same static pre-checks.
///
/// # Examples
///
/// ```
/// # use cairn_core::contract::memory_store::{MemoryStore, MemoryStoreCapabilities, MemoryStorePlugin};
/// # use cairn_core::contract::version::{ContractVersion, VersionRange};
/// use cairn_core::register_plugin;
///
/// #[derive(Default)]
/// struct MyStore;
///
/// #[async_trait::async_trait]
/// impl MemoryStore for MyStore {
///     fn name(&self) -> &str { Self::NAME }
///     fn capabilities(&self) -> &MemoryStoreCapabilities {
///         static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
///             fts: false,
///             vector: false,
///             graph_edges: false,
///             transactions: false,
///         };
///         &CAPS
///     }
///     fn supported_contract_versions(&self) -> VersionRange { Self::SUPPORTED_VERSIONS }
///     async fn get(&self, _: &str) -> Result<Option<cairn_core::contract::memory_store::StoredRecord>, cairn_core::contract::memory_store::StoreError> {
///         Err(cairn_core::contract::memory_store::StoreError::Unimplemented)
///     }
///     async fn upsert(&self, _: cairn_core::domain::record::MemoryRecord) -> Result<cairn_core::contract::memory_store::StoredRecord, cairn_core::contract::memory_store::StoreError> {
///         Err(cairn_core::contract::memory_store::StoreError::Unimplemented)
///     }
///     async fn list_active(&self) -> Result<Vec<cairn_core::contract::memory_store::StoredRecord>, cairn_core::contract::memory_store::StoreError> {
///         Err(cairn_core::contract::memory_store::StoreError::Unimplemented)
///     }
/// }
///
/// impl MemoryStorePlugin for MyStore {
///     const NAME: &'static str = "acme-store";
///     const SUPPORTED_VERSIONS: VersionRange = VersionRange::new(
///         ContractVersion::new(0, 2, 0),
///         ContractVersion::new(0, 3, 0),
///     );
/// }
///
/// register_plugin!(MemoryStore, MyStore, "acme-store");
///
/// // The macro emits a `register` fn that hosts call during startup.
/// // Constructing a registry and verifying registration works:
/// let mut reg = cairn_core::contract::registry::PluginRegistry::new();
/// register(&mut reg).expect("compatible plugin registers");
/// ```
///
/// # Manifest-aware form (preferred for bundled plugins)
///
/// ```
/// # use cairn_core::contract::memory_store::{MemoryStore, MemoryStoreCapabilities, MemoryStorePlugin};
/// # use cairn_core::contract::version::{ContractVersion, VersionRange};
/// use cairn_core::register_plugin;
///
/// const MANIFEST_TOML: &str = r#"
/// name = "acme-store"
/// contract = "MemoryStore"
///
/// [contract_version_range.min]
/// major = 0
/// minor = 2
/// patch = 0
///
/// [contract_version_range.max_exclusive]
/// major = 0
/// minor = 3
/// patch = 0
/// "#;
///
/// #[derive(Default)]
/// struct MyStore;
///
/// #[async_trait::async_trait]
/// impl MemoryStore for MyStore {
///     fn name(&self) -> &str { Self::NAME }
///     fn capabilities(&self) -> &MemoryStoreCapabilities {
///         static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
///             fts: false,
///             vector: false,
///             graph_edges: false,
///             transactions: false,
///         };
///         &CAPS
///     }
///     fn supported_contract_versions(&self) -> VersionRange { Self::SUPPORTED_VERSIONS }
///     async fn get(&self, _: &str) -> Result<Option<cairn_core::contract::memory_store::StoredRecord>, cairn_core::contract::memory_store::StoreError> {
///         Err(cairn_core::contract::memory_store::StoreError::Unimplemented)
///     }
///     async fn upsert(&self, _: cairn_core::domain::record::MemoryRecord) -> Result<cairn_core::contract::memory_store::StoredRecord, cairn_core::contract::memory_store::StoreError> {
///         Err(cairn_core::contract::memory_store::StoreError::Unimplemented)
///     }
///     async fn list_active(&self) -> Result<Vec<cairn_core::contract::memory_store::StoredRecord>, cairn_core::contract::memory_store::StoreError> {
///         Err(cairn_core::contract::memory_store::StoreError::Unimplemented)
///     }
/// }
///
/// impl MemoryStorePlugin for MyStore {
///     const NAME: &'static str = "acme-store";
///     const SUPPORTED_VERSIONS: VersionRange = VersionRange::new(
///         ContractVersion::new(0, 2, 0),
///         ContractVersion::new(0, 3, 0),
///     );
/// }
///
/// register_plugin!(MemoryStore, MyStore, "acme-store", MANIFEST_TOML);
///
/// let mut reg = cairn_core::contract::registry::PluginRegistry::new();
/// register(&mut reg).expect("compatible plugin registers");
/// assert!(reg.parsed_manifest(
///     &cairn_core::contract::registry::PluginName::new("acme-store").unwrap()
/// ).is_some());
/// ```
#[macro_export]
macro_rules! register_plugin {
    // 3-arg form: legacy / unit-test path with no manifest.
    (MemoryStore, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(
            register_memory_store,
            $crate::contract::memory_store::MemoryStorePlugin,
            "MemoryStore",
            $crate::contract::memory_store::CONTRACT_VERSION,
            $impl,
            $name
        );
    };
    (LLMProvider, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(
            register_llm_provider,
            $crate::contract::llm_provider::LLMProviderPlugin,
            "LLMProvider",
            $crate::contract::llm_provider::CONTRACT_VERSION,
            $impl,
            $name
        );
    };
    (WorkflowOrchestrator, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(
            register_workflow_orchestrator,
            $crate::contract::workflow_orchestrator::WorkflowOrchestratorPlugin,
            "WorkflowOrchestrator",
            $crate::contract::workflow_orchestrator::CONTRACT_VERSION,
            $impl,
            $name
        );
    };
    (SensorIngress, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(
            register_sensor_ingress,
            $crate::contract::sensor_ingress::SensorIngressPlugin,
            "SensorIngress",
            $crate::contract::sensor_ingress::CONTRACT_VERSION,
            $impl,
            $name
        );
    };
    (MCPServer, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(
            register_mcp_server,
            $crate::contract::mcp_server::MCPServerPlugin,
            "MCPServer",
            $crate::contract::mcp_server::CONTRACT_VERSION,
            $impl,
            $name
        );
    };
    (FrontendAdapter, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(
            register_frontend_adapter,
            $crate::contract::frontend_adapter::FrontendAdapterPlugin,
            "FrontendAdapter",
            $crate::contract::frontend_adapter::CONTRACT_VERSION,
            $impl,
            $name
        );
    };
    (AgentProvider, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(
            register_agent_provider,
            $crate::contract::agent_provider::AgentProviderPlugin,
            "AgentProvider",
            $crate::contract::agent_provider::CONTRACT_VERSION,
            $impl,
            $name
        );
    };

    // 4-arg form: manifest-aware. `$manifest` is an expression producing a
    // `&'static str` (typically a `pub const MANIFEST_TOML: &str = include_str!(...)`).
    (MemoryStore, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_memory_store_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (LLMProvider, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_llm_provider_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (WorkflowOrchestrator, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_workflow_orchestrator_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (SensorIngress, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_sensor_ingress_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (MCPServer, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_mcp_server_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (FrontendAdapter, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_frontend_adapter_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (AgentProvider, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_agent_provider_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __register_plugin_helper {
    ($method:ident, $plugin_trait:path, $contract:literal, $host_version:expr, $impl:ty, $name:literal) => {
        /// Plugin entry point. Hosts call this during startup assembly.
        ///
        /// Performs static version and identity checks via the `XxxPlugin`
        /// associated consts before constructing the plugin, so incompatible
        /// plugins are rejected without running `Default::default()`.
        ///
        /// # Errors
        /// Returns [`cairn_core::contract::registry::PluginError`] when the
        /// name is invalid, the contract version is unsupported, the static
        /// `NAME` const disagrees with the registered name, or another plugin
        /// already holds this name.
        pub fn register(
            reg: &mut $crate::contract::registry::PluginRegistry,
        ) -> ::core::result::Result<(), $crate::contract::registry::PluginError> {
            let name = $crate::contract::registry::PluginName::new($name)?;
            // Static version check — no construction yet.
            let supported = <$impl as $plugin_trait>::SUPPORTED_VERSIONS;
            let host = $host_version;
            if !supported.accepts(host) {
                return Err(
                    $crate::contract::registry::PluginError::UnsupportedContractVersion {
                        contract: $contract,
                        plugin: name,
                        plugin_range: supported,
                        host,
                    },
                );
            }
            // Static identity check — no construction yet.
            let static_name = <$impl as $plugin_trait>::NAME;
            if static_name != $name {
                return Err($crate::contract::registry::PluginError::IdentityMismatch {
                    contract: $contract,
                    registered: name,
                    runtime: static_name.to_owned(),
                });
            }
            reg.$method(
                name,
                ::std::sync::Arc::new(<$impl as ::core::default::Default>::default()),
            )
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __register_plugin_with_manifest_helper {
    ($method:ident, $impl:ty, $name:literal, $manifest:expr) => {
        /// Plugin entry point with manifest-aware registration.
        ///
        /// # Errors
        /// Returns [`cairn_core::contract::registry::PluginError`] when the
        /// name is invalid, the manifest fails to parse, the manifest
        /// disagrees with the registered name / contract / host version,
        /// or another plugin already holds this name.
        pub fn register(
            reg: &mut $crate::contract::registry::PluginRegistry,
        ) -> ::core::result::Result<(), $crate::contract::registry::PluginError> {
            let name = $crate::contract::registry::PluginName::new($name)?;
            let manifest = $crate::contract::manifest::PluginManifest::parse_toml($manifest)?;
            reg.$method(
                name,
                manifest,
                ::std::sync::Arc::new(<$impl as ::core::default::Default>::default()),
            )
        }
    };
}

/// Emit a config-driven
/// `register(&mut PluginRegistry, &CairnConfig) -> Result<(), PluginError>`
/// function for plugins that require runtime configuration at construction time.
///
/// # Key guarantee
///
/// The factory closure is called **only after** the static version and identity
/// checks pass. Callers can prove this by supplying a factory that panics — the
/// test will not panic for an incompatible plugin.
///
/// # Factory type
///
/// The closure receives `&CairnConfig` and returns `Result<Impl, E>` for any
/// `E: std::error::Error + Send + Sync + 'static`. The macro boxes the error
/// into [`crate::contract::registry::PluginError::FactoryError`].
///
/// # Examples
///
/// ```
/// # use cairn_core::contract::memory_store::{MemoryStore, MemoryStoreCapabilities, MemoryStorePlugin};
/// # use cairn_core::contract::version::{ContractVersion, VersionRange};
/// use cairn_core::register_plugin_with;
///
/// struct ConfigStore { vault_name: String }
///
/// #[async_trait::async_trait]
/// impl MemoryStore for ConfigStore {
///     fn name(&self) -> &str { Self::NAME }
///     fn capabilities(&self) -> &MemoryStoreCapabilities {
///         static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
///             fts: false, vector: false, graph_edges: false, transactions: false,
///         };
///         &CAPS
///     }
///     fn supported_contract_versions(&self) -> VersionRange { Self::SUPPORTED_VERSIONS }
///     async fn get(&self, _: &str) -> Result<Option<cairn_core::contract::memory_store::StoredRecord>, cairn_core::contract::memory_store::StoreError> {
///         Err(cairn_core::contract::memory_store::StoreError::Unimplemented)
///     }
///     async fn upsert(&self, _: cairn_core::domain::record::MemoryRecord) -> Result<cairn_core::contract::memory_store::StoredRecord, cairn_core::contract::memory_store::StoreError> {
///         Err(cairn_core::contract::memory_store::StoreError::Unimplemented)
///     }
///     async fn list_active(&self) -> Result<Vec<cairn_core::contract::memory_store::StoredRecord>, cairn_core::contract::memory_store::StoreError> {
///         Err(cairn_core::contract::memory_store::StoreError::Unimplemented)
///     }
/// }
///
/// impl MemoryStorePlugin for ConfigStore {
///     const NAME: &'static str = "config-store";
///     const SUPPORTED_VERSIONS: VersionRange = VersionRange::new(
///         ContractVersion::new(0, 2, 0),
///         ContractVersion::new(0, 3, 0),
///     );
/// }
///
/// register_plugin_with!(MemoryStore, ConfigStore, "config-store", |cfg: &cairn_core::config::CairnConfig| {
///     Ok::<_, std::convert::Infallible>(ConfigStore { vault_name: cfg.vault.name.clone() })
/// });
///
/// let mut reg = cairn_core::contract::registry::PluginRegistry::new();
/// let cfg = cairn_core::config::CairnConfig::default();
/// register(&mut reg, &cfg).expect("config-driven plugin registers");
/// ```
#[macro_export]
macro_rules! register_plugin_with {
    (MemoryStore, $impl:ty, $name:literal, $factory:expr) => {
        $crate::__register_plugin_with_helper!(
            register_memory_store,
            $crate::contract::memory_store::MemoryStorePlugin,
            "MemoryStore",
            $crate::contract::memory_store::CONTRACT_VERSION,
            $impl,
            $name,
            $factory
        );
    };
    (LLMProvider, $impl:ty, $name:literal, $factory:expr) => {
        $crate::__register_plugin_with_helper!(
            register_llm_provider,
            $crate::contract::llm_provider::LLMProviderPlugin,
            "LLMProvider",
            $crate::contract::llm_provider::CONTRACT_VERSION,
            $impl,
            $name,
            $factory
        );
    };
    (WorkflowOrchestrator, $impl:ty, $name:literal, $factory:expr) => {
        $crate::__register_plugin_with_helper!(
            register_workflow_orchestrator,
            $crate::contract::workflow_orchestrator::WorkflowOrchestratorPlugin,
            "WorkflowOrchestrator",
            $crate::contract::workflow_orchestrator::CONTRACT_VERSION,
            $impl,
            $name,
            $factory
        );
    };
    (SensorIngress, $impl:ty, $name:literal, $factory:expr) => {
        $crate::__register_plugin_with_helper!(
            register_sensor_ingress,
            $crate::contract::sensor_ingress::SensorIngressPlugin,
            "SensorIngress",
            $crate::contract::sensor_ingress::CONTRACT_VERSION,
            $impl,
            $name,
            $factory
        );
    };
    (MCPServer, $impl:ty, $name:literal, $factory:expr) => {
        $crate::__register_plugin_with_helper!(
            register_mcp_server,
            $crate::contract::mcp_server::MCPServerPlugin,
            "MCPServer",
            $crate::contract::mcp_server::CONTRACT_VERSION,
            $impl,
            $name,
            $factory
        );
    };
    (FrontendAdapter, $impl:ty, $name:literal, $factory:expr) => {
        $crate::__register_plugin_with_helper!(
            register_frontend_adapter,
            $crate::contract::frontend_adapter::FrontendAdapterPlugin,
            "FrontendAdapter",
            $crate::contract::frontend_adapter::CONTRACT_VERSION,
            $impl,
            $name,
            $factory
        );
    };
    (AgentProvider, $impl:ty, $name:literal, $factory:expr) => {
        $crate::__register_plugin_with_helper!(
            register_agent_provider,
            $crate::contract::agent_provider::AgentProviderPlugin,
            "AgentProvider",
            $crate::contract::agent_provider::CONTRACT_VERSION,
            $impl,
            $name,
            $factory
        );
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __register_plugin_with_helper {
    ($method:ident, $plugin_trait:path, $contract:literal, $host_version:expr, $impl:ty, $name:literal, $factory:expr) => {
        /// Config-driven plugin entry point.
        ///
        /// # Errors
        /// Returns [`cairn_core::contract::registry::PluginError`] when the
        /// name is invalid, the static `SUPPORTED_VERSIONS` const rejects the
        /// host version, the static `NAME` const disagrees with the registered
        /// name, the factory closure fails, or another plugin already holds
        /// this name.
        pub fn register(
            reg: &mut $crate::contract::registry::PluginRegistry,
            cfg: &$crate::config::CairnConfig,
        ) -> ::core::result::Result<(), $crate::contract::registry::PluginError> {
            let name = $crate::contract::registry::PluginName::new($name)?;
            // Static version check — factory NOT called yet.
            let supported = <$impl as $plugin_trait>::SUPPORTED_VERSIONS;
            let host = $host_version;
            if !supported.accepts(host) {
                return Err(
                    $crate::contract::registry::PluginError::UnsupportedContractVersion {
                        contract: $contract,
                        plugin: name,
                        plugin_range: supported,
                        host,
                    },
                );
            }
            // Static identity check — factory NOT called yet.
            let static_name = <$impl as $plugin_trait>::NAME;
            if static_name != $name {
                return Err($crate::contract::registry::PluginError::IdentityMismatch {
                    contract: $contract,
                    registered: name,
                    runtime: static_name.to_owned(),
                });
            }
            // All static checks passed. Call the factory.
            let plugin: $impl = ($factory)(cfg).map_err(|e| {
                $crate::contract::registry::PluginError::FactoryError {
                    contract: $contract,
                    plugin: name.clone(),
                    source: ::std::boxed::Box::new(e),
                }
            })?;
            reg.$method(name, ::std::sync::Arc::new(plugin))
        }
    };
}
