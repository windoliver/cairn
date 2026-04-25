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
/// # Examples
/// ```ignore
/// use cairn_core::contract::register_plugin;
/// # use cairn_core::contract::memory_store::{MemoryStore, MemoryStoreCapabilities};
/// # use cairn_core::contract::version::{ContractVersion, VersionRange};
///
/// #[derive(Default)]
/// struct MyStore;
///
/// #[async_trait::async_trait]
/// impl MemoryStore for MyStore {
///     fn name(&self) -> &str { "acme-store" }
///     fn capabilities(&self) -> &MemoryStoreCapabilities { unimplemented!() }
///     fn supported_contract_versions(&self) -> VersionRange { unimplemented!() }
/// }
///
/// register_plugin!(MemoryStore, MyStore, "acme-store");
/// ```
#[macro_export]
macro_rules! register_plugin {
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
