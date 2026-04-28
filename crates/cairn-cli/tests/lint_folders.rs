//! Integration tests for `cairn lint --fix-folders` (issue #44).

use cairn_cli::verbs::lint::{FixFoldersResult, fix_folders_handler};
use cairn_core::contract::memory_store::MemoryStore;
use cairn_test_fixtures::store::{FixtureStore, sample_record};

#[tokio::test]
async fn rebuilds_index_from_empty_markdown_tree() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    // Bootstrap-style minimal layout: just .cairn/ and raw/.
    std::fs::create_dir_all(vault.path().join(".cairn")).unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();

    let result: FixFoldersResult = fix_folders_handler(&store, vault.path()).await.unwrap();

    assert!(
        !result.written.is_empty(),
        "expected at least one _index.md written"
    );
    let index = vault.path().join("raw/_index.md");
    assert!(index.exists(), "raw/_index.md not written");
    let content = std::fs::read_to_string(&index).unwrap();
    assert!(content.contains("kind: folder_index"));
}

#[tokio::test]
async fn idempotent_second_run_reports_unchanged() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();

    let r1 = fix_folders_handler(&store, vault.path()).await.unwrap();
    assert!(!r1.written.is_empty());

    let r2 = fix_folders_handler(&store, vault.path()).await.unwrap();
    assert!(r2.written.is_empty());
    assert!(r2.unchanged > 0);
}

#[tokio::test]
async fn bad_policy_yaml_does_not_abort_run() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();
    std::fs::write(vault.path().join("raw/_policy.yaml"), "unknown_key: 42\n").unwrap();

    let result = fix_folders_handler(&store, vault.path()).await.unwrap();
    assert_eq!(result.policy_errors.len(), 1);
    assert!(result.policy_errors[0].path.ends_with("raw/_policy.yaml"));
    // The valid record's _index.md is still emitted.
    assert!(vault.path().join("raw/_index.md").exists());
}
