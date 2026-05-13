//! Cairn contract surface.
//!
//! Brief §4.1 — every contract is a trait, every trait declares
//! `CONTRACT_VERSION`, plugins register through `register_plugin!` and end
//! up in a [`registry::PluginRegistry`] the host assembles at startup.
//!
//! Public surface (re-exported from this module):
//! - [`ContractVersion`], [`VersionRange`] — versioning primitives (full path: [`version`]).
//! - [`PluginRegistry`], [`PluginName`], [`PluginError`] — runtime registry (full path: [`registry`]).
//! - [`PluginManifest`], [`ContractKind`] — capability manifest (full path: [`manifest`]).
//! - Five P0 contract traits + their capability structs:
//!   [`MemoryStore`] / [`MemoryStoreCapabilities`],
//!   [`LLMProvider`] / [`LLMProviderCapabilities`],
//!   [`WorkflowOrchestrator`] / [`WorkflowOrchestratorCapabilities`],
//!   [`SensorIngress`] / [`SensorIngressCapabilities`],
//!   [`MCPServer`] / [`MCPServerCapabilities`].
//! - Identity provisioning contract (§4.1): [`Keystore`] / [`KeystoreError`],
//!   [`IdentityRegistry`] / [`RegistryError`] / [`IdentityVisibility`] /
//!   [`MaintenanceMode`] / [`PurgeAcknowledgement`] / [`PurgeReason`].
//! - Forward stubs (P1/P2, hidden until #113 / #124): `FrontendAdapter`, `AgentProvider`.
//! - Metrics contract (§15): [`MetricsSink`] / [`MetricsError`],
//!   [`CapturingMetricsSink`], [`NoopMetricsSink`].

pub mod agent_provider;
pub mod conformance;
pub mod consent_lookup;
pub mod frontend_adapter;
pub mod hot_prefix_cache;
pub mod identity_registry;
pub mod job_store;
pub mod keystore;
pub mod llm_provider;
pub mod manifest;
pub mod mcp_server;
pub mod memory_store;
pub mod metrics;
pub mod registry;
pub mod sensor_ingress;
pub mod version;
pub mod workflow_orchestrator;

#[doc(hidden)]
pub mod macros;

// Flat re-exports so users can write `cairn_core::contract::MemoryStore`
// instead of `cairn_core::contract::memory_store::MemoryStore`.
pub use conformance::{CaseOutcome, CaseStatus, Tier, run_conformance_for_plugin};
pub use manifest::{ContractKind, PluginManifest};
pub use registry::{PluginError, PluginName, PluginRegistry};
pub use version::{ContractVersion, VersionRange};

pub use agent_provider::{AgentProvider, AgentProviderCapabilities, AgentProviderPlugin};
pub use consent_lookup::{ConsentLookup, ConsentLookupError};
pub use frontend_adapter::{
    FrontendAdapter, FrontendAdapterCapabilities, FrontendAdapterError, FrontendAdapterPlugin,
    FrontendBackendState, FrontendEdit, FrontendEventStream, FrontendFieldClass,
    FrontendFieldPolicy, FrontendIdentityContext, FrontendProjection, FrontendProjectionRequest,
    FrontendReconcileError, FrontendReconcileRequest, FrontendSubscription,
};
pub use hot_prefix_cache::{CacheError, CachedPrefix, HotPrefixCache};
pub use identity_registry::{
    IdentityRegistry, IdentityVisibility, MaintenanceMode, PurgeAcknowledgement, PurgeReason,
    RegistryError,
};
pub use job_store::{
    EnqueueRequest, FailDisposition, JobId, JobKind, JobPayload, JobState, JobStore, JobStoreError,
    LeaseToken, LeasedJob, RetryPolicy,
};
pub use keystore::{Keystore, KeystoreError};
pub use llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LLMProviderPlugin,
    LlmError,
};
pub use mcp_server::{MCPServer, MCPServerCapabilities, MCPServerPlugin};
pub use memory_store::{MemoryStore, MemoryStoreCapabilities, MemoryStorePlugin};
pub use metrics::{CapturingMetricsSink, MetricsError, MetricsSink, NoopMetricsSink};
pub use sensor_ingress::{SensorIngress, SensorIngressCapabilities, SensorIngressPlugin};
pub use workflow_orchestrator::{
    WorkflowOrchestrator, WorkflowOrchestratorCapabilities, WorkflowOrchestratorPlugin,
};
