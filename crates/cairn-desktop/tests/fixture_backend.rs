//! Fixture-backed desktop backend tests.

use cairn_desktop::fixture::DesktopFixture;
use cairn_desktop::{
    model::{DesktopReconcileApplyRequest, DesktopReconcilePreviewRequest},
    repository::DesktopRepository,
};
use serde_json::json;
use std::{collections::BTreeMap, fs};
use tempfile::tempdir;

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
fn fixture_rejects_record_count_mismatch() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.vault.record_count += 1;
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("mismatched count rejected");

    assert!(err.to_string().contains("recordCount"));
}

#[test]
fn fixture_rejects_record_with_unknown_folder() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.records[0].folder_id = "folder-missing".to_string();
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("unknown folder rejected");

    assert!(err.to_string().contains("folder-missing"));
}

#[test]
fn fixture_rejects_link_to_unknown_record() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.records[0].links = vec!["rec-alpha-missing".to_string()];
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("unknown link rejected");

    assert!(err.to_string().contains("rec-alpha-missing"));
}

#[test]
fn fixture_rejects_duplicate_record_links() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.records[0].links = vec!["rec-alpha-002".to_string(), "rec-alpha-002".to_string()];
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("duplicate link rejected");

    assert!(err.to_string().contains("duplicate link"));
}

#[test]
fn fixture_rejects_duplicate_record_tags() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.records[0].tags = vec!["alpha".to_string(), "alpha".to_string()];
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("duplicate tag rejected");

    assert!(err.to_string().contains("duplicate tag"));
}

#[test]
fn fixture_rejects_lint_finding_for_unknown_record() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.lint_findings[0].record_id = Some("rec-alpha-missing".to_string());
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("unknown lint record rejected");

    assert!(err.to_string().contains("rec-alpha-missing"));
}

#[test]
fn fixture_rejects_duplicate_lint_finding_ids() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.lint_findings.push(fixture.lint_findings[0].clone());
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("duplicate lint rejected");

    assert!(err.to_string().contains("duplicate lint finding id"));
}

#[test]
fn fixture_rejects_duplicate_record_ids() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.records[1].id = fixture.records[0].id.clone();
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("duplicate record rejected");

    assert!(err.to_string().contains("duplicate record id"));
}

#[test]
fn fixture_rejects_duplicate_folder_ids() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.folders[1].id = fixture.folders[0].id.clone();
    fixture
        .records
        .iter_mut()
        .filter(|record| record.folder_id == "folder-ops")
        .for_each(|record| record.folder_id = "folder-core".to_string());
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("duplicate folder rejected");

    assert!(err.to_string().contains("duplicate folder id"));
}

#[test]
fn fixture_rejects_folder_with_unknown_parent() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.folders[1].parent_id = Some("folder-missing".to_string());
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("unknown folder parent rejected");

    assert!(err.to_string().contains("folder-missing"));
}

#[test]
fn fixture_rejects_folder_with_self_parent() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.folders[1].parent_id = Some(fixture.folders[1].id.clone());
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("self parent rejected");

    assert!(err.to_string().contains("parentId itself"));
}

#[test]
fn fixture_rejects_folder_parent_cycle() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.folders[0].parent_id = Some(fixture.folders[1].id.clone());
    fixture.folders[1].parent_id = Some(fixture.folders[0].id.clone());
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("folder cycle rejected");

    assert!(err.to_string().contains("parent cycle"));
}

#[test]
fn fixture_rejects_reconcile_example_for_unknown_record() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.reconcile_examples.mutable_record_id = "rec-alpha-missing".to_string();
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("unknown reconcile record rejected");

    assert!(err.to_string().contains("rec-alpha-missing"));
}

#[test]
fn fixture_rejects_mutable_field_as_immutable_example() {
    let mut fixture = DesktopFixture::load_default().expect("fixture loads");
    fixture.reconcile_examples.immutable_field = "body".to_string();
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vault.json");
    fs::write(&path, serde_json::to_vec(&fixture).expect("fixture json")).expect("write fixture");

    let err = DesktopFixture::load_from_path(&path).expect_err("mutable example rejected");

    assert!(err.to_string().contains("immutableField"));
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
    let padded_results = repo.search("  reconcile  ");

    assert!(
        results
            .iter()
            .any(|result| result.record_id == "rec-alpha-002")
    );
    assert_eq!(
        results
            .iter()
            .map(|result| &result.record_id)
            .collect::<Vec<_>>(),
        padded_results
            .iter()
            .map(|result| &result.record_id)
            .collect::<Vec<_>>()
    );
    assert!(results[0].score >= results.last().expect("result").score);
}

#[test]
fn repository_search_returns_empty_results_for_blank_query() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));

    assert!(repo.search("").is_empty());
    assert!(repo.search("   ").is_empty());
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
fn repository_reconcile_rejects_invalid_mutable_field_shape() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("tags".to_string(), json!("not-an-array"));

    let preview = repo.preview_reconcile(DesktopReconcilePreviewRequest {
        target_id: "rec-alpha-001".to_string(),
        expected_version: 2,
        backend_hash: "sha256:fixture-alpha-001".to_string(),
        field_diff,
    });

    assert!(!preview.accepted);
    assert!(preview.mutable_diff.is_empty());
    assert_eq!(preview.rejected_fields[0].field, "tags");
    assert_eq!(preview.rejected_fields[0].code, "invalid_field_shape");
}

#[test]
fn repository_reconcile_rejects_duplicate_tags() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("tags".to_string(), json!(["alpha", "alpha"]));

    let preview = repo.preview_reconcile(DesktopReconcilePreviewRequest {
        target_id: "rec-alpha-001".to_string(),
        expected_version: 2,
        backend_hash: "sha256:fixture-alpha-001".to_string(),
        field_diff,
    });

    assert!(!preview.accepted);
    assert!(preview.mutable_diff.is_empty());
    assert_eq!(preview.rejected_fields[0].field, "tags");
    assert_eq!(preview.rejected_fields[0].code, "duplicate_tag");
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
fn repository_reconcile_apply_leaves_noop_edit_version_unchanged() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let original = repo.record("rec-alpha-001").expect("record");
    let mut field_diff = BTreeMap::new();
    field_diff.insert("body".to_string(), json!(original.body));

    let result = repo.apply_reconcile(DesktopReconcileApplyRequest {
        preview: DesktopReconcilePreviewRequest {
            target_id: "rec-alpha-001".to_string(),
            expected_version: original.version,
            backend_hash: original.backend_hash.clone(),
            field_diff,
        },
    });

    assert!(result.accepted);
    let record = repo.record("rec-alpha-001").expect("record persisted");
    assert_eq!(record.version, original.version);
    assert_eq!(record.backend_hash, original.backend_hash);
}

#[test]
fn repository_reconcile_apply_persists_tags_and_wikilinks() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("tags".to_string(), json!(["alpha", "reviewed"]));
    field_diff.insert(
        "wikilinks".to_string(),
        json!(["rec-alpha-002", "rec-alpha-003"]),
    );

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
    assert_eq!(record.tags, ["alpha", "reviewed"]);
    assert_eq!(record.links, ["rec-alpha-002", "rec-alpha-003"]);
    assert_eq!(record.version, 3);
}

#[test]
fn repository_reconcile_rejects_unknown_wikilink_target() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("wikilinks".to_string(), json!(["rec-alpha-missing"]));

    let preview = repo.preview_reconcile(DesktopReconcilePreviewRequest {
        target_id: "rec-alpha-001".to_string(),
        expected_version: 2,
        backend_hash: "sha256:fixture-alpha-001".to_string(),
        field_diff,
    });

    assert!(!preview.accepted);
    assert!(preview.mutable_diff.is_empty());
    assert_eq!(preview.rejected_fields[0].field, "wikilinks");
    assert_eq!(preview.rejected_fields[0].code, "unknown_wikilink_target");
}

#[test]
fn repository_reconcile_rejects_duplicate_wikilink_targets() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert(
        "wikilinks".to_string(),
        json!(["rec-alpha-002", "rec-alpha-002"]),
    );

    let preview = repo.preview_reconcile(DesktopReconcilePreviewRequest {
        target_id: "rec-alpha-001".to_string(),
        expected_version: 2,
        backend_hash: "sha256:fixture-alpha-001".to_string(),
        field_diff,
    });

    assert!(!preview.accepted);
    assert!(preview.mutable_diff.is_empty());
    assert_eq!(preview.rejected_fields[0].field, "wikilinks");
    assert_eq!(preview.rejected_fields[0].code, "duplicate_wikilink_target");
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
fn repository_reconcile_rejects_mixed_diff_atomically() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("body".to_string(), json!("Updated fixture body"));
    field_diff.insert("confidence".to_string(), json!(0.99));

    let preview = repo.preview_reconcile(DesktopReconcilePreviewRequest {
        target_id: "rec-alpha-001".to_string(),
        expected_version: 2,
        backend_hash: "sha256:fixture-alpha-001".to_string(),
        field_diff,
    });

    assert!(!preview.accepted);
    assert!(preview.mutable_diff.is_empty());
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
