//! `FrontendAdapter` contract surface (brief §4 row 7, §13.5.c, §13.5.d).
//!
//! The contract stays pure and core-only: it models projection / reconcile
//! requests and typed failures, but does not depend on any frontend runtime.

use std::collections::BTreeMap;

use crate::domain::{CanonicalRecordHash, Identity, TargetId, VerifiedSignedIntent};
use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `FrontendAdapter`. Bumps when the trait surface changes.
#[doc(hidden)]
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

/// Static capability declaration for a `FrontendAdapter` impl.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[doc(hidden)]
pub struct FrontendAdapterCapabilities {
    /// Whether the adapter can render selected backend fields as frontmatter.
    pub frontmatter: bool,
    /// Whether the adapter can emit auxiliary sidecar files.
    pub sidecar_files: bool,
    /// Whether the adapter supports a live plugin or event bridge.
    pub live_plugin: bool,
    /// Whether the adapter can project graph-oriented data.
    pub graph_view: bool,
    /// Maximum number of fields the adapter can safely render in frontmatter.
    pub max_frontmatter_fields: usize,
}

/// Field classes used by the frontend mutability policy in brief §13.5.c.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum FrontendFieldClass {
    /// Editable user-authored content such as body text and tags.
    UserContent,
    /// Informational metadata safe to edit from a frontend.
    Metadata,
    /// Backend-owned classifier outputs.
    Classification,
    /// Identity, signature, or provenance data.
    IdentityProvenance,
    /// Visibility or consent-governed fields.
    VisibilityConsent,
    /// Version and audit fields, plus unknown future fields.
    VersionAudit,
}

/// Fail-closed policy helpers for frontend-editable fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[doc(hidden)]
pub struct FrontendFieldPolicy;

impl FrontendFieldPolicy {
    /// Classify a projected field name into one of the frontend policy buckets.
    #[must_use]
    pub fn classify(field_name: &str) -> FrontendFieldClass {
        match field_name {
            "body" | "tags" | "wikilinks" => FrontendFieldClass::UserContent,
            "last_read_at" | "local_sort_key" => FrontendFieldClass::Metadata,
            "kind" | "confidence" | "evidence_vector" => FrontendFieldClass::Classification,
            "actor_chain" | "signature" | "key_version" | "operation_id" => {
                FrontendFieldClass::IdentityProvenance
            }
            "consent_tier" | "consent_receipt_ref" | "visibility" | "share_grants" => {
                FrontendFieldClass::VisibilityConsent
            }
            "version" | "promoted_at" | "produced_by" => FrontendFieldClass::VersionAudit,
            _ => FrontendFieldClass::VersionAudit,
        }
    }

    /// Whether the field may be changed by a frontend-originated edit.
    #[must_use]
    pub fn is_mutable_from_frontend(field_name: &str) -> bool {
        matches!(
            Self::classify(field_name),
            FrontendFieldClass::UserContent | FrontendFieldClass::Metadata
        )
    }
}

/// Narrow projection request for adapter-facing backend state projection.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct FrontendProjectionRequest {
    /// Target record or memory identifier.
    pub target_id: TargetId,
    /// Version the caller expects to project.
    pub expected_version: u64,
}

/// Narrow projection output consumed by frontends and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct FrontendProjection {
    /// Rendered markdown body.
    pub body: String,
    /// Key/value frontmatter fields.
    pub frontmatter: Vec<(String, String)>,
    /// Named sidecar file payloads.
    pub sidecars: Vec<(String, String)>,
    /// Backend hash the frontend must present back during reconcile.
    pub target_hash: CanonicalRecordHash,
}

/// Identity context attached to a frontend-originated reconcile request.
#[derive(Debug, Clone, PartialEq)]
#[doc(hidden)]
pub struct FrontendIdentityContext {
    /// Principal requesting the edit.
    pub principal: Identity,
    /// Optional agent identity on whose behalf the edit originated.
    pub agent: Option<Identity>,
    /// Signed intent envelope presented with the edit, if any.
    pub signed_intent: Option<VerifiedSignedIntent>,
}

/// Raw frontend edit observed by an adapter.
#[derive(Debug, Clone, PartialEq)]
#[doc(hidden)]
pub struct FrontendEdit {
    /// Target record or memory identifier.
    pub target_id: TargetId,
    /// Optimistic version observed by the frontend.
    pub expected_version: u64,
    /// Hash of the projected state the edit was based on.
    pub target_hash: CanonicalRecordHash,
    /// Full field diff observed by the frontend.
    pub field_diff: BTreeMap<String, serde_json::Value>,
}

/// Backend-facing reconcile request synthesized from a frontend edit.
#[derive(Debug, Clone, PartialEq)]
#[doc(hidden)]
pub struct FrontendReconcileRequest {
    /// Target record or memory identifier.
    pub target_id: TargetId,
    /// Optimistic version to check before applying.
    pub expected_version: u64,
    /// Hash of the projected state the edit was based on.
    pub target_hash: CanonicalRecordHash,
    /// Mutable fields extracted from the frontend diff.
    pub field_diff: BTreeMap<String, serde_json::Value>,
    /// Authenticated caller identity context.
    pub ctx: FrontendIdentityContext,
}

/// Typed reconcile failures surfaced by the frontend contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[doc(hidden)]
pub enum FrontendReconcileError {
    /// The frontend based its edit on a stale version.
    #[error("frontend reconcile conflict at version {current_version}")]
    Conflict { current_version: u64 },
    /// No signed intent envelope accompanied the edit.
    #[error("frontend reconcile requires a signed intent")]
    UnsignedIntent,
    /// The same signed operation was seen before.
    #[error("frontend reconcile detected a replayed operation")]
    ReplayDetected,
    /// The edit attempted to mutate a backend-owned field.
    #[error("frontend reconcile attempted to change immutable field {field}")]
    ImmutableFieldChanged { field: String },
    /// A policy gate rejected the reconcile request.
    #[error("frontend reconcile policy denied: {gate}: {reason}")]
    PolicyDenied { gate: String, reason: String },
    /// Caller lacks a capability required for the attempted edit.
    #[error("frontend reconcile requires capability {required}")]
    InsufficientCapability { required: String },
}

/// Top-level adapter failures for projection and reconcile calls.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[doc(hidden)]
pub enum FrontendAdapterError {
    /// Adapter left an operation unimplemented.
    #[error("frontend adapter does not implement {operation}")]
    NotImplemented { operation: &'static str },
    /// Projection failed before any reconcile phase began.
    #[error("frontend projection failed: {message}")]
    Projection { message: String },
    /// Reconcile failed with a typed policy or concurrency reason.
    #[error(transparent)]
    Reconcile(#[from] FrontendReconcileError),
}

/// Placeholder event stream handle for live frontend subscriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[doc(hidden)]
pub struct FrontendEventStream;

/// Placeholder subscription token for live frontend bridges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[doc(hidden)]
pub struct FrontendSubscription;

/// Frontend adapter contract — projects vault state into editor / desktop UIs.
#[async_trait::async_trait]
#[doc(hidden)]
pub trait FrontendAdapter: Send + Sync {
    /// Stable identifier of the registered plugin instance.
    fn name(&self) -> &str;

    /// Static capability advertisement (brief §4.1).
    fn capabilities(&self) -> &FrontendAdapterCapabilities;

    /// Range of `FrontendAdapter::CONTRACT_VERSION` values this impl accepts.
    fn supported_contract_versions(&self) -> VersionRange;

    /// Project backend state into the frontend-specific representation.
    fn project(
        &self,
        _request: &FrontendProjectionRequest,
    ) -> Result<FrontendProjection, FrontendAdapterError> {
        Err(FrontendAdapterError::NotImplemented {
            operation: "project",
        })
    }

    /// Translate a frontend edit into a backend reconcile request.
    fn reconcile(
        &self,
        _ctx: FrontendIdentityContext,
        _edit: FrontendEdit,
    ) -> Result<FrontendReconcileRequest, FrontendAdapterError> {
        Err(FrontendAdapterError::NotImplemented {
            operation: "reconcile",
        })
    }

    /// Optional live channel for frontends that support it.
    fn subscribe(&self, _events: FrontendEventStream) -> Option<FrontendSubscription> {
        None
    }

    /// Optional teardown hook for graceful shutdown.
    fn shutdown(&self) {}
}

/// Static identity descriptor for a [`FrontendAdapter`] plugin (§4.1).
#[doc(hidden)]
pub trait FrontendAdapterPlugin: FrontendAdapter + Sized {
    /// Stable plugin name, checked statically before construction (§4.1).
    const NAME: &'static str;
    /// Version range checked statically before construction (§4.1).
    const SUPPORTED_VERSIONS: VersionRange;
}
