//! Frontend adapter contract coverage for issue #113.
//!
//! These tests are written against the planned frontend contract surface so
//! they fail red until the production types land.
#![allow(missing_docs)]

use cairn_core::contract::frontend_adapter::{
    FrontendAdapterCapabilities, FrontendFieldClass, FrontendFieldPolicy,
    FrontendReconcileError,
};

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
