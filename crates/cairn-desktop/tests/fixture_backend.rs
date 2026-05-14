//! Fixture-backed desktop backend tests.

use cairn_desktop::fixture::DesktopFixture;
use cairn_desktop::{
    model::{DesktopReconcileApplyRequest, DesktopReconcilePreviewRequest},
    repository::DesktopRepository,
};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn fixture_loads_alpha_vault_records_and_folders() {
    let fixture = DesktopFixture::load_default().expect("fixture loads");

    assert_eq!(fixture.vault.id, "desktop-alpha");
    assert_eq!(fixture.folders.len(), 2);
    assert_eq!(fixture.records.len(), 3);
    assert!(
        fixture
            .records
            .iter()
            .any(|record| record.id == "rec-alpha-001" && record.links == ["rec-alpha-002"])
    );
}

#[test]
fn fixture_contains_lint_and_reconcile_examples() {
    let fixture = DesktopFixture::load_default().expect("fixture loads");

    assert_eq!(fixture.lint_findings.len(), 1);
    assert_eq!(
        fixture.reconcile_examples.mutable_record_id,
        "rec-alpha-001"
    );
    assert_eq!(fixture.reconcile_examples.immutable_field, "confidence");
}

#[test]
fn repository_derives_graph_edges_from_fixture_links() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let graph = repo.graph();

    assert_eq!(graph.nodes.len(), 3);
    assert_eq!(graph.edges.len(), 2);
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.source == "rec-alpha-001" && edge.target == "rec-alpha-002")
    );
}

#[test]
fn repository_searches_titles_tags_and_body() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let results = repo.search("reconcile");

    assert!(
        results
            .iter()
            .any(|result| result.record_id == "rec-alpha-002")
    );
    assert!(results[0].score >= results.last().expect("result").score);
}

#[test]
fn repository_reconcile_accepts_body_edit() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("body".to_string(), json!("Updated fixture body"));

    let preview = repo.preview_reconcile(DesktopReconcilePreviewRequest {
        target_id: "rec-alpha-001".to_string(),
        expected_version: 2,
        backend_hash: "sha256:fixture-alpha-001".to_string(),
        field_diff,
    });

    assert!(preview.accepted);
    assert_eq!(preview.mutable_diff["body"], json!("Updated fixture body"));
    assert!(preview.rejected_fields.is_empty());
}

#[test]
fn repository_reconcile_apply_persists_body_edit() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("body".to_string(), json!("Persisted fixture body"));

    let result = repo.apply_reconcile(DesktopReconcileApplyRequest {
        preview: DesktopReconcilePreviewRequest {
            target_id: "rec-alpha-001".to_string(),
            expected_version: 2,
            backend_hash: "sha256:fixture-alpha-001".to_string(),
            field_diff,
        },
    });

    assert!(result.accepted);
    let record = repo.record("rec-alpha-001").expect("record persisted");
    assert_eq!(record.body, "Persisted fixture body");
    assert_eq!(record.version, 3);
    assert_ne!(record.backend_hash, "sha256:fixture-alpha-001");
}

#[test]
fn repository_reconcile_rejects_immutable_field() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("confidence".to_string(), json!(0.99));

    let preview = repo.preview_reconcile(DesktopReconcilePreviewRequest {
        target_id: "rec-alpha-001".to_string(),
        expected_version: 2,
        backend_hash: "sha256:fixture-alpha-001".to_string(),
        field_diff,
    });

    assert!(!preview.accepted);
    assert_eq!(preview.rejected_fields[0].field, "confidence");
    assert_eq!(preview.rejected_fields[0].code, "immutable_field_changed");
}

#[test]
fn repository_reconcile_rejects_version_conflict() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("body".to_string(), json!("Updated fixture body"));

    let preview = repo.preview_reconcile(DesktopReconcilePreviewRequest {
        target_id: "rec-alpha-001".to_string(),
        expected_version: 1,
        backend_hash: "sha256:fixture-alpha-001".to_string(),
        field_diff,
    });

    assert!(!preview.accepted);
    assert_eq!(preview.rejected_fields[0].field, "version");
    assert_eq!(preview.rejected_fields[0].code, "version_conflict");
}

#[test]
fn repository_reconcile_rejects_target_hash_mismatch() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("body".to_string(), json!("Updated fixture body"));

    let preview = repo.preview_reconcile(DesktopReconcilePreviewRequest {
        target_id: "rec-alpha-001".to_string(),
        expected_version: 2,
        backend_hash: "sha256:wrong-hash".to_string(),
        field_diff,
    });

    assert!(!preview.accepted);
    assert_eq!(preview.rejected_fields[0].field, "backendHash");
    assert_eq!(preview.rejected_fields[0].code, "target_hash_mismatch");
}
