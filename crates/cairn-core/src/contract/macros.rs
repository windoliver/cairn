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
/// The generated `register` function constructs the impl via
/// `<$impl as Default>::default()` **before** the registry runs version,
/// identity, or duplicate-name checks. Implementations of [`Default::default`]
/// must therefore be **side-effect free**: no I/O, no resource allocation,
/// no panics. Open files, network connections, model handles, etc. belong in
/// a separate `init`/`open` step the host invokes after registration —
/// typically driven by `.cairn/config.yaml` (brief §4.1).
///
/// Config-driven construction (e.g., a `MemoryStore` impl that needs a
/// database path at build time) is **not yet supported** by this macro;
/// it will arrive in a follow-up issue that adds a factory variant
/// (`register_plugin_with!(<contract>, <impl>, <name>, |cfg| <factory>)`)
/// alongside trait-level associated consts so compatibility can be checked
/// before construction. Until then: keep `Default` cheap, push real init
/// behind an explicit `init(&Config)` method on your impl.
///
/// # Examples
///
/// ```
/// # use cairn_core::contract::memory_store::{MemoryStore, MemoryStoreCapabilities};
/// # use cairn_core::contract::version::{ContractVersion, VersionRange};
/// use cairn_core::register_plugin;
///
/// #[derive(Default)]
/// struct MyStore;
///
/// #[async_trait::async_trait]
/// impl MemoryStore for MyStore {
///     fn name(&self) -> &str { "acme-store" }
///     fn capabilities(&self) -> &MemoryStoreCapabilities {
///         static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
///             fts: false,
///             vector: false,
///             graph_edges: false,
///             transactions: false,
///         };
///         &CAPS
///     }
///     fn supported_contract_versions(&self) -> VersionRange {
///         VersionRange::new(
///             ContractVersion::new(0, 2, 0),
///             ContractVersion::new(0, 3, 0),
///         )
///     }
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
/// # use cairn_core::contract::memory_store::{MemoryStore, MemoryStoreCapabilities};
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
///     fn name(&self) -> &str { "acme-store" }
///     fn capabilities(&self) -> &MemoryStoreCapabilities {
///         static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
///             fts: false,
///             vector: false,
///             graph_edges: false,
///             transactions: false,
///         };
///         &CAPS
///     }
///     fn supported_contract_versions(&self) -> VersionRange {
///         VersionRange::new(
///             ContractVersion::new(0, 2, 0),
///             ContractVersion::new(0, 3, 0),
///         )
///     }
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
        $crate::__register_plugin_helper!(register_memory_store, $impl, $name);
    };
    (LLMProvider, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_llm_provider, $impl, $name);
    };
    (WorkflowOrchestrator, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_workflow_orchestrator, $impl, $name);
    };
    (SensorIngress, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_sensor_ingress, $impl, $name);
    };
    (MCPServer, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_mcp_server, $impl, $name);
    };
    (FrontendAdapter, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_frontend_adapter, $impl, $name);
    };
    (AgentProvider, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_agent_provider, $impl, $name);
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
    ($method:ident, $impl:ty, $name:literal) => {
        /// Plugin entry point. Hosts call this during startup assembly.
        ///
        /// # Errors
        /// Returns [`cairn_core::contract::registry::PluginError`] when the
        /// name is invalid, the contract version is unsupported, or another
        /// plugin already holds this name.
        pub fn register(
            reg: &mut $crate::contract::registry::PluginRegistry,
        ) -> ::core::result::Result<(), $crate::contract::registry::PluginError> {
            let name = $crate::contract::registry::PluginName::new($name)?;
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
