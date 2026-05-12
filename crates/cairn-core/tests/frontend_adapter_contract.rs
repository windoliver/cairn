//! Frontend adapter contract coverage for issue #113.
//!
//! These tests are written against the planned frontend contract surface so
//! they fail red until the production types land.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::conformance::{CaseStatus, Tier, run_conformance_for_plugin};
use cairn_core::contract::frontend_adapter::{
    FrontendAdapter, FrontendAdapterCapabilities, FrontendAdapterError, FrontendEdit,
    FrontendFieldClass, FrontendFieldPolicy, FrontendIdentityContext, FrontendReconcileError,
    FrontendReconcileRequest,
};
use cairn_core::contract::manifest::PluginManifest;
use cairn_core::contract::registry::{PluginName, PluginRegistry};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::BodyHash;

const FRONTEND_MANIFEST: &str = r#"
name = "stub-frontend-runner"
contract = "FrontendAdapter"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
frontmatter = false
sidecar_files = false
live_plugin = false
graph_view = false
"#;

#[derive(Default)]
struct StubFrontend;

#[async_trait::async_trait]
impl FrontendAdapter for StubFrontend {
    fn name(&self) -> &'static str {
        "stub-frontend-runner"
    }

    fn capabilities(&self) -> &FrontendAdapterCapabilities {
        static CAPS: FrontendAdapterCapabilities = FrontendAdapterCapabilities {
            frontmatter: false,
            sidecar_files: false,
            live_plugin: false,
            graph_view: false,
            max_frontmatter_fields: 0,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
}

#[derive(Default)]
struct ConformanceFrontend;

#[async_trait::async_trait]
impl FrontendAdapter for ConformanceFrontend {
    fn name(&self) -> &'static str {
        "stub-frontend-runner"
    }

    fn capabilities(&self) -> &FrontendAdapterCapabilities {
        static CAPS: FrontendAdapterCapabilities = FrontendAdapterCapabilities {
            frontmatter: false,
            sidecar_files: false,
            live_plugin: false,
            graph_view: false,
            max_frontmatter_fields: 0,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    fn reconcile(
        &self,
        ctx: FrontendIdentityContext,
        edit: FrontendEdit,
    ) -> Result<FrontendReconcileRequest, FrontendAdapterError> {
        if edit
            .field_diff
            .keys()
            .any(|field| !FrontendFieldPolicy::is_mutable_from_frontend(field))
        {
            let field = edit
                .field_diff
                .keys()
                .find(|field| !FrontendFieldPolicy::is_mutable_from_frontend(field))
                .expect("immutable field present")
                .clone();
            return Err(FrontendReconcileError::ImmutableFieldChanged { field }.into());
        }
        if edit
            .field_diff
            .get("body")
            .and_then(serde_json::Value::as_str)
            == Some("replay://operation")
        {
            return Err(FrontendReconcileError::ReplayDetected.into());
        }
        if ctx.principal.as_str() != "hmn:known-user" {
            return Err(FrontendReconcileError::PolicyDenied {
                gate: "principal".into(),
                reason: "principal is not registered for this adapter".into(),
            }
            .into());
        }
        if edit.expected_version != 100 {
            return Err(FrontendReconcileError::Conflict {
                current_version: 100,
            }
            .into());
        }
        if edit.target_hash != sample_target_hash() {
            return Err(FrontendReconcileError::PolicyDenied {
                gate: "target_hash".into(),
                reason: "projection hash does not match body hash".into(),
            }
            .into());
        }
        Ok(FrontendReconcileRequest {
            target_id: edit.target_id,
            expected_version: edit.expected_version,
            target_hash: edit.target_hash,
            field_diff: edit.field_diff,
            ctx,
        })
    }
}

#[test]
fn frontend_field_policy_allows_user_content_and_metadata_only() {
    assert!(FrontendFieldPolicy::is_mutable_from_frontend("body"));
    assert!(FrontendFieldPolicy::is_mutable_from_frontend("tags"));
    assert!(FrontendFieldPolicy::is_mutable_from_frontend("last_read_at"));

    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("kind"));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("operation_id"));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("visibility"));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("version"));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("unknown_future_field"));
}

#[test]
fn frontend_field_policy_classifies_unknown_fields_as_version_audit() {
    assert_eq!(
        FrontendFieldPolicy::classify("unknown_future_field"),
        FrontendFieldClass::VersionAudit
    );
}

#[test]
fn frontend_capabilities_default_to_no_projection_features() {
    let caps = FrontendAdapterCapabilities::default();
    assert!(!caps.frontmatter);
    assert!(!caps.sidecar_files);
    assert!(!caps.live_plugin);
    assert!(!caps.graph_view);
    assert_eq!(caps.max_frontmatter_fields, 0);
}

#[test]
fn frontend_reconcile_error_exposes_immutable_field_variant() {
    let err = FrontendReconcileError::ImmutableFieldChanged {
        field: "operation_id".into(),
    };
    let rendered = err.to_string();
    assert!(rendered.contains("operation_id"));
}

#[test]
fn frontend_adapter_runner_reports_expected_tier2_case_ids() {
    let mut reg = PluginRegistry::new();
    let name = PluginName::new("stub-frontend-runner").expect("valid");
    let manifest = PluginManifest::parse_toml(FRONTEND_MANIFEST).expect("manifest parses");
    reg.register_frontend_adapter_with_manifest(name.clone(), manifest, Arc::new(StubFrontend))
        .expect("registers");

    let outcomes = run_conformance_for_plugin(&reg, &name);
    let tier2_ids: Vec<_> = outcomes
        .iter()
        .filter(|outcome| outcome.tier == Tier::Two)
        .map(|outcome| outcome.id)
        .collect();

    assert!(tier2_ids.contains(&"rejects_immutable_field_edits"));
    assert!(tier2_ids.contains(&"rejects_replayed_operation"));
    assert!(tier2_ids.contains(&"rejects_tampered_target_hash"));
    assert!(tier2_ids.contains(&"rejects_unrecognized_principal"));
    assert!(tier2_ids.contains(&"honors_optimistic_version_check"));
}

#[test]
fn frontend_adapter_runner_marks_contract_stub_tier2_cases_ok() {
    let mut reg = PluginRegistry::new();
    let name = PluginName::new("stub-frontend-runner").expect("valid");
    let manifest = PluginManifest::parse_toml(FRONTEND_MANIFEST).expect("manifest parses");
    reg.register_frontend_adapter_with_manifest(
        name.clone(),
        manifest,
        Arc::new(ConformanceFrontend),
    )
    .expect("registers");

    let outcomes = run_conformance_for_plugin(&reg, &name);

    for id in [
        "rejects_immutable_field_edits",
        "rejects_replayed_operation",
        "rejects_tampered_target_hash",
        "rejects_unrecognized_principal",
        "honors_optimistic_version_check",
    ] {
        let outcome = outcomes
            .iter()
            .find(|outcome| outcome.id == id)
            .expect("required tier-2 case exists");
        assert!(
            matches!(outcome.status, CaseStatus::Ok),
            "tier-2 case {id} must pass, got {:?}",
            outcome.status
        );
    }
}

fn sample_target_hash() -> BodyHash {
    BodyHash::compute("trusted body")
}
