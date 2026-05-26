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
//! - Federation transport contract (§12.a, issue #123): [`FederationTransport`] /
//!   [`SendOutcome`] / [`TransportReason`].
//! - Connector consent journal (§14 + §19 v0.3, issue #130):
//!   [`ConnectorConsentJournal`] / [`ConsentGrant`] / [`ConsentGrantId`] /
//!   [`ConnectorConsentLookup`].

pub mod admin_state;
pub mod agent_provider;
pub mod conformance;
pub mod connector_consent;
pub mod consent_journal;
pub mod consent_lookup;
pub mod federation_outbox;
pub mod federation_transport;
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
pub mod snapshot_artifact;
pub mod source_resolver;
pub mod version;
pub mod workflow_jobs;
pub mod workflow_orchestrator;

#[doc(hidden)]
pub mod macros;

// Flat re-exports so users can write `cairn_core::contract::MemoryStore`
// instead of `cairn_core::contract::memory_store::MemoryStore`.
pub use conformance::{CaseOutcome, CaseStatus, Tier, run_conformance_for_plugin};
pub use manifest::{ContractKind, PluginManifest};
pub use registry::{PluginError, PluginName, PluginRegistry};
pub use version::{ContractVersion, VersionRange};

pub use admin_state::{AdminStateStore, ConnectorStateRow};
pub use agent_provider::{
    AgentBudgetConsumed, AgentCostBudget, AgentIdentity, AgentOutput, AgentOutputSchema,
    AgentProvider, AgentProviderCapabilities, AgentProviderError, AgentProviderPlugin, AgentRun,
    AgentRunMeter, AgentRunStatus, AgentScope, AgentSpawnRequest, AgentToolAllowlist,
    AgentToolAttempt, AgentToolCall, AgentToolPolicyOutcome, AgentWallClockBudget, CairnVerb,
    evaluate_tool_policy, validate_output,
};
pub use connector_consent::{
    ConnectorConsentJournal, ConnectorConsentLookup, ConsentGrant, ConsentGrantId,
};
pub use consent_journal::{
    ConsentJournalReader, MalformedSourceForget, MalformedSourceForgetReason, SourceForget,
    TargetReplayKey,
};
pub use consent_lookup::{ConsentLookup, ConsentLookupError, FederationAcceptRecord};
pub use federation_outbox::{FederationOutbox, FederationOutboxError};
pub use federation_transport::{FederationTransport, SendOutcome, TransportReason};
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
    EnqueueRequest, FailDisposition, FailureClass, JobId, JobKind, JobPayload, JobState, JobStore,
    JobStoreError, LeaseToken, LeasedJob, ReclaimedRow, RetryPolicy,
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
pub use snapshot_artifact::{
    BackupRegistry, ConsentLog, MaterializedArtifact, SnapshotApplier, SnapshotArtifactProducer,
    SnapshotArtifactReader, SnapshotMetadataProvider,
};
pub use source_resolver::{SourceResolver, SourceResolverError};
pub use workflow_jobs::{DeadLetterRow, WorkflowJobsReader};
pub use workflow_orchestrator::{
    WorkflowOrchestrator, WorkflowOrchestratorCapabilities, WorkflowOrchestratorPlugin,
};
